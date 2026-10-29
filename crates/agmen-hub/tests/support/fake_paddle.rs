//! A loopback stand-in for Paddle. Hosted tests point the hub and the Paddle
//! client at it.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use aws_lc_rs::hmac::{self, HMAC_SHA256};
use axum::Json;
use axum::extract::State;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use url::Url;

const LOG_CAP: usize = 256;
const DEFAULT_MAX_PAGE_SIZE: usize = 200;
const DEFAULT_PER_PAGE: usize = 50;

struct FakeState {
    base: Url,
    next_id: u64,
    page_size: usize,
    endless_pages: bool,
    fail_next: Option<(u16, Value)>,
    requests: Vec<(Method, String, Value)>,
    authorizations: Vec<Option<String>>,
    paddle_versions: Vec<Option<String>>,
    products: Vec<Value>,
    prices: Vec<Value>,
}

impl FakeState {
    fn new(base: Url) -> Self {
        Self {
            base,
            next_id: 0,
            page_size: DEFAULT_MAX_PAGE_SIZE,
            endless_pages: false,
            fail_next: None,
            requests: Vec::new(),
            authorizations: Vec::new(),
            paddle_versions: Vec::new(),
            products: Vec::new(),
            prices: Vec::new(),
        }
    }

    fn fresh_number(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

type SharedState = Arc<Mutex<FakeState>>;

/// A fake Paddle Billing API on an ephemeral loopback port.
///
/// Dropping it stops the server. `Debug` omits the API key and webhook secret.
pub struct FakePaddle {
    base: Url,
    api_key: String,
    webhook_secret: String,
    state: SharedState,
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
    /// Starts a server that records each call and answers Paddle Billing routes
    /// (falling back to `{"data": {}}` for unknown routes).
    ///
    /// # Errors
    ///
    /// When the random source fails, or the loopback listener cannot bind.
    pub async fn start() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let api_key = random_secret()?;
        let webhook_secret = random_secret()?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        let state = Arc::new(Mutex::new(FakeState::new(base.clone())));
        let app = axum::Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::error!(%error, "fake Paddle server failed");
            }
        });
        Ok(Self {
            base,
            api_key,
            webhook_secret,
            state,
            server,
        })
    }

    /// The server's base URL, including the trailing slash.
    #[must_use]
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// The https origin of checkout and portal links this fake returns.
    ///
    /// The API itself stays on [`Self::base`]'s http listener. Buyer links use
    /// the same host over https, which is an allowed host for that API.
    #[must_use]
    pub fn link_base(&self) -> String {
        https_base(&self.base)
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
    pub fn requests(&self) -> Vec<(Method, String, Value)> {
        self.lock_state().requests.clone()
    }

    /// The `Authorization` header of each logged call, in order.
    #[must_use]
    pub fn authorizations(&self) -> Vec<Option<String>> {
        self.lock_state().authorizations.clone()
    }

    /// The `Paddle-Version` header of each logged call, in order.
    #[must_use]
    pub fn paddle_versions(&self) -> Vec<Option<String>> {
        self.lock_state().paddle_versions.clone()
    }

    /// Answers `status` and `body` on the next call once.
    ///
    /// When `body` is `Value::Null`, the response carries an empty body.
    pub fn fail_next(&self, status: u16, body: Value) {
        self.lock_state().fail_next = Some((status, body));
    }

    /// Caps the number of entities returned per page on list routes.
    pub fn set_page_size(&self, n: usize) {
        self.lock_state().page_size = n.max(1);
    }

    /// Makes list routes report `has_more: true` on every page, for testing
    /// pagination bounds.
    pub fn set_endless_pages(&self, endless: bool) {
        self.lock_state().endless_pages = endless;
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

    fn lock_state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

async fn handle(State(state): State<SharedState>, request: axum::extract::Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().map(str::to_owned);
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let paddle_version = request
        .headers()
        .get("paddle-version")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .unwrap_or_default();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    if state.requests.len() < LOG_CAP {
        state
            .requests
            .push((method.clone(), path.clone(), body.clone()));
        state.authorizations.push(auth);
        state.paddle_versions.push(paddle_version);
    }
    if let Some((status, fail_body)) = state.fail_next.take() {
        let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        return if fail_body.is_null() {
            (code, String::new()).into_response()
        } else {
            (code, Json(fail_body)).into_response()
        };
    }
    route_request(&mut state, &method, &path, query.as_deref(), &body)
}

fn route_request(
    state: &mut FakeState,
    method: &Method,
    path: &str,
    query: Option<&str>,
    body: &Value,
) -> Response {
    match (method.as_str(), path) {
        ("POST", "/transactions") => create_transaction(state, body),
        ("POST", "/products") => create_product(state, body),
        ("GET", "/products") => list_entities(state, "products", query),
        ("POST", "/prices") => create_price(state, body),
        ("GET", "/prices") => list_entities(state, "prices", query),
        ("PATCH", _) => {
            if let Some(sub_id) = single_segment_after(path, "/subscriptions/") {
                update_subscription(state, sub_id, body)
            } else {
                Json(json!({"data": {}})).into_response()
            }
        }
        ("POST", _) => {
            if let Some(customer_id) = customer_portal_id(path) {
                create_portal_session(state, customer_id)
            } else {
                Json(json!({"data": {}})).into_response()
            }
        }
        _ => Json(json!({"data": {}})).into_response(),
    }
}

fn single_segment_after<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(prefix)?;
    (!rest.is_empty() && !rest.contains('/')).then_some(rest)
}

fn customer_portal_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/customers/")?;
    let customer_id = rest.strip_suffix("/portal-sessions")?;
    (!customer_id.is_empty() && !customer_id.contains('/')).then_some(customer_id)
}

