//! A loopback stand-in for Paddle. Later hosted tasks point the hub at it.
//! This task never calls it. Checkout responses stay Task 5's.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use aws_lc_rs::hmac::{self, HMAC_SHA256};
use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use url::Url;

const LOG_CAP: usize = 256;

type Log = Arc<Mutex<Vec<(axum::http::Method, String, Value)>>>;

/// A fake Paddle Billing API on an ephemeral loopback port.
///
/// Dropping it stops the server. `Debug` omits the API key and webhook secret.
pub struct FakePaddle {
    base: Url,
    api_key: String,
    webhook_secret: String,
    requests: Log,
    server: JoinHandle<()>,
}

impl fmt::Debug for FakePaddle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakePaddle")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl Drop for FakePaddle {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakePaddle {
    /// Starts a server that records each call and answers `{"data": {}}`.
    ///
    /// # Errors
    ///
    /// When the random source fails, or the loopback listener cannot bind.
    pub async fn start() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let api_key = random_secret()?;
        let webhook_secret = random_secret()?;
        let requests = Log::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        let app = axum::Router::new()
            .fallback(record)
            .with_state(Arc::clone(&requests));
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::error!(%error, "fake Paddle server failed");
            }
        });
        Ok(Self {
            base,
            api_key,
            webhook_secret,
            requests,
            server,
        })
    }

    /// The server's base URL, including the trailing slash.
    #[must_use]
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// The API key generated for this server.
    #[must_use]
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// The webhook secret generated for this server.
    #[must_use]
    pub fn webhook_secret(&self) -> &str {
        &self.webhook_secret
    }

    /// Recorded calls, oldest first, capped at 256. The path has no query.
    /// A body that is not JSON is `Null`.
    #[must_use]
    pub fn requests(&self) -> Vec<(axum::http::Method, String, Value)> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A `Paddle-Signature` value for `body` at unix seconds `ts`.
    ///
    /// `ts=<ts>;h1=<hex>` where `h1` is HMAC-SHA256 of `{ts}:{body}` under
    /// this server's webhook secret. Hex is lowercase.
    #[must_use]
    pub fn sign(&self, body: &[u8], ts: i64) -> String {
        let key = hmac::Key::new(HMAC_SHA256, self.webhook_secret.as_bytes());
        let mut message = format!("{ts}:").into_bytes();
        message.extend_from_slice(body);
        let tag = hmac::sign(&key, &message);
        format!("ts={ts};h1={}", hex(tag.as_ref()))
    }
}

async fn record(State(log): State<Log>, request: axum::extract::Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mut log = log.lock().unwrap_or_else(PoisonError::into_inner);
    if log.len() < LOG_CAP {
        log.push((method, path, value));
    }
    Json(json!({"data": {}})).into_response()
}

fn random_secret() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    out
}
