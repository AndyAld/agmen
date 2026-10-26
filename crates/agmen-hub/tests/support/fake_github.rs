//! A fake GitHub for the OAuth web flow (`/login/oauth/authorize`,
//! `/login/oauth/access_token` and `/user`) and the users API
//! (`/users/{login}`), on an ephemeral port.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinHandle;
use url::Url;

use super::new_id;

pub const CLIENT_ID: &str = "agmen-test-client";

/// Built at runtime so the source holds no secret-shaped literal (R22).
pub fn client_secret() -> String {
    format!("fake-secret-{}", "s".repeat(24))
}

/// A GitHub account: its numeric id and login.
type Account = (u64, String);

#[derive(Debug, Default)]
struct Accounts {
    /// Who signs in at the next authorize call.
    current: Option<Account>,
    codes: HashMap<String, Account>,
    tokens: HashMap<String, Account>,
    /// Accounts by login, lowercased: who holds each login now.
    users: HashMap<String, Account>,
}

type Shared = Arc<Mutex<Accounts>>;

#[derive(Debug)]
pub struct FakeGithub {
    pub base: Url,
    accounts: Shared,
    server: JoinHandle<()>,
}

impl Drop for FakeGithub {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakeGithub {
    pub async fn start() -> Self {
        let accounts = Shared::default();
        let app = Router::new()
            .route("/login/oauth/authorize", get(authorize))
            .route("/login/oauth/access_token", post(access_token))
            .route("/user", get(user))
            .route("/users/{login}", get(user_by_login))
            .with_state(accounts.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base,
            accounts,
            server,
        }
    }

    /// The account the next sign-in uses. It also holds `login` from now
    /// on, as far as `/users/{login}` is concerned.
    pub fn sign_in_as(&self, id: u64, login: &str) {
        self.register(id, login);
        self.accounts.lock().unwrap().current = Some((id, login.to_owned()));
    }

    /// Account `id` holds `login` from now on (a new account, or a login
    /// that changed hands).
    pub fn register(&self, id: u64, login: &str) {
        self.accounts
            .lock()
            .unwrap()
            .users
            .insert(login.to_ascii_lowercase(), (id, login.to_owned()));
    }
}

#[derive(Debug, Deserialize)]
struct AuthorizeQuery {
    client_id: String,
    redirect_uri: String,
    state: String,
}

async fn authorize(State(accounts): State<Shared>, Query(q): Query<AuthorizeQuery>) -> Response {
    if q.client_id != CLIENT_ID {
        return (StatusCode::BAD_REQUEST, "unknown client_id").into_response();
    }
    let mut accounts = accounts.lock().unwrap();
    let account = accounts.current.clone().expect("call sign_in_as first");
    let code = new_id();
    accounts.codes.insert(code.clone(), account);
    let mut back = Url::parse(&q.redirect_uri).unwrap();
    back.query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &q.state);
    Redirect::to(back.as_str()).into_response()
}

#[derive(Debug, Deserialize)]
struct TokenForm {
    client_id: String,
    client_secret: String,
    code: String,
}

async fn access_token(State(accounts): State<Shared>, Form(form): Form<TokenForm>) -> Response {
    if form.client_id != CLIENT_ID || form.client_secret != client_secret() {
        return Json(json!({ "error": "incorrect_client_credentials" })).into_response();
    }
    let mut accounts = accounts.lock().unwrap();
    // GitHub answers a bad code with 200 and an error body.
    let Some(account) = accounts.codes.remove(&form.code) else {
        return Json(json!({ "error": "bad_verification_code" })).into_response();
    };
    let token = format!("gho_{}", new_id());
    accounts.tokens.insert(token.clone(), account);
    Json(json!({ "access_token": token, "token_type": "bearer", "scope": "" })).into_response()
}

async fn user(State(accounts): State<Shared>, headers: HeaderMap) -> Response {
    if headers.get(header::USER_AGENT).is_none() {
        // GitHub's API refuses requests without a User-Agent.
        return (StatusCode::FORBIDDEN, "User-Agent required").into_response();
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    match accounts.lock().unwrap().tokens.get(token) {
        Some((id, login)) => Json(json!({ "id": id, "login": login })).into_response(),
        None => (StatusCode::UNAUTHORIZED, "bad token").into_response(),
    }
}

/// `GET /users/{login}`: GitHub matches the login in any case.
async fn user_by_login(
    State(accounts): State<Shared>,
    Path(login): Path<String>,
    headers: HeaderMap,
) -> Response {
    if headers.get(header::USER_AGENT).is_none() {
        return (StatusCode::FORBIDDEN, "User-Agent required").into_response();
    }
    match accounts
        .lock()
        .unwrap()
        .users
        .get(&login.to_ascii_lowercase())
    {
        Some((id, login)) => Json(json!({ "id": id, "login": login })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Not Found" })),
        )
            .into_response(),
    }
}