fn https_base(base: &Url) -> String {
    let mut url = base.clone();
    match url.set_scheme("https") {
        Ok(()) => url.to_string(),
        Err(()) => base.to_string(),
    }
}

fn create_transaction(state: &mut FakeState, body: &Value) -> Response {
    let n = state.fresh_number();
    let id = format!("txn_{n}");
    let checkout_url = format!("{}checkout/{id}", https_base(&state.base));
    (
        StatusCode::CREATED,
        Json(json!({
            "data": {
                "id": id,
                "status": "draft",
                "custom_data": body.get("custom_data").cloned().unwrap_or(Value::Null),
                "checkout": {
                    "url": checkout_url,
                },
            },
            "meta": {
                "request_id": format!("req_{n}"),
            },
        })),
    )
        .into_response()
}

fn update_subscription(state: &mut FakeState, sub_id: &str, body: &Value) -> Response {
    let n = state.fresh_number();
    let items = body
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    json!({
                        "status": "active",
                        "quantity": item.get("quantity").cloned().unwrap_or_else(|| json!(1)),
                        "recurring": true,
                        "price": {
                            "id": item.get("price_id").cloned().unwrap_or(Value::Null),
                        },
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({
            "data": {
                "id": sub_id,
                "status": "active",
                "items": items,
            },
            "meta": {
                "request_id": format!("req_{n}"),
            },
        })),
    )
        .into_response()
}

fn create_portal_session(state: &mut FakeState, customer_id: &str) -> Response {
    let n = state.fresh_number();
    let overview = format!("{}portal/{customer_id}", https_base(&state.base));
    (
        StatusCode::CREATED,
        Json(json!({
            "data": {
                "id": format!("cpls_{n}"),
                "customer_id": customer_id,
                "urls": {
                    "general": {
                        "overview": overview,
                    },
                },
            },
            "meta": {
                "request_id": format!("req_{n}"),
            },
        })),
    )
        .into_response()
}

fn create_product(state: &mut FakeState, body: &Value) -> Response {
    let n = state.fresh_number();
    let row = json!({
        "id": format!("pro_{n}"),
        "name": body.get("name").cloned().unwrap_or(Value::Null),
        "tax_category": body.get("tax_category").cloned().unwrap_or(Value::Null),
        "type": body.get("type").cloned().unwrap_or_else(|| json!("standard")),
        "description": body.get("description").cloned().unwrap_or(Value::Null),
        "custom_data": body.get("custom_data").cloned().unwrap_or(Value::Null),
        "status": "active",
    });
    state.products.push(row.clone());
    (
        StatusCode::CREATED,
        Json(json!({
            "data": row,
            "meta": { "request_id": format!("req_{n}") },
        })),
    )
        .into_response()
}

