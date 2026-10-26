//! GitHub sign-in for the device flow's browser step (OAuth web flow, with
//! the `oauth2` crate). The hub only reads the account id and login.

use std::future::Future;
use std::pin::Pin;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, Redirect};
use oauth2::basic::BasicClient;
use oauth2::{
    AsyncHttpClient, AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken,
    EndpointNotSet, EndpointSet, HttpRequest, HttpResponse, RedirectUrl, TokenResponse, TokenUrl,
};
use serde::Deserialize;
use url::Url;

use super::device::{display_user_code, escape, normalize_user_code, page};
use crate::config::GithubConfig;
use crate::db::{hash_secret, is_unique_violation, new_id, new_secret};
use crate::http::{ApiError, Hub, public_link};

/// Why a GitHub sign-in failed on GitHub's side.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GithubError {
    /// A configured GitHub URL does not make a valid endpoint.
    #[error("GitHub endpoint URL {0} is invalid")]
    BadUrl(String),
    /// Exchanging the authorization code failed.
    #[error("exchanging the GitHub authorization code failed")]
    Exchange(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Reading the signed-in account failed.
    #[error("reading the GitHub account failed")]
    User(#[source] reqwest::Error),
}

type GithubClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// The GitHub OAuth app and the HTTP client that talks to GitHub.
#[derive(Debug, Clone)]
pub(crate) struct GithubOAuth {
    client: GithubClient,
    http: OAuthHttp,
    user_url: Url,
}

/// A GitHub account as `GET /user` describes it.
#[derive(Debug, Deserialize)]
struct GithubUser {
    id: u64,
    login: String,
}

/// Endpoint `path` under `base`, keeping any path prefix (GitHub
/// Enterprise serves its API under `/api/v3`).
fn endpoint(base: &Url, path: &str) -> String {
    public_link(base, path).into()
}

impl GithubOAuth {
    pub(crate) fn new(
        config: &GithubConfig,
        public_url: &Url,
        http: reqwest::Client,
    ) -> Result<Self, GithubError> {
        let bad = |url: String| move |_| GithubError::BadUrl(url);
        let auth = endpoint(&config.url, "login/oauth/authorize");
        let token = endpoint(&config.url, "login/oauth/access_token");
        let redirect = endpoint(public_url, "v1/auth/oauth/github/callback");
        let user = endpoint(&config.api_url, "user");
        let client = BasicClient::new(ClientId::new(config.client_id.clone()))
            .set_client_secret(ClientSecret::new(config.client_secret.expose().to_owned()))
            // GitHub takes the client credentials in the form body.
            .set_auth_type(AuthType::RequestBody)
            .set_auth_uri(AuthUrl::new(auth.clone()).map_err(bad(auth))?)
            .set_token_uri(TokenUrl::new(token.clone()).map_err(bad(token))?)
            .set_redirect_uri(RedirectUrl::new(redirect.clone()).map_err(bad(redirect))?);
        Ok(Self {
            client,
            http: OAuthHttp(http),
            user_url: Url::parse(&user).map_err(bad(user))?,
        })
    }

    /// The URL that sends the browser to GitHub, carrying `state`.
    fn authorize_url(&self, state: String) -> Url {
        let (url, _) = self.client.authorize_url(|| CsrfToken::new(state)).url();
        url
    }

    /// Exchanges an authorization code and reads who signed in.
    async fn account(&self, code: String) -> Result<GithubUser, GithubError> {
        let token = self
            .client
            .exchange_code(AuthorizationCode::new(code))
            .request_async(&self.http)
            .await
            .map_err(|e| GithubError::Exchange(Box::new(e)))?;
        self.http
            .0
            .get(self.user_url.clone())
            .bearer_auth(token.access_token().secret())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(GithubError::User)?
            .json()
            .await
            .map_err(GithubError::User)
    }
}

/// Lets `oauth2` send its requests through the hub's reqwest client.
#[derive(Debug, Clone)]
struct OAuthHttp(reqwest::Client);

impl<'c> AsyncHttpClient<'c> for OAuthHttp {
    type Error = reqwest::Error;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, reqwest::Error>> + Send + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let response = self.0.execute(request.try_into()?).await?;
            let status = response.status();
            let headers = response.headers().clone();
            let mut out = HttpResponse::new(response.bytes().await?.to_vec());
            *out.status_mut() = status;
            *out.headers_mut() = headers;
            Ok(out)
        })
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct StartQuery {
    user_code: String,
}

