//! The hub's HTTP API as the CLI calls it, and the signed-in person: who
//! `agmen login` signed in at which hub, and their tokens.
//!
//! The login file and the person's tokens are agmen-client's (`login.rs`
//! there), which agmend reads too; this wraps them in the CLI's errors.

use std::time::Duration;

use agmen_client::{BodyReadError, LoginError, TokenKey, TokenStore, read_hub_body};
use agmen_proto::IdentityId;
use anyhow::{Context as _, Result};
use reqwest::{Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::output::Fail;
use crate::paths;

/// How long one request to the hub may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The hub refused a request: its status and `{error, error_description}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) status: u16,
    pub(crate) code: String,
    pub(crate) description: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.description.is_empty() {
            write!(f, "the hub answered {} {}", self.status, self.code)
        } else {
            write!(
                f,
                "{} (the hub answered {} {})",
                self.description, self.status, self.code
            )
        }
    }
}

impl std::error::Error for Refusal {}

/// The hub's error body.
#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: String,
    #[serde(default)]
    error_description: String,
}

/// An HTTP client that follows no redirects, so a token never reaches
/// another host.
pub(crate) fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("agmen-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("building the HTTP client failed")
}

/// `/v1/<segments>` under `base`, keeping any path prefix it has.
pub(crate) fn url(base: &Url, segments: &[&str]) -> Url {
    let mut url = base.clone();
    if let Ok(mut path) = url.path_segments_mut() {
        path.pop_if_empty().push("v1").extend(segments);
    }
    url
}

/// Sends `request` and reads a JSON answer, or the hub's refusal.
pub(crate) async fn send<T: DeserializeOwned>(request: RequestBuilder) -> Result<T> {
    let response = request
        .send()
        .await
        .context("the hub could not be reached")?;
    let status = response.status();
    if status.is_success() {
        let bytes = read_hub_body(response)
            .await
            .context("reading the hub's answer failed")?;
        // A 204 carries no body; the caller asks for `()` then.
        let bytes = if bytes.is_empty() {
            &b"null"[..]
        } else {
            &bytes[..]
        };
        return serde_json::from_slice(bytes).context("the hub's answer is not what agmen expects");
    }
    let body = match read_hub_body(response).await {
        Ok(bytes) => serde_json::from_slice::<ErrorBody>(&bytes).ok(),
        // Preserve status-only refusals when the transport fails.
        Err(BodyReadError::Http(_)) => None,
        Err(error) => return Err(error.into()),
    };
    Err(Refusal {
        status: status.as_u16(),
        code: body.as_ref().map(|b| b.error.clone()).unwrap_or_default(),
        description: body.map(|b| b.error_description).unwrap_or_default(),
    }
    .into())
}

/// The hub refusal behind `err`, if it is one.
pub(crate) fn refusal(err: &anyhow::Error) -> Option<&Refusal> {
    err.downcast_ref::<Refusal>()
}

/// Calls to one hub with one bearer token.
#[derive(Clone)]
pub(crate) struct Api {
    http: reqwest::Client,
    base: Url,
    token: String,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

impl Api {
    pub(crate) fn new(base: Url, token: impl Into<String>) -> Result<Self> {
        agmen_client::check_hub_url(&base)?;
        Ok(Self {
            http: http()?,
            base,
            token: token.into(),
        })
    }

    /// A request to `/v1/<segments>` with the token.
    pub(crate) fn request(&self, method: Method, segments: &[&str]) -> RequestBuilder {
        self.http
            .request(method, url(&self.base, segments))
            .bearer_auth(&self.token)
    }

    pub(crate) async fn get<T: DeserializeOwned>(&self, segments: &[&str]) -> Result<T> {
        send(self.request(Method::GET, segments)).await
    }

    pub(crate) async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        segments: &[&str],
        body: &B,
    ) -> Result<T> {
        send(self.request(Method::POST, segments).json(body)).await
    }
}

#[cfg(test)]
#[test]
fn hub_url_policy_api() {
    for hub in [
        "http://localhost",
        "http://127.0.0.1",
        "http://127.2.3.4",
        "http://[::1]",
        "https://hub.example",
    ] {
        Api::new(hub.parse().unwrap(), "token").unwrap();
    }
    for hub in ["http://hub.example", "ftp://localhost"] {
        let error = Api::new(hub.parse().unwrap(), "token").unwrap_err();
        assert!(error.to_string().contains("use https"), "{error}");
    }
}