fn create_price(state: &mut FakeState, body: &Value) -> Response {
    let n = state.fresh_number();
    let row = json!({
        "id": format!("pri_{n}"),
        "product_id": body.get("product_id").cloned().unwrap_or(Value::Null),
        "description": body.get("description").cloned().unwrap_or(Value::Null),
        "type": body.get("type").cloned().unwrap_or_else(|| json!("standard")),
        "billing_cycle": body.get("billing_cycle").cloned().unwrap_or(Value::Null),
        "unit_price": body.get("unit_price").cloned().unwrap_or(Value::Null),
        "quantity": body
            .get("quantity")
            .cloned()
            .unwrap_or_else(|| json!({"minimum": 1, "maximum": 100})),
        "custom_data": body.get("custom_data").cloned().unwrap_or(Value::Null),
        "status": "active",
    });
    state.prices.push(row.clone());
    (
        StatusCode::CREATED,
        Json(json!({
            "data": row,
            "meta": { "request_id": format!("req_{n}") },
        })),
    )
        .into_response()
}

fn list_entities(state: &mut FakeState, resource: &str, query: Option<&str>) -> Response {
    let n = state.fresh_number();
    let mut after: Option<String> = None;
    let mut requested_per_page: Option<usize> = None;
    let mut status_filter: Option<String> = None;
    let mut preserved_pairs: Vec<(String, String)> = Vec::new();
    for (k, v) in url::form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
        match k.as_ref() {
            "after" => after = Some(v.into_owned()),
            "per_page" => {
                requested_per_page = v.parse().ok();
                preserved_pairs.push((k.into_owned(), v.into_owned()));
            }
            "status" => {
                status_filter = Some(v.clone().into_owned());
                preserved_pairs.push((k.into_owned(), v.into_owned()));
            }
            _ => preserved_pairs.push((k.into_owned(), v.into_owned())),
        }
    }
    let statuses: Vec<&str> = status_filter
        .as_deref()
        .unwrap_or("active")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let rows = match resource {
        "products" => &state.products,
        _ => &state.prices,
    };
    let filtered: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            row.get("status")
                .and_then(Value::as_str)
                .is_some_and(|s| statuses.contains(&s))
        })
        .collect();
    let estimated_total = filtered.len();
    let remaining: &[&Value] = match after.as_deref() {
        Some(cursor) => filtered
            .iter()
            .position(|row| row.get("id").and_then(Value::as_str) == Some(cursor))
            .map_or(&[], |idx| &filtered[idx + 1..]),
        None => &filtered,
    };
    let per_page = requested_per_page
        .unwrap_or(DEFAULT_PER_PAGE)
        .min(state.page_size)
        .max(1);
    let page: Vec<Value> = remaining.iter().take(per_page).copied().cloned().collect();
    let has_more = state.endless_pages || remaining.len() > page.len();
    let next_cursor = page
        .last()
        .and_then(|row| row.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| state.endless_pages.then(|| format!("endless_{n}")));
    let mut next_url = state.base.clone();
    if let Ok(mut segments) = next_url.path_segments_mut() {
        segments.pop_if_empty().push(resource);
    }
    {
        let mut qp = next_url.query_pairs_mut();
        for (k, v) in &preserved_pairs {
            qp.append_pair(k, v);
        }
        if let Some(cursor) = &next_cursor {
            qp.append_pair("after", cursor);
        }
    }
    (
        StatusCode::OK,
        Json(json!({
            "data": page,
            "meta": {
                "request_id": format!("req_{n}"),
                "pagination": {
                    "per_page": per_page,
                    "next": next_url.as_str(),
                    "has_more": has_more,
                    "estimated_total": estimated_total,
                },
            },
        })),
    )
        .into_response()
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