/// `GET /v1/auth/oauth/github/start?user_code=...`: ties a fresh OAuth state
/// to the device login and sends the browser to GitHub.
pub(super) async fn start(
    State(hub): State<Hub>,
    Query(query): Query<StartQuery>,
) -> Result<Redirect, ApiError> {
    let unknown =
        || ApiError::not_found("that code is unknown or has expired; run agmen login again");
    let user_code = normalize_user_code(&query.user_code).ok_or_else(unknown)?;
    let state = new_secret()?;
    // Starting again (a reload, a second tab) replaces the earlier state and
    // any sign-in that was not approved yet.
    let updated = sqlx::query(
        "UPDATE device_authorizations
         SET oauth_state_hash = $1, human_id = NULL, approval_hash = NULL
         WHERE user_code = $2 AND expires_at > now() AND approved_at IS NULL",
    )
    .bind(hash_secret(&state))
    .bind(&user_code)
    .execute(hub.pool())
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(unknown());
    }
    Ok(Redirect::to(hub.github().authorize_url(state).as_str()))
}

#[derive(Debug, Deserialize)]
pub(super) struct CallbackQuery {
    state: String,
    code: Option<String>,
    error: Option<String>,
}

/// `GET /v1/auth/oauth/github/callback`: GitHub sends the browser back here.
/// Signs the person in and shows the confirm page, whose button approves
/// the device login.
pub(super) async fn callback(
    State(hub): State<Hub>,
    Query(query): Query<CallbackQuery>,
) -> Result<Html<String>, ApiError> {
    let state_hash = hash_secret(&query.state);
    let pending: Option<String> = sqlx::query_scalar(
        "SELECT user_code FROM device_authorizations
         WHERE oauth_state_hash = $1 AND expires_at > now() AND approved_at IS NULL",
    )
    .bind(&state_hash)
    .fetch_optional(hub.pool())
    .await?;
    if pending.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "this sign-in link has expired or was already used; run agmen login again",
        ));
    }
    let code = match (query.code, query.error) {
        (Some(code), None) => code,
        (_, error) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "access_denied",
                format!(
                    "GitHub sign-in did not complete: {}",
                    error.as_deref().unwrap_or("no code")
                ),
            ));
        }
    };
    let account = hub.github().account(code).await?;
    let (human, handle) = upsert_human(&hub, &account).await?;
    let approval = new_secret()?;
    let user_code: Option<String> = sqlx::query_scalar(
        "UPDATE device_authorizations
         SET human_id = $1, approval_hash = $2, oauth_state_hash = NULL
         WHERE oauth_state_hash = $3 AND expires_at > now() AND approved_at IS NULL
         RETURNING user_code",
    )
    .bind(&human)
    .bind(hash_secret(&approval))
    .bind(&state_hash)
    .fetch_optional(hub.pool())
    .await?;
    let user_code = user_code.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "this sign-in was replaced by a newer one",
        )
    })?;
    let action = hub.public_link("v1/auth/device/approve");
    Ok(page(
        "Approve agmen login",
        &format!(
            "<p>Signed in with GitHub as <strong>{handle}</strong>.</p>\
             <p>Approve only if your terminal shows <strong>{code}</strong>.</p>\
             <form method=\"post\" action=\"{action}\">\
             <input type=\"hidden\" name=\"approve\" value=\"{approval}\">\
             <button>Approve</button></form>",
            handle = escape(&handle),
            code = escape(&display_user_code(&user_code)),
            action = escape(action.as_str()),
        ),
    ))
}

/// The human for a GitHub account, made at first sign-in with the login,
/// lowercased, as handle. Returns the id and handle.
async fn upsert_human(hub: &Hub, account: &GithubUser) -> Result<(String, String), ApiError> {
    let handle = account.login.to_ascii_lowercase();
    if !is_human_handle(&handle) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_handle",
            format!(
                "the GitHub login {:?} cannot be an agmen handle",
                account.login
            ),
        ));
    }
    let row: Result<(String, String), sqlx::Error> = sqlx::query_as(
        "INSERT INTO humans (id, provider, provider_sub, handle) VALUES ($1, 'github', $2, $3)
         ON CONFLICT (provider, provider_sub) DO UPDATE SET provider_sub = EXCLUDED.provider_sub
         RETURNING id, handle",
    )
    .bind(new_id()?)
    .bind(account.id.to_string())
    .bind(&handle)
    .fetch_one(hub.pool())
    .await;
    match row {
        Ok(row) => Ok(row),
        // The account is new and another account already has the handle.
        Err(err) if is_unique_violation(&err) => Err(ApiError::conflict(
            "handle_taken",
            format!("the handle {handle:?} belongs to another account"),
        )),
        Err(err) => Err(err.into()),
    }
}

/// `humans.handle`'s rule: a GitHub login, lowercased (`[a-z0-9-]`, at most
/// 39 characters, not starting with `-`).
fn is_human_handle(handle: &str) -> bool {
    (1..=39).contains(&handle.len())
        && !handle.starts_with('-')
        && handle
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_handles_follow_github_logins() {
        for good in ["andy", "adam-k", "a", "x1-2"] {
            assert!(is_human_handle(good), "{good}");
        }
        for bad in ["", "-andy", "andy/sol", "Andy", "a".repeat(40).as_str()] {
            assert!(!is_human_handle(bad), "{bad}");
        }
    }
}
