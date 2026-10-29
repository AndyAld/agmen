//! A Paddle stand-in for CLI integration tests.
//!
//! It answers `POST /transactions` and `POST /customers/{id}/portal-sessions`
//! and nothing else. The API stays on http. Buyer links use the same host
//! and port over https, which is what a later hub host check accepts for an
//! IP (its registrable domain is the IP itself).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tokio::task::JoinHandle;
use url::Url;

/// The most bytes read from one Paddle request body.
const PADDLE_BODY_CAP: usize = 64 * 1024;

struct App {
    /// `https://127.0.0.1:{port}/`, used for checkout and portal links.
    links: String,
    seq: AtomicU64,
}

/// Loopback Paddle. Abort its server on drop so the listener does not outlive
/// the test: a dropped [`JoinHandle`] detaches the task.
pub struct FakePaddle {
    base: Url,
    api_key: String,
    webhook_secret: String,
    server: JoinHandle<()>,
}

impl Drop for FakePaddle {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakePaddle {
    /// Binds `127.0.0.1:0` and serves until dropped.
    pub async fn start() -> Result<Self, String> {
        // `random` instead of `getrandom`: agmen-e2e compiles this file and
        // does not depend on getrandom. The values are still new each run.
        let api_key = super::random(b"0123456789abcdef", 64);
        let webhook_secret = super::random(b"0123456789abcdef", 64);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|err| err.to_string())?;
        let addr = listener.local_addr().map_err(|err| err.to_string())?;
        let base = Url::parse(&format!("http://{addr}/")).map_err(|err| err.to_string())?;
        let mut links = base.clone();
        links
            .set_scheme("https")
            .map_err(|()| "the fake Paddle link base cannot use https".to_owned())?;
        let app = Arc::new(App {
            links: links.to_string(),
            seq: AtomicU64::new(1),
        });
        let router = axum::Router::new().fallback(handle).with_state(app);
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Ok(Self {
            base,
            api_key,
            webhook_secret,
            server,
        })
    }

    /// The http base the hub's Paddle client calls.
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// A key generated for this process. Not a real Paddle key.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// A webhook secret generated for this process. Not a real Paddle secret.
    pub fn webhook_secret(&self) -> &str {
        &self.webhook_secret
    }
}

async fn handle(State(app): State<Arc<App>>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    if axum::body::to_bytes(request.into_body(), PADDLE_BODY_CAP)
        .await
        .is_err()
    {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    if method == axum::http::Method::POST && path == "/transactions" {
        return checkout(&app);
    }
    if method == axum::http::Method::POST
        && let Some(customer) = portal_customer(&path)
    {
        return portal(&app, customer);
    }
    StatusCode::NOT_FOUND.into_response()
}

fn checkout(app: &App) -> Response {
    let n = app.seq.fetch_add(1, Ordering::Relaxed);
    let url = format!("{}checkout/txn_{n}", app.links);
    (
        StatusCode::CREATED,
        Json(json!({
            "data": {
                "id": format!("txn_{n}"),
                "status": "draft",
                "checkout": { "url": url },
            }
        })),
    )
        .into_response()
}

fn portal(app: &App, customer: &str) -> Response {
    let n = app.seq.fetch_add(1, Ordering::Relaxed);
    let overview = format!("{}portal/{customer}", app.links);
    (
        StatusCode::CREATED,
        Json(json!({
            "data": {
                "id": format!("cpls_{n}"),
                "customer_id": customer,
                "urls": {
                    "general": { "overview": overview },
                },
            }
        })),
    )
        .into_response()
}

fn portal_customer(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/customers/")?;
    let customer = rest.strip_suffix("/portal-sessions")?;
    (!customer.is_empty() && !customer.contains('/')).then_some(customer)
}
