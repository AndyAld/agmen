//! A local GitHub device-flow server for tests, with no external calls.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::extract::{DefaultBodyLimit, Form, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::task::JoinHandle;
use url::Url;

use crate::HookError;
use crate::db::{new_id, new_secret};

/// Public client id the fake device endpoints accept.
pub const FAKE_GITHUB_CLIENT_ID: &str = "agmen-test-client";
const MAX_CODES: usize = 4096;
type Account = (u64, String);

#[derive(Debug)]
enum Approval {
    Pending,
    Approved(Account),
    Denied,
    Expired,
}

#[derive(Debug)]
struct Device {
    user_code: String,
    interval: u32,
    slow_down: bool,
    approval: Approval,
}

#[derive(Debug)]
struct Accounts {
    interval: u32,
    codes: HashMap<String, Device>,
    tokens: HashMap<String, Account>,
    users: HashMap<String, Account>,
    refusal: Option<String>,
    device_status: Option<u16>,
    polls: usize,
    device_form: Option<Vec<(String, String)>>,
    token_form: Option<Vec<(String, String)>>,
    device_accept: Option<String>,
}

impl Default for Accounts {
    fn default() -> Self {
        Self {
            interval: 1,
            codes: HashMap::new(),
            tokens: HashMap::new(),
            users: HashMap::new(),
            refusal: None,
            device_status: None,
            polls: 0,
            device_form: None,
            token_form: None,
            device_accept: None,
        }
    }
}

type Shared = Arc<Mutex<Accounts>>;

fn lock(accounts: &Shared) -> MutexGuard<'_, Accounts> {
    accounts.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fake GitHub on an ephemeral loopback port. Dropping it stops its server.
#[derive(Debug)]
pub struct FakeGithub {
    base: Url,
    accounts: Shared,
    server: JoinHandle<()>,
}

impl Drop for FakeGithub {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakeGithub {
    /// Starts a server on an ephemeral loopback port.
    ///
    /// # Errors
    /// Returns a bind or URL error when the local server cannot start.
    pub async fn start() -> Result<Self, HookError> {
        let accounts = Shared::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        let app = Router::new()
            .route("/login/device/code", post(device_code))
            .route("/login/oauth/access_token", post(access_token))
            .route("/user", get(user))
            .route("/users/{login}", get(user_by_login))
            .layer(DefaultBodyLimit::max(16 * 1024))
            .with_state((accounts.clone(), base.clone()));
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::error!(%error, "fake GitHub server failed");
            }
        });
        Ok(Self {
            base,
            accounts,
            server,
        })
    }

    /// The server's base URL.
    #[must_use]
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// Sets the interval for new device codes, clamped to 1 to 600 seconds.
    pub fn set_interval(&self, secs: u32) {
        lock(&self.accounts).interval = secs.clamp(1, 600);
    }

    /// Approves this user code as account `id`, which now holds `login`.
    pub fn approve(&self, user_code: &str, id: u64, login: &str) {
        self.register(id, login);
        if let Some(device) = lock(&self.accounts)
            .codes
            .values_mut()
            .find(|device| device.user_code.eq_ignore_ascii_case(user_code))
        {
            device.approval = Approval::Approved((id, login.to_owned()));
        }
    }

    /// Declines this user code.
    pub fn deny(&self, user_code: &str) {
        if let Some(device) = lock(&self.accounts)
            .codes
            .values_mut()
            .find(|device| device.user_code.eq_ignore_ascii_case(user_code))
        {
            device.approval = Approval::Denied;
        }
    }

    /// Makes this user code expire on the next poll.
    pub fn expire(&self, user_code: &str) {
        if let Some(device) = lock(&self.accounts)
            .codes
            .values_mut()
            .find(|device| device.user_code.eq_ignore_ascii_case(user_code))
        {
            device.approval = Approval::Expired;
        }
    }

    /// Answers the next poll for this code with slow_down, adding five seconds.
    pub fn slow_down_next(&self, user_code: &str) {
        if let Some(device) = lock(&self.accounts)
            .codes
            .values_mut()
            .find(|device| device.user_code.eq_ignore_ascii_case(user_code))
        {
            device.slow_down = true;
        }
    }

    /// Refuses the next token poll with this GitHub error code.
    pub fn refuse_next_poll(&self, error: &str) {
        lock(&self.accounts).refusal = Some(error.to_owned());
    }

    /// Answers device starts with this HTTP status.
    pub fn fail_device_code(&self, status: u16) {
        lock(&self.accounts).device_status = Some(status);
    }

    /// Records who holds a login for the users API, matching any case.
    pub fn register(&self, id: u64, login: &str) {
        lock(&self.accounts)
            .users
            .insert(login.to_ascii_lowercase(), (id, login.to_owned()));
    }

    /// The number of token requests received.
    #[must_use]
    pub fn token_polls(&self) -> usize {
        lock(&self.accounts).polls
    }

    /// The last device-start form, including unexpected fields.
    #[must_use]
    pub fn last_device_form(&self) -> Option<Vec<(String, String)>> {
        lock(&self.accounts).device_form.clone()
    }

    /// The last token form, including unexpected fields.
    #[must_use]
    pub fn last_token_form(&self) -> Option<Vec<(String, String)>> {
        lock(&self.accounts).token_form.clone()
    }

    /// The last device-start Accept header.
    #[must_use]
    pub fn last_device_accept(&self) -> Option<String> {
        lock(&self.accounts).device_accept.clone()
    }
}

