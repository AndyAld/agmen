//! Hub response limits against a local HTTP server, including stalled tails.

// Include helpers in clippy's test-only unwrap and expect policy.
#![cfg(test)]

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use agmen_client::{HubClient, token_grant};
use agmen_proto::Stream;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::StatusCode;
use futures_core::Stream as BodyStream;
use url::Url;

/// Sends past the limit without ever sending EOF, with no Content-Length.
struct OversizedBody {
    remaining: usize,
}

impl BodyStream for OversizedBody {
    type Item = Result<Bytes, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.remaining == 0 {
            return Poll::Pending;
        }
        let len = self.remaining.min(64 * 1024);
        self.remaining -= len;
        Poll::Ready(Some(Ok(Bytes::from(vec![b' '; len]))))
    }
}

struct Server {
    base: Url,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server(status: StatusCode, body: Body) -> Server {
    let body = std::sync::Arc::new(std::sync::Mutex::new(Some(body)));
    let app = Router::new().fallback(move || {
        let body = body.lock().unwrap().take().unwrap();
        async move { (status, [("content-type", "application/json")], body) }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { base, task }
}

fn hub(server: &Server) -> HubClient {
    HubClient::new(
        server.base.clone(),
        "project1".parse().unwrap(),
        "idmememe".parse().unwrap(),
        "test-token",
    )
    .unwrap()
}

#[derive(Clone, Copy)]
enum Endpoint {
    Page,
    BrokerToken,
    TokenGrant,
}

async fn call(endpoint: Endpoint, server: &Server) -> Result<(), String> {
    match endpoint {
        Endpoint::Page => hub(server)
            .page(&Stream::Feed, 0)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Endpoint::BrokerToken => hub(server)
            .broker_token()
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Endpoint::TokenGrant => token_grant(&reqwest::Client::new(), &server.base, &[])
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

async fn refuses_oversized(endpoint: Endpoint, status: StatusCode) {
    let limit = match endpoint {
        Endpoint::Page => 500 * 80 * 1024 + 1024 * 1024,
        Endpoint::BrokerToken | Endpoint::TokenGrant => 16 * 1024 * 1024,
    };
    let server = server(
        status,
        Body::from_stream(OversizedBody {
            remaining: limit + 1,
        }),
    )
    .await;
    let result = tokio::time::timeout(Duration::from_secs(3), call(endpoint, &server))
        .await
        .expect("the client must reject the body before its stalled tail or EOF");
    let error = result.unwrap_err();
    assert!(
        error.contains(&limit.to_string()) && error.contains("limit"),
        "{error}"
    );
}

#[tokio::test]
async fn page_refuses_oversized_success_before_eof() {
    refuses_oversized(Endpoint::Page, StatusCode::OK).await;
}

#[tokio::test]
async fn page_refuses_oversized_error_before_eof() {
    refuses_oversized(Endpoint::Page, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
async fn broker_token_refuses_oversized_success_before_eof() {
    refuses_oversized(Endpoint::BrokerToken, StatusCode::OK).await;
}

#[tokio::test]
async fn broker_token_refuses_oversized_error_before_eof() {
    refuses_oversized(Endpoint::BrokerToken, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
async fn token_grant_refuses_oversized_success_before_eof() {
    refuses_oversized(Endpoint::TokenGrant, StatusCode::OK).await;
}

#[tokio::test]
async fn token_grant_refuses_oversized_error_before_eof() {
    refuses_oversized(Endpoint::TokenGrant, StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn normal_page_parses() {
    let server = server(
        StatusCode::OK,
        Body::from(r#"{"messages":[],"next_after":null,"head":0}"#),
    )
    .await;
    let page = hub(&server).page(&Stream::Feed, 0).await.unwrap();
    assert!(page.messages.is_empty());
    assert_eq!(page.next_after, None);
    assert_eq!(page.head, 0);
}

#[tokio::test]
async fn page_just_under_page_cap_parses() {
    let mut body = br#"{"messages":[],"next_after":null,"head":0}"#.to_vec();
    body.resize(500 * 80 * 1024 + 1024 * 1024 - 1, b' ');
    let server = server(StatusCode::OK, Body::from(body)).await;
    let page = hub(&server).page(&Stream::Feed, 0).await.unwrap();
    assert!(page.messages.is_empty());
    assert_eq!(page.next_after, None);
    assert_eq!(page.head, 0);
}

#[tokio::test]
async fn broker_token_at_limit_parses() {
    let mut body = br#"{"token":"broker-token"}"#.to_vec();
    body.resize(16 * 1024 * 1024, b' ');
    let server = server(StatusCode::OK, Body::from(body)).await;
    assert_eq!(hub(&server).broker_token().await.unwrap(), "broker-token");
}

#[tokio::test]
async fn normal_token_grant_parses() {
    let server = server(
        StatusCode::OK,
        Body::from(r#"{"access_token":"access","refresh_token":"refresh","expires_in":3600}"#),
    )
    .await;
    let tokens = token_grant(&reqwest::Client::new(), &server.base, &[])
        .await
        .unwrap();
    assert_eq!(tokens.access_token(), "access");
}

#[tokio::test]
async fn normal_errors_keep_status_and_code() {
    let server = server(
        StatusCode::FORBIDDEN,
        Body::from(r#"{"error":"identity_revoked"}"#),
    )
    .await;
    assert_eq!(
        call(Endpoint::BrokerToken, &server).await.unwrap_err(),
        "the hub answered 403 identity_revoked"
    );
    let server = self::server(
        StatusCode::BAD_REQUEST,
        Body::from(r#"{"error":"invalid_grant","error_description":"expired"}"#),
    )
    .await;
    let error = token_grant(&reqwest::Client::new(), &server.base, &[])
        .await
        .unwrap_err();
    assert!(
        matches!(error, agmen_client::LoginError::Refused { status: 400, code, description } if code == "invalid_grant" && description == "expired")
    );
}

#[tokio::test]
async fn malformed_or_empty_error_bodies_keep_only_status() {
    for body in ["", "not json"] {
        let server = server(StatusCode::FORBIDDEN, Body::from(body)).await;
        assert!(matches!(
            hub(&server).broker_token().await.unwrap_err(),
            agmen_client::ClientError::Hub { status: 403, code } if code.is_empty()
        ));
        let server = self::server(StatusCode::BAD_REQUEST, Body::from(body)).await;
        let error = token_grant(&reqwest::Client::new(), &server.base, &[])
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            agmen_client::LoginError::Refused { status: 400, code, description }
                if code.is_empty() && description.is_empty()
        ));
    }
}

#[tokio::test]
async fn malformed_or_empty_success_bodies_return_parse_errors() {
    for body in ["", "not json"] {
        let server = server(StatusCode::OK, Body::from(body)).await;
        assert!(matches!(
            hub(&server).broker_token().await.unwrap_err(),
            agmen_client::ClientError::BadAnswer(_)
        ));
        let server = self::server(StatusCode::OK, Body::from(body)).await;
        let error = token_grant(&reqwest::Client::new(), &server.base, &[])
            .await
            .unwrap_err();
        assert!(matches!(error, agmen_client::LoginError::BadAnswer(_)));
    }
}

#[tokio::test]
async fn declared_oversized_body_is_refused() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.read_exact(&mut [0; 1]).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16777217\r\n\r\n")
            .await
            .unwrap();
        // No body bytes are sent before the client's deadline.
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let server = Server { base, task };
    let result = tokio::time::timeout(Duration::from_secs(3), hub(&server).broker_token())
        .await
        .expect("reject the declared length without waiting for body bytes");
    assert!(matches!(
        result.unwrap_err(),
        agmen_client::ClientError::HubBodyTooLarge { limit: 16_777_216 }
    ));
}