pub(crate) use agmen_client::{HumanTokens, Login};

/// Calls the token endpoint of the hub at `hub` with `form`. A refusal
/// comes back as a [`Refusal`].
pub(crate) async fn token_grant(hub: &Url, form: &[(&str, &str)]) -> Result<HumanTokens> {
    agmen_client::token_grant(&http()?, hub, form)
        .await
        .map_err(login_error)
}

/// `err` as the CLI reports it: a refusal as a [`Refusal`], nobody signed
/// in as [`Fail::not_signed_in`].
fn login_error(err: LoginError) -> anyhow::Error {
    match err {
        LoginError::Refused {
            status,
            code,
            description,
        } => Refusal {
            status,
            code,
            description,
        }
        .into(),
        LoginError::NotSignedIn => Fail::not_signed_in().into(),
        err => anyhow::Error::new(err),
    }
}

/// The token-store key of `login`'s tokens.
fn key(login: &Login) -> Result<TokenKey> {
    login.key().context("the login's hub URL")
}

/// The signed-in person and an API client with their access token.
#[derive(Debug)]
pub(crate) struct Human {
    pub(crate) login: Login,
    pub(crate) api: Api,
}

/// Reads the login file, or `None` when nobody is signed in.
pub(crate) async fn read_login() -> Result<Option<Login>> {
    let path = paths::login_file()?;
    blocking(move || Ok(Login::read(&path)?)).await
}

/// Stores `tokens` and writes the login file, replacing an earlier login.
pub(crate) async fn save_login(login: &Login, tokens: &HumanTokens) -> Result<()> {
    if let Some(old) = read_login().await?
        && key(&old)? != key(login)?
    {
        delete_token(key(&old)?).await?;
    }
    agmen_client::store_tokens(&TokenStore::from_env(), login, tokens).await?;
    let path = paths::login_file()?;
    let json = serde_json::to_vec_pretty(login).context("encoding the login file")?;
    blocking(move || {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
    })
    .await
}

/// Forgets the signed-in person: their tokens and the login file. Returns
/// who it was, or `None` when nobody was signed in.
pub(crate) async fn forget_login() -> Result<Option<Login>> {
    let Some(login) = read_login().await? else {
        return Ok(None);
    };
    delete_token(key(&login)?).await?;
    let path = paths::login_file()?;
    blocking(move || match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", path.display())),
    })
    .await?;
    Ok(Some(login))
}

/// The signed-in person, with a fresh access token.
///
/// # Errors
///
/// [`Fail::not_signed_in`] when nobody is signed in or the hub no longer
/// takes the refresh token.
pub(crate) async fn human() -> Result<Human> {
    let login = read_login().await?.ok_or_else(Fail::not_signed_in)?;
    let tokens = agmen_client::human_tokens(&http()?, &TokenStore::from_env(), &login)
        .await
        .map_err(|err| match err {
            // The renewal's own call to the hub failed: the CLI has always
            // said so with this context. Reading and writing the store, the
            // login's key and a blocking task speak for themselves.
            err @ (LoginError::Refused { .. }
            | LoginError::Http(_)
            | LoginError::ReadAnswer(_)
            | LoginError::BadAnswer(_)) => login_error(err).context("renewing the sign-in failed"),
            err => login_error(err),
        })?;
    let api = Api::new(login.hub_url.clone(), tokens.access_token())?;
    Ok(Human { login, api })
}

/// Whether two hub URLs name one hub: the same origin and path, where a
/// trailing slash does not count (`url` ignores it too).
pub(crate) fn same_hub(a: &Url, b: &Url) -> bool {
    a.origin() == b.origin() && a.path().trim_end_matches('/') == b.path().trim_end_matches('/')
}