fn accepts_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .is_some_and(|v| v == "application/json")
}

fn field<'a>(form: &'a [(String, String)], name: &str) -> Option<&'a str> {
    form.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

async fn device_code(
    State((accounts, base)): State<(Shared, Url)>,
    headers: HeaderMap,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let mut accounts = lock(&accounts);
    accounts.device_accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let valid = field(&form, "client_id") == Some(FAKE_GITHUB_CLIENT_ID);
    accounts.device_form = Some(form);
    if !accepts_json(&headers) {
        return "error=unsupported_response_format".into_response();
    }
    if let Some(status) = accounts.device_status {
        return StatusCode::from_u16(status)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response();
    }
    if !valid {
        return Json(json!({"error": "incorrect_client_credentials"})).into_response();
    }
    if accounts.codes.len() >= MAX_CODES {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let (Ok(device_code), Ok(random)) = (new_secret(), new_id()) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let consonants = b"BCDFGHJKLMNPQRST";
    let code: String = random
        .bytes()
        .take(8)
        .map(|b| char::from(consonants[usize::from(b & 15)]))
        .collect();
    let user_code = format!("{}-{}", &code[..4], &code[4..]);
    let interval = accounts.interval;
    let verification = crate::http::public_link(&base, "login/device");
    accounts.codes.insert(
        device_code.clone(),
        Device {
            user_code: user_code.clone(),
            interval,
            slow_down: false,
            approval: Approval::Pending,
        },
    );
    Json(json!({"device_code": device_code, "user_code": user_code, "verification_uri": verification, "expires_in": 900, "interval": interval})).into_response()
}

async fn access_token(
    State((accounts, _)): State<(Shared, Url)>,
    headers: HeaderMap,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let mut accounts = lock(&accounts);
    accounts.polls += 1;
    let valid = field(&form, "client_id") == Some(FAKE_GITHUB_CLIENT_ID)
        && field(&form, "grant_type") == Some("urn:ietf:params:oauth:grant-type:device_code");
    let code = field(&form, "device_code").unwrap_or_default().to_owned();
    accounts.token_form = Some(form);
    if !accepts_json(&headers) {
        return "error=unsupported_response_format".into_response();
    }
    if !valid {
        return Json(json!({"error": "incorrect_client_credentials"})).into_response();
    }
    if let Some(error) = accounts.refusal.take() {
        return Json(json!({"error": error})).into_response();
    }
    let Some(device) = accounts.codes.get_mut(&code) else {
        return Json(json!({"error": "bad_verification_code"})).into_response();
    };
    if device.slow_down {
        device.slow_down = false;
        device.interval = device.interval.saturating_add(5).min(600);
        return Json(json!({"error": "slow_down", "interval": device.interval})).into_response();
    }
    let answer = match &device.approval {
        Approval::Pending => json!({"error": "authorization_pending"}),
        Approval::Denied => json!({"error": "access_denied"}),
        Approval::Expired => json!({"error": "expired_token"}),
        Approval::Approved(account) => {
            let account = account.clone();
            let Ok(token) = new_secret() else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            accounts.tokens.insert(token.clone(), account);
            accounts.codes.remove(&code);
            json!({"access_token": token, "token_type": "bearer", "scope": ""})
        }
    };
    Json(answer).into_response()
}

async fn user(State((accounts, _)): State<(Shared, Url)>, headers: HeaderMap) -> Response {
    if headers.get(header::USER_AGENT).is_none() {
        return (StatusCode::FORBIDDEN, "User-Agent required").into_response();
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    match lock(&accounts).tokens.get(token) {
        Some((id, login)) => Json(json!({"id": id, "login": login})).into_response(),
        None => (StatusCode::UNAUTHORIZED, "bad token").into_response(),
    }
}

async fn user_by_login(
    State((accounts, _)): State<(Shared, Url)>,
    Path(login): Path<String>,
    headers: HeaderMap,
) -> Response {
    if headers.get(header::USER_AGENT).is_none() {
        return (StatusCode::FORBIDDEN, "User-Agent required").into_response();
    }
    match lock(&accounts).users.get(&login.to_ascii_lowercase()) {
        Some((id, login)) => Json(json!({"id": id, "login": login})).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response(),
    }
}
