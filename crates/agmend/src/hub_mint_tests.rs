//! Shared fake mint responses for the listener and agent rollback tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::response::{IntoResponse as _, Response};
use serde_json::{Value, json};
use tokio_stream::StreamExt as _;

use crate::hub::Hub;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Failure {
    LateHeaders,
    LateBody,
    ResetBody,
    OversizedBody,
    OversizedStream,
    /// A 409 whose error body is over the cap. A refusal made nothing.
    OversizedRefusal,
}

impl Failure {
    pub(crate) fn configure(self, hub: &Hub) {
        if matches!(self, Self::LateHeaders | Self::LateBody) {
            *hub.mint_timeout.lock().unwrap() = Some(Duration::from_millis(200));
        }
    }

    pub(crate) fn assert_error(self, error: &agmen_local::ApiError) {
        assert_eq!(error.code, agmen_local::ErrorCode::Hub);
        let expected = match self {
            Self::LateHeaders | Self::LateBody => "operation timed out",
            Self::ResetBody => "body",
            Self::OversizedBody | Self::OversizedStream => "16777216 byte limit",
            Self::OversizedRefusal => "409",
        };
        assert!(error.message.contains(expected), "{error:?}");
    }

    async fn response(self, minted: Value) -> Response {
        match self {
            Self::LateHeaders => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                axum::Json(minted).into_response()
            }
            Self::LateBody | Self::ResetBody => {
                // The first chunk reaches the client before the delayed second
                // chunk, so this fails while reading a 2xx body.
                let chunks = [
                    Ok(Bytes::from_static(
                        b"{\"token\":\"fake-private-mint-token\",\"id\":\"decoy001\",",
                    )),
                    Err(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
                ];
                let delay = if matches!(self, Self::LateBody) {
                    Duration::from_secs(1)
                } else {
                    Duration::from_millis(20)
                };
                Body::from_stream(tokio_stream::iter(chunks).throttle(delay)).into_response()
            }
            Self::OversizedRefusal => (
                axum::http::StatusCode::CONFLICT,
                vec![b'x'; agmen_client::MAX_HUB_RESPONSE_BYTES + 1],
            )
                .into_response(),
            Self::OversizedBody | Self::OversizedStream => {
                let bytes = Bytes::from(vec![b'x'; agmen_client::MAX_HUB_RESPONSE_BYTES + 1]);
                if matches!(self, Self::OversizedBody) {
                    bytes.into_response()
                } else {
                    Body::from_stream(tokio_stream::once(Ok::<_, std::io::Error>(bytes)))
                        .into_response()
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Calls {
    pub(crate) mints: AtomicUsize,
    pub(crate) roster: AtomicUsize,
    pub(crate) revoked: Mutex<Vec<String>>,
    live: AtomicBool,
}

pub(crate) fn mint_router(
    failure: Failure,
    id: &'static str,
    roster: Value,
) -> (axum::Router, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    let (mints, reads, revokes) = (calls.clone(), calls.clone(), calls.clone());
    let app = axum::Router::new()
        .route("/v1/me", axum::routing::get(|| async {
            axum::Json(json!({"broker_url":"mqtt://127.0.0.1:9"}))
        }))
        .route("/v1/identities", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let first = mints.mints.fetch_add(1, Ordering::SeqCst) == 0;
            let taken = mints.live.swap(true, Ordering::SeqCst);
            async move {
                if taken {
                    return (axum::http::StatusCode::CONFLICT, axum::Json(json!({"error":"handle_taken"}))).into_response();
                }
                let minted = json!({"id":id,"handle":body["handle"],"tenant":"tenant01","project":body["project"],"token":"fake-private-mint-token","verbs":["FYI"]});
                if first {
                    failure.response(minted).await
                } else {
                    axum::Json(minted).into_response()
                }
            }
        }))
        .route("/v1/projects/{project}/state", axum::routing::get(move |axum::extract::Path(project): axum::extract::Path<String>| {
            assert_eq!(project, "project1");
            reads.roster.fetch_add(1, Ordering::SeqCst);
            let roster = roster.clone();
            async move { axum::Json(json!({"value":roster})) }
        }))
        .route("/v1/identities/{id}", axum::routing::delete(move |axum::extract::Path(revoked): axum::extract::Path<String>| {
            if revoked == id {
                revokes.live.store(false, Ordering::SeqCst);
            }
            revokes.revoked.lock().unwrap().push(revoked);
            async { axum::http::StatusCode::NO_CONTENT }
        }));
    (app, calls)
}

// The mint's client cannot establish a connection. The fixture checks both
// that its server received nothing and that no rollback warning was emitted.
pub(crate) async fn connect_failure_hub(hub: &Hub) -> Arc<Hub> {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", socket.local_addr().unwrap());
    drop(socket);
    let http = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(proxy).unwrap())
        .build()
        .unwrap();
    Arc::new(hub.with_http(http))
}
