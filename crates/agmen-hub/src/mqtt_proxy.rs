//! A bounded HTTP upgrade followed by an opaque MQTT WebSocket byte stream.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use hyper::client::conn::http1;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use url::{Position, Url};

use crate::http::ApiError;

pub(crate) const MAX_PROXIED_CONNECTIONS: usize = 1024;
pub(crate) const UPSTREAM_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const HALF_CLOSE_GRACE: Duration = Duration::from_secs(5);
const MAX_HEAD_BYTES: usize = 8 * 1024;
const REQUEST_HEADERS: &[&str] = &[
    "upgrade",
    "connection",
    "sec-websocket-key",
    "sec-websocket-version",
    "sec-websocket-protocol",
    "sec-websocket-extensions",
];
const RESPONSE_HEADERS: &[&str] = &[
    "upgrade",
    "connection",
    "sec-websocket-accept",
    "sec-websocket-protocol",
    "sec-websocket-extensions",
];

pub(crate) fn router<S: Clone + Send + Sync + 'static>(
    upstream: Url,
    max_connections: usize,
) -> Router<S> {
    let connections = Arc::new(Semaphore::new(max_connections));
    Router::new().route(
        "/mqtt",
        get(move |request: Request| proxy(request, upstream.clone(), connections.clone())),
    )
}

async fn proxy(
    mut request: Request,
    upstream: Url,
    connections: Arc<Semaphore>,
) -> Result<Response, ApiError> {
    if !request
        .headers()
        .get_all(header::UPGRADE)
        .iter()
        .any(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"))
    {
        return Err(ApiError::new(
            StatusCode::UPGRADE_REQUIRED,
            "upgrade_required",
            "a WebSocket upgrade is required",
        ));
    }
    let permit = connections.try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "too_many_connections",
            "the MQTT proxy connection limit was reached",
        )
    })?;
    let path = &upstream[Position::BeforePath..Position::AfterQuery];
    let host = upstream.host_str().ok_or_else(upstream_unavailable)?;
    let port = upstream
        .port_or_known_default()
        .ok_or_else(upstream_unavailable)?;
    let authority = format!("{host}:{port}");
    // Count the whole incoming head as well as the serialized upstream head,
    // including repeated fields. Private headers never enter the new request.
    let incoming_bytes = request.method().as_str().len()
        + request.uri().to_string().len()
        + 12
        + header_bytes(request.headers());
    let mut forwarded = Request::builder()
        .uri(path)
        .header(header::HOST, authority)
        .body(Body::empty())
        .map_err(|_| upstream_unavailable())?;
    for name in REQUEST_HEADERS {
        for value in request.headers().get_all(*name) {
            forwarded.headers_mut().append(*name, value.clone());
        }
    }
    let outgoing_bytes = path.len() + 15 + header_bytes(forwarded.headers());
    if incoming_bytes > MAX_HEAD_BYTES || outgoing_bytes > MAX_HEAD_BYTES {
        return Err(ApiError::new(
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "headers_too_large",
            "the MQTT upgrade head exceeds 8 KiB",
        ));
    }
    let (headers, upstream) = timeout(UPSTREAM_HANDSHAKE_TIMEOUT, async {
        let stream = TcpStream::connect((host.trim_start_matches('[').trim_end_matches(']'), port))
            .await
            .map_err(|_| upstream_unavailable())?;
        let (mut sender, connection) = http1::Builder::new()
            .max_buf_size(MAX_HEAD_BYTES)
            .max_header_size(MAX_HEAD_BYTES)
            .handshake(TokioIo::new(stream))
            .await
            .map_err(|_| upstream_unavailable())?;
        let mut connection = std::pin::pin!(connection.with_upgrades());
        let mut send = std::pin::pin!(sender.send_request(forwarded));
        let mut response = tokio::select! {
            response = &mut send => {
                let response = response.map_err(|_| upstream_unavailable())?;
                if response.status() == StatusCode::SWITCHING_PROTOCOLS {
                    connection.await.map_err(|_| upstream_unavailable())?;
                }
                response
            },
            result = &mut connection => {
                // A successful upgrade completes the driver before the
                // response future is necessarily polled again.
                result.map_err(|_| upstream_unavailable())?;
                send.await.map_err(|_| upstream_unavailable())?
            },
        };
        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
            return Err(upstream_unavailable());
        }
        let upgraded = hyper::upgrade::on(&mut response)
            .await
            .map_err(|_| upstream_unavailable())?;
        Ok((response.into_parts().0.headers, upgraded))
    })
    .await
    .map_err(|_| upstream_unavailable())??;
    let downstream = hyper::upgrade::on(&mut request);
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    for name in RESPONSE_HEADERS {
        for value in headers.get_all(*name) {
            response.headers_mut().append(*name, value.clone());
        }
    }
    // shortcut: proxied connections are not drained on shutdown; revisit if shutdown must close them cleanly
    let task = tokio::spawn(async move {
        let _permit = permit;
        if let Ok(Ok(downstream)) = timeout(UPSTREAM_HANDSHAKE_TIMEOUT, downstream).await {
            copy(downstream, upstream).await;
        }
    });
    drop(task);
    Ok(response)
}

fn header_bytes(headers: &HeaderMap) -> usize {
    headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.as_bytes().len() + 4)
        .sum::<usize>()
        + 2
}

fn upstream_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "upstream_unavailable",
        "the MQTT upstream is unavailable",
    )
}

async fn copy(downstream: Upgraded, upstream: Upgraded) {
    let (mut down_read, mut down_write) = tokio::io::split(TokioIo::new(downstream));
    let (mut up_read, mut up_write) = tokio::io::split(TokioIo::new(upstream));
    // A WebSocket has no half-close: once either side has finished, the other
    // direction gets a short grace to flush and then the slot is released, so a
    // silent peer cannot hold a permit after its counterpart hung up.
    // Disconnects end the opaque stream; never log its headers or payload.
    let mut to_upstream = std::pin::pin!(async {
        let _copied = tokio::io::copy(&mut down_read, &mut up_write).await;
        let _closed = up_write.shutdown().await;
    });
    let mut to_downstream = std::pin::pin!(async {
        let _copied = tokio::io::copy(&mut up_read, &mut down_write).await;
        let _closed = down_write.shutdown().await;
    });
    tokio::select! {
        () = &mut to_upstream => {
            let _drained = timeout(HALF_CLOSE_GRACE, to_downstream).await;
        },
        () = &mut to_downstream => {
            let _drained = timeout(HALF_CLOSE_GRACE, to_upstream).await;
        },
    }
}

#[cfg(test)]
#[path = "mqtt_proxy_tests.rs"]
mod tests;