/// Reuses a working sign-in only for the same hub origin and path.
pub(crate) async fn human_at(hub: &Url) -> Result<Option<Human>> {
    let Some(login) = read_login().await? else {
        return Ok(None);
    };
    if !same_hub(&login.hub_url, hub) {
        return Ok(None);
    }
    let result: Result<Human> = async {
        let human = human().await?;
        human.api.me().await?;
        Ok(human)
    }
    .await;
    match result {
        Ok(human) => Ok(Some(human)),
        Err(err)
            if err
                .downcast_ref::<Fail>()
                .is_some_and(|f| f.code == crate::output::Code::NotLoggedIn)
                || refusal(&err).is_some_and(|r| r.status == 401) =>
        {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// `GET /v1/me`'s answer (ruling R56).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Me {
    pub(crate) id: IdentityId,
    pub(crate) handle: String,
    pub(crate) broker_url: String,
    pub(crate) projects: Vec<Membership>,
}

/// One project in [`Me`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Membership {
    pub(crate) tenant: String,
    pub(crate) project: String,
    pub(crate) name: String,
    pub(crate) role: String,
}

impl Api {
    /// `GET /v1/me` (a human token).
    pub(crate) async fn me(&self) -> Result<Me> {
        self.get(&["me"]).await
    }
}

/// The token `store` holds under `key`.
pub(crate) async fn get_token(store: TokenStore, key: TokenKey) -> Result<Option<String>> {
    blocking(move || store.get(&key).context("reading the token store failed")).await
}

/// Stores `token` under `key`.
pub(crate) async fn set_token(key: TokenKey, token: String) -> Result<()> {
    blocking(move || {
        TokenStore::from_env()
            .set(&key, &token)
            .context("writing the token store failed")
    })
    .await
}

/// Removes the token under `key`.
pub(crate) async fn delete_token(key: TokenKey) -> Result<()> {
    blocking(move || {
        TokenStore::from_env()
            .delete(&key)
            .context("writing the token store failed")
    })
    .await
}

/// Runs `work` (files, the keychain, git, SQLite) on a blocking thread.
pub(crate) async fn blocking<R: Send + 'static>(
    work: impl FnOnce() -> Result<R> + Send + 'static,
) -> Result<R> {
    tokio::task::spawn_blocking(work)
        .await
        .context("a blocking task failed")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn same_hub_ignores_a_trailing_slash_only() {
        let url = |text: &str| Url::parse(text).unwrap();
        for (a, b, same) in [
            ("https://h.example", "https://h.example/", true),
            ("https://h.example/hub", "https://h.example/hub/", true),
            ("https://h.example/hub", "https://h.example/other/", false),
            ("https://h.example/", "https://h.example:8443/", false),
            ("https://h.example/", "http://h.example/", false),
        ] {
            assert_eq!(same_hub(&url(a), &url(b)), same, "{a} {b}");
        }
    }

    #[tokio::test]
    async fn normal_hub_answers_keep_their_behavior() {
        for (status, body, expected) in [
            (204, "", ""),
            (200, r#"{"ok":true}"#, ""),
            (
                403,
                r#"{"error":"forbidden","error_description":"no access"}"#,
                "no access (the hub answered 403 forbidden)",
            ),
            (
                403,
                r#"{"error":"forbidden"}"#,
                "the hub answered 403 forbidden",
            ),
            (403, "", "the hub answered 403 "),
            (403, "not json", "the hub answered 403 "),
        ] {
            let app = axum::Router::new().fallback(move || async move {
                (axum::http::StatusCode::from_u16(status).unwrap(), body)
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let result = send::<serde_json::Value>(http().unwrap().get(url)).await;
            task.abort();
            if status == 204 {
                assert_eq!(result.unwrap(), serde_json::Value::Null);
            } else if status == 200 {
                assert_eq!(result.unwrap(), serde_json::json!({"ok": true}));
            } else {
                assert_eq!(result.unwrap_err().to_string(), expected);
            }
        }
    }

    #[tokio::test]
    async fn oversized_hub_bodies_are_refused() {
        for status in [200, 403] {
            for chunked in [false, true] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                let task = tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    socket.read_exact(&mut [0; 1]).await.unwrap();
                    let framing = if chunked {
                        "Transfer-Encoding: chunked"
                    } else {
                        "Content-Length: 16777217"
                    };
                    socket
                        .write_all(
                            format!("HTTP/1.1 {status} Test\r\n{framing}\r\n\r\n").as_bytes(),
                        )
                        .await
                        .unwrap();
                    if chunked {
                        let chunk = format!("10000\r\n{}\r\n", " ".repeat(65536));
                        for _ in 0..257 {
                            if socket.write_all(chunk.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                });
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    send::<serde_json::Value>(http().unwrap().get(url)),
                )
                .await;
                task.abort();
                let error = result
                    .expect("refuse before the stalled body ends")
                    .unwrap_err();
                assert!(
                    format!("{error:#}").contains("16777216 byte limit"),
                    "{error:#}"
                );
            }
        }
    }
}
