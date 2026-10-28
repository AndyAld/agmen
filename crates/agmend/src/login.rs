//! The person agmend acts for: whoever `agmen login` signed in, read from
//! the CLI's login file and token store through agmen-client, so there is
//! one store and one renewal path.
//!
//! agmend keeps the access token in memory and asks the store again when it
//! is about to run out or the hub refused it. The hub replaces the refresh
//! token on every use, so renewals in this process go one at a time (the
//! cache lock is held across one); a renewal by the CLI at the same moment
//! can still spend the refresh token first, and then agmend reports
//! `unauthorized` until `agmen login` runs again.

use std::path::PathBuf;

use agmen_client::{HumanTokens, Login, LoginError, TokenKey, TokenStore, human_tokens};
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::IdentityId;
use tokio::sync::Mutex;
use url::Url;

/// The signed-in person's login and access token.
#[derive(Debug)]
pub(crate) struct Credentials {
    login_file: PathBuf,
    store: TokenStore,
    http: reqwest::Client,
    cache: Mutex<Option<(Login, HumanTokens)>>,
}

impl Credentials {
    /// Stores one freshly minted identity's credential without logging it.
    pub(crate) async fn set_token(
        &self,
        origin: &Url,
        id: &IdentityId,
        token: String,
    ) -> Result<(), ApiError> {
        let key = TokenKey::new(origin, id).map_err(|_| token_error())?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.set(&key, &token))
            .await
            .map_err(|_| token_error())?
            .map_err(|_| token_error())
    }
    /// Reads a credential off the async worker, without exposing store errors or values.
    pub(crate) async fn token(
        &self,
        origin: &Url,
        id: &IdentityId,
    ) -> Result<Option<String>, ApiError> {
        let key = TokenKey::new(origin, id).map_err(|_| token_error())?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.get(&key))
            .await
            .map_err(|_| token_error())?
            .map_err(|_| token_error())
    }

    pub(crate) fn new(login_file: PathBuf, store: TokenStore, http: reqwest::Client) -> Self {
        Self {
            login_file,
            store,
            http,
            cache: Mutex::new(None),
        }
    }

    /// Who is signed in, read from the login file now, so a new
    /// `agmen login` shows at once.
    ///
    /// # Errors
    ///
    /// [`LoginError::NotSignedIn`] when there is no login file.
    pub(crate) async fn login(&self) -> Result<Login, LoginError> {
        let path = self.login_file.clone();
        tokio::task::spawn_blocking(move || Login::read(&path))
            .await
            .map_err(LoginError::Join)??
            .ok_or(LoginError::NotSignedIn)
    }

    /// The login and a current access token for it.
    ///
    /// # Errors
    ///
    /// [`LoginError::NotSignedIn`] when nobody is signed in or the hub no
    /// longer takes the refresh token, else the store's or the hub's error.
    pub(crate) async fn access(&self) -> Result<(Login, String), LoginError> {
        let login = self.login().await?;
        let mut cache = self.cache.lock().await;
        if let Some((cached, tokens)) = cache.as_ref()
            && *cached == login
            && !tokens.expiring()
        {
            return Ok((login, tokens.access_token().to_owned()));
        }
        let tokens = human_tokens(&self.http, &self.store, &login).await?;
        let token = tokens.access_token().to_owned();
        *cache = Some((login.clone(), tokens));
        Ok((login, token))
    }

    /// Drops the cached token after the hub refused it (a logout, or a
    /// renewal by the CLI), so the next call reads the store again.
    pub(crate) async fn forget(&self) {
        *self.cache.lock().await = None;
    }
}

fn token_error() -> ApiError {
    ApiError {
        code: ErrorCode::Unauthorized,
        message: "could not read identity credential".into(),
    }
}
