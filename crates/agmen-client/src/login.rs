//! The signed-in person, as `agmen login` leaves them: who signed in at
//! which hub (the login file) and their tokens (one token-store entry).
//! The CLI and agmend both read them here, and both renew the access token
//! the same way.
//!
//! A person's access and refresh tokens are one token-store entry, keyed by
//! the hub's origin and the person's id (rulings R17 and R42), and are sent
//! only to that hub. The access token lasts 1 hour; a reader renews it with
//! the refresh grant shortly before it runs out. The hub replaces the
//! refresh token on every use, so the renewed pair is stored at once.

use std::path::{Path, PathBuf};

use agmen_proto::{IdentityId, Timestamp};
use jiff::SignedDuration;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::token_store::{TokenKey, TokenStore, TokenStoreError};

/// Renew the access token when it has less than this left.
const RENEW_LEAD: SignedDuration = SignedDuration::from_secs(120);
/// The largest login file read.
const MAX_LOGIN_BYTES: u64 = 64 * 1024;

/// Why the signed-in person or their tokens could not be had.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoginError {
    /// Nobody is signed in, the stored tokens are missing or unreadable, or
    /// the hub no longer takes the refresh token.
    #[error("not signed in")]
    NotSignedIn,
    /// The login file could not be read or written.
    #[error("the login file {path} failed")]
    File {
        /// The file.
        path: PathBuf,
        /// What failed.
        #[source]
        source: std::io::Error,
    },
    /// The login file is not JSON this version reads.
    #[error("{path} is not a valid login file")]
    BadFile {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        #[source]
        source: serde_json::Error,
    },
    /// The token store failed.
    #[error("the token store failed")]
    Store(#[from] TokenStoreError),
    /// The tokens could not be encoded for the store.
    #[error("encoding the tokens failed")]
    Encode(#[source] serde_json::Error),
    /// The hub could not be reached, or its answer could not be read.
    #[error("calling the hub failed")]
    Http(#[source] reqwest::Error),
    /// The hub's token endpoint refused.
    #[error("the hub answered {status} {code}")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// The OAuth error code, such as `authorization_pending`.
        code: String,
        /// The hub's `error_description`; empty when it gave none.
        description: String,
    },
    /// A blocking task (the file or the keychain) failed to run.
    #[error("a blocking task failed")]
    Join(#[source] tokio::task::JoinError),
}

/// Who `agmen login` signed in, kept in the login file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Login {
    /// The hub they signed in at.
    pub hub_url: Url,
    /// Their id at that hub.
    pub human_id: IdentityId,
    /// Their handle.
    pub handle: String,
}

impl Login {
    /// The token-store key of this person's tokens.
    ///
    /// # Errors
    ///
    /// [`TokenStoreError::OpaqueOrigin`] when the hub URL has no origin.
    pub fn key(&self) -> Result<TokenKey, TokenStoreError> {
        TokenKey::new(&self.hub_url, &self.human_id)
    }

    /// Reads the login file at `path`, or `None` when it does not exist.
    /// This reads a file; call it off the async runtime's worker threads.
    ///
    /// # Errors
    ///
    /// [`LoginError::File`] when it cannot be read and
    /// [`LoginError::BadFile`] when it does not parse.
    pub fn read(path: &Path) -> Result<Option<Self>, LoginError> {
        use std::io::Read as _;
        let file_error = |source| LoginError::File {
            path: path.to_owned(),
            source,
        };
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(file_error(err)),
        };
        let mut raw = Vec::new();
        file.take(MAX_LOGIN_BYTES)
            .read_to_end(&mut raw)
            .map_err(file_error)?;
        serde_json::from_slice(&raw)
            .map(Some)
            .map_err(|source| LoginError::BadFile {
                path: path.to_owned(),
                source,
            })
    }
}

/// A person's tokens, stored as one JSON value.
#[derive(Clone, Serialize, Deserialize)]
pub struct HumanTokens {
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

impl HumanTokens {
    /// Tokens as the token endpoint grants them: `expires_in` seconds from
    /// now.
    #[must_use]
    pub fn new(access_token: String, refresh_token: String, expires_in: i64) -> Self {
        Self {
            access_token,
            refresh_token,
            expires_at: Timestamp::now()
                .checked_add(SignedDuration::from_secs(expires_in))
                .unwrap_or_else(|_| Timestamp::now()),
        }
    }

