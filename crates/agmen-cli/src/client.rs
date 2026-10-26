//! The hub's HTTP API as the CLI calls it, and the signed-in person: who
//! `agmen login` signed in at which hub, and their tokens.
//!
//! A person's access and refresh tokens are one token-store entry, keyed by
//! the hub's origin and the person's id (rulings R17 and R42), and are sent
//! only to that hub. The access token lasts 1 hour; the CLI renews it with
//! the refresh grant shortly before it runs out.

use std::time::Duration;

use agmen_client::{TokenKey, TokenStore};
use agmen_proto::{IdentityId, Timestamp};
use anyhow::{Context as _, Result};
use jiff::SignedDuration;
use reqwest::{Method, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::output::Fail;
use crate::paths;

/// How long one request to the hub may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Renew the access token when it has less than this left.
const RENEW_LEAD: SignedDuration = SignedDuration::from_secs(120);
/// The largest login file read.
const MAX_LOGIN_BYTES: u64 = 64 * 1024;

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
        let bytes = response
            .bytes()
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
    let body = response.json::<ErrorBody>().await.ok();
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

/// Who `agmen login` signed in, kept in the login file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Login {
    /// The hub they signed in at.
    pub(crate) hub_url: Url,
    /// Their id at that hub.
    pub(crate) human_id: IdentityId,
    /// Their handle.
    pub(crate) handle: String,
}

impl Login {
    fn key(&self) -> Result<TokenKey> {
        TokenKey::new(&self.hub_url, &self.human_id).context("the login's hub URL")
    }
}

/// A person's tokens, stored as one JSON value.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct HumanTokens {
    access_token: String,
    refresh_token: String,
    expires_at: Timestamp,
}

impl std::fmt::Debug for HumanTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HumanTokens")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// The token endpoint's answer (RFC 6749 section 5.1).
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

impl TokenResponse {
    fn into_tokens(self) -> HumanTokens {
        HumanTokens {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at: Timestamp::now()
                .checked_add(SignedDuration::from_secs(self.expires_in))
                .unwrap_or_else(|_| Timestamp::now()),
        }
    }
}

/// Calls the token endpoint of the hub at `hub` with `form`.
pub(crate) async fn token_grant(hub: &Url, form: &[(&str, &str)]) -> Result<HumanTokens> {
    let request = http()?.post(url(hub, &["auth", "token"])).form(form);
    Ok(send::<TokenResponse>(request).await?.into_tokens())
}

impl HumanTokens {
    /// The access token.
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }
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
    blocking(move || {
        use std::io::Read as _;
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let mut raw = Vec::new();
        file.take(MAX_LOGIN_BYTES)
            .read_to_end(&mut raw)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&raw)
            .map(Some)
            .with_context(|| format!("{} is not a valid login file", path.display()))
    })
    .await
}

/// Stores `tokens` and writes the login file, replacing an earlier login.
pub(crate) async fn save_login(login: &Login, tokens: &HumanTokens) -> Result<()> {
    if let Some(old) = read_login().await?
        && old.key()? != login.key()?
    {
        delete_token(old.key()?).await?;
    }
    store_human_tokens(login, tokens).await?;
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
    delete_token(login.key()?).await?;
    let path = paths::login_file()?;
    blocking(move || match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", path.display())),
    })
    .await?;
    Ok(Some(login))
}

async fn store_human_tokens(login: &Login, tokens: &HumanTokens) -> Result<()> {
    let json = serde_json::to_string(tokens).context("encoding the tokens")?;
    set_token(login.key()?, json).await
}

/// The signed-in person, with a fresh access token.
///
/// # Errors
///
/// [`Fail::not_signed_in`] when nobody is signed in or the hub no longer
/// takes the refresh token.
pub(crate) async fn human() -> Result<Human> {
    let login = read_login().await?.ok_or_else(Fail::not_signed_in)?;
    let stored = get_token(login.key()?)
        .await?
        .ok_or_else(Fail::not_signed_in)?;
    let mut tokens: HumanTokens =
        serde_json::from_str(&stored).map_err(|_| Fail::not_signed_in())?;
    if tokens.expires_at.duration_since(Timestamp::now()) < RENEW_LEAD {
        let refresh = tokens.refresh_token.clone();
        tokens = match token_grant(
            &login.hub_url,
            &[("grant_type", "refresh_token"), ("refresh_token", &refresh)],
        )
        .await
        {
            Ok(tokens) => tokens,
            Err(err) if refusal(&err).is_some_and(|r| r.status == 400) => {
                return Err(Fail::not_signed_in().into());
            }
            Err(err) => return Err(err.context("renewing the sign-in failed")),
        };
        store_human_tokens(&login, &tokens).await?;
    }
    let api = Api::new(login.hub_url.clone(), tokens.access_token)?;
    Ok(Human { login, api })
}

/// `GET /v1/me`'s answer (ruling R56).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Me {
    pub(crate) id: IdentityId,
    pub(crate) handle: String,
    pub(crate) broker_url: String,
    pub(crate) projects: Vec<Membership>,
}

/// One project in [`Me`].
#[derive(Debug, Clone, Deserialize)]
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

/// The token stored under `key`.
pub(crate) async fn get_token(key: TokenKey) -> Result<Option<String>> {
    blocking(move || {
        TokenStore::from_env()
            .get(&key)
            .context("reading the token store failed")
    })
    .await
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