    /// The access token.
    #[must_use]
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    /// When the access token expires.
    #[must_use]
    pub const fn expires_at(&self) -> Timestamp {
        self.expires_at
    }

    /// Whether the access token runs out within 2 minutes, so it should be
    /// renewed before use.
    #[must_use]
    pub fn expiring(&self) -> bool {
        self.expires_at.duration_since(Timestamp::now()) < RENEW_LEAD
    }
}

/// The token endpoint's answer (RFC 6749 section 5.1).
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

/// The token endpoint's error body.
#[derive(Deserialize)]
struct ErrorBody {
    error: String,
    #[serde(default)]
    error_description: String,
}

/// Calls the token endpoint of the hub at `hub` (`POST /v1/auth/token`)
/// with `form`: the device code grant or the refresh grant.
///
/// # Errors
///
/// [`LoginError::Refused`] with the OAuth error when the hub refuses, and
/// [`LoginError::Http`] when it cannot be reached or answers nonsense.
pub async fn token_grant(
    http: &reqwest::Client,
    hub: &Url,
    form: &[(&str, &str)],
) -> Result<HumanTokens, LoginError> {
    let mut url = hub.clone();
    if let Ok(mut path) = url.path_segments_mut() {
        path.pop_if_empty().extend(["v1", "auth", "token"]);
    }
    let response = http
        .post(url)
        .form(form)
        .send()
        .await
        .map_err(LoginError::Http)?;
    let status = response.status();
    if status.is_success() {
        let granted: TokenResponse = response.json().await.map_err(LoginError::Http)?;
        return Ok(HumanTokens::new(
            granted.access_token,
            granted.refresh_token,
            granted.expires_in,
        ));
    }
    // A body that does not parse leaves only the status.
    let body = response.json::<ErrorBody>().await.ok();
    Err(LoginError::Refused {
        status: status.as_u16(),
        code: body.as_ref().map(|b| b.error.clone()).unwrap_or_default(),
        description: body.map(|b| b.error_description).unwrap_or_default(),
    })
}

/// Stores `tokens` as `login`'s, replacing any there.
///
/// # Errors
///
/// [`LoginError::Store`] when the store cannot be written.
pub async fn store_tokens(
    store: &TokenStore,
    login: &Login,
    tokens: &HumanTokens,
) -> Result<(), LoginError> {
    let key = login.key()?;
    let json = serde_json::to_string(tokens).map_err(LoginError::Encode)?;
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.set(&key, &json))
        .await
        .map_err(LoginError::Join)??;
    Ok(())
}

/// `login`'s tokens from `store`, renewed through the hub first when the
/// access token is about to run out (the renewed pair is stored).
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use agmen_client::{Login, TokenStore, human_tokens, login_file};
///
/// let login = Login::read(&login_file()?)?.ok_or("not signed in")?;
/// let tokens = human_tokens(&reqwest::Client::new(), &TokenStore::from_env(), &login).await?;
/// println!("token good until {}", tokens.expires_at());
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// [`LoginError::NotSignedIn`] when the store has no readable tokens for
/// `login` or the hub refuses the refresh token (400), else the error of
/// the store or the hub.
pub async fn human_tokens(
    http: &reqwest::Client,
    store: &TokenStore,
    login: &Login,
) -> Result<HumanTokens, LoginError> {
    let key = login.key()?;
    let reader = store.clone();
    let stored = tokio::task::spawn_blocking(move || reader.get(&key))
        .await
        .map_err(LoginError::Join)??
        .ok_or(LoginError::NotSignedIn)?;
    let tokens: HumanTokens = serde_json::from_str(&stored).map_err(|_| LoginError::NotSignedIn)?;
    if !tokens.expiring() {
        return Ok(tokens);
    }
    let refresh = [
        ("grant_type", "refresh_token"),
        ("refresh_token", tokens.refresh_token.as_str()),
    ];
    let renewed = match token_grant(http, &login.hub_url, &refresh).await {
        Ok(renewed) => renewed,
        Err(LoginError::Refused { status: 400, .. }) => return Err(LoginError::NotSignedIn),
        Err(err) => return Err(err),
    };
    store_tokens(store, login, &renewed).await?;
    Ok(renewed)
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
