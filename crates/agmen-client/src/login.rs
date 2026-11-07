//! The signed-in person, as `agmen login` leaves them: who signed in at
//! which hub (the login file) and their tokens (one token-store entry).
//! The CLI and agmend both read them here, and both renew the access token
//! the same way.
//!
//! A person's access and refresh tokens are one token-store entry, keyed by
//! the hub's origin and the person's id (rulings R17 and R42), and are sent
//! only to that hub. The access token lasts 1 hour; a reader renews it with
//! the refresh grant shortly before it runs out. The hub replaces the
//! refresh token on every use, so the renewed pair is stored at once, unless
//! the person signed out, or in as someone else, while the hub renewed it
//! (then the pair is dropped).

use std::path::{Path, PathBuf};

use agmen_proto::{IdentityId, Timestamp};
use jiff::SignedDuration;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::body::{BodyReadError, read_hub_body};
use crate::token_store::{TokenKey, TokenStore, TokenStoreError};

#[path = "login_lock.rs"]
mod lock;
use lock::{LoginLock, RENEWAL_LOCK_TIMEOUT, SIGN_IN_LOCK_TIMEOUT};

/// Renew the access token when it has less than this left.
const RENEW_LEAD: SignedDuration = SignedDuration::from_secs(120);
/// The largest login file read.
const MAX_LOGIN_BYTES: u64 = 64 * 1024;

/// Why the signed-in person or their tokens could not be had.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoginError {
    /// Another process held the login lock past the bounded wait.
    #[error("timed out waiting for {} after {timeout:?}; this is the local login.lock, not the hub; try again", path.display())]
    LockTimeout {
        /// The shared lock file.
        path: PathBuf,
        /// How long acquisition was allowed to wait.
        timeout: std::time::Duration,
    },
    /// The login lock file could not be opened or locked, for a reason other
    /// than another process holding it. A login or logout needs the lock, so
    /// it changed nothing.
    #[error(
        "cannot use the login lock {}: {error}; nothing was changed. Check its owner and permissions, and that the filesystem supports file locks. This is the local login.lock, not the hub",
        path.display()
    )]
    LockUnusable {
        /// The shared lock file.
        path: PathBuf,
        /// What failed. Not a `source`, since the message already holds it
        /// and an error chain would print it twice.
        error: std::io::Error,
    },
    /// The hub URL would expose credentials over an unsafe transport.
    #[error(transparent)]
    HubUrl(#[from] crate::HubUrlError),
    /// Nobody is signed in, the stored tokens are missing or unreadable, or
    /// the hub no longer takes the refresh token.
    #[error("not signed in")]
    NotSignedIn,
    /// The login file could not be read.
    #[error("reading {}", path.display())]
    File {
        /// The file.
        path: PathBuf,
        /// What failed.
        #[source]
        source: std::io::Error,
    },
    /// The login file is not JSON this version reads.
    #[error("{} is not a valid login file", path.display())]
    BadFile {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        #[source]
        source: serde_json::Error,
    },
    /// The login's hub URL has no origin to key its tokens by.
    #[error("the login's hub URL")]
    Key(#[source] TokenStoreError),
    /// The token store could not be read.
    #[error("reading the token store failed")]
    StoreRead(#[source] TokenStoreError),
    /// The token store could not be written.
    #[error("writing the token store failed")]
    StoreWrite(#[source] TokenStoreError),
    /// The tokens could not be encoded for the store.
    #[error("encoding the tokens")]
    Encode(#[source] serde_json::Error),
    /// The hub could not be reached.
    #[error("the hub could not be reached")]
    Http(#[source] reqwest::Error),
    /// The hub's answer could not be read.
    #[error("reading the hub's answer failed")]
    ReadAnswer(#[source] reqwest::Error),
    /// The hub's response body exceeded the size limit.
    #[error("the hub's response body exceeds the {limit} byte limit")]
    HubBodyTooLarge {
        /// The maximum response body size.
        limit: usize,
    },
    /// The hub answered success with a body that is not a token response.
    #[error("the hub's answer is not what agmen expects")]
    BadAnswer(#[source] serde_json::Error),
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
    /// The login file or its folder could not be written.
    #[error("writing {}", path.display())]
    Write {
        /// The file, or the folder that could not be created.
        path: PathBuf,
        /// What failed.
        #[source]
        source: std::io::Error,
    },
    /// The login file could not be removed during logout.
    #[error("removing {}", path.display())]
    Remove {
        /// The login file.
        path: PathBuf,
        /// What failed.
        #[source]
        source: std::io::Error,
    },
    /// The person declined the sign-in at GitHub: the token endpoint
    /// answered 400 `access_denied` to the device code.
    #[error("the sign-in was declined")]
    Declined,
    /// The device code ran out before the person confirmed it: the token
    /// endpoint answered `expired_token`, or the hub's `expires_in` passed.
    #[error("the code expired before it was confirmed")]
    Expired,
}

impl From<BodyReadError> for LoginError {
    fn from(error: BodyReadError) -> Self {
        match error {
            BodyReadError::Http(error) => Self::ReadAnswer(error),
            BodyReadError::TooLarge { limit } => Self::HubBodyTooLarge { limit },
        }
    }
}

/// Who `agmen login` signed in, kept in the login file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Login {
    /// The hub they signed in at.
    #[serde(deserialize_with = "crate::hub::deserialize_hub_url")]
    pub hub_url: Url,
    /// Their id at that hub.
    pub human_id: IdentityId,
    /// Their handle.
    pub handle: String,
}

impl Login {
    /// Forgets the signed-in person and their tokens, returning the old
    /// login, or `None` if nobody is signed in. The same lock as login and
    /// renewal covers reading the login, deleting its tokens and its file.
    /// File and keychain work runs off the async workers.
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use agmen_client::{Login, TokenStore, login_file};
    /// let _previous = Login::forget(&TokenStore::from_env(), &login_file()?).await?;
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// [`LoginError::LockTimeout`] when another process holds the lock too
    /// long and [`LoginError::LockUnusable`] when it cannot be taken at all;
    /// both leave the login and its tokens as they were. Else the error of
    /// reading the login or deleting its tokens or file.
    pub async fn forget(store: &TokenStore, login_file: &Path) -> Result<Option<Self>, LoginError> {
        let (store, path) = (store.clone(), login_file.to_owned());
        tokio::task::spawn_blocking(move || {
            let _lock = LoginLock::hold(&path, SIGN_IN_LOCK_TIMEOUT)?;
            let Some(login) = Self::read(&path)? else {
                return Ok(None);
            };
            let key = login.key().map_err(LoginError::Key)?;
            store.delete(&key).map_err(LoginError::StoreWrite)?;
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err(LoginError::Remove { path, source }),
            }
            Ok(Some(login))
        })
        .await
        .map_err(LoginError::Join)?
    }

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
/// [`LoginError::Refused`] with the OAuth error when the hub refuses,
/// [`LoginError::Http`] when it cannot be reached, and
/// [`LoginError::ReadAnswer`] or [`LoginError::BadAnswer`] when its
/// success answer cannot be read.
/// [`LoginError::HubBodyTooLarge`] when a success or error body exceeds 16 MiB.
pub async fn token_grant(
    http: &reqwest::Client,
    hub: &Url,
    form: &[(&str, &str)],
) -> Result<HumanTokens, LoginError> {
    crate::check_hub_url(hub)?;
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
    let bytes = read_hub_body(response).await;
    if status.is_success() {
        let bytes = bytes.map_err(LoginError::from)?;
        let granted: TokenResponse =
            serde_json::from_slice(&bytes).map_err(LoginError::BadAnswer)?;
        return Ok(HumanTokens::new(
            granted.access_token,
            granted.refresh_token,
            granted.expires_in,
        ));
    }
    Err(refusal(status.as_u16(), bytes))
}

/// The refusal for a non-success answer of `status`, from its body. A
/// body that cannot be read or does not parse leaves only the status.
pub(crate) fn refusal(status: u16, bytes: Result<Vec<u8>, BodyReadError>) -> LoginError {
    let body = match bytes {
        // A malformed error body leaves only the status.
        Ok(bytes) => serde_json::from_slice::<ErrorBody>(&bytes).ok(),
        // Preserve status-only errors when the body cannot be read.
        Err(BodyReadError::Http(_)) => None,
        Err(error) => return error.into(),
    };
    LoginError::Refused {
        status,
        code: body.as_ref().map(|b| b.error.clone()).unwrap_or_default(),
        description: body.map(|b| b.error_description).unwrap_or_default(),
    }
}

/// Stores `tokens` as `login`'s, replacing any there.
///
/// # Errors
///
/// [`LoginError::StoreWrite`] when the store cannot be written.
pub async fn store_tokens(
    store: &TokenStore,
    login: &Login,
    tokens: &HumanTokens,
) -> Result<(), LoginError> {
    let key = login.key().map_err(LoginError::Key)?;
    let json = serde_json::to_string(tokens).map_err(LoginError::Encode)?;
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.set(&key, &json))
        .await
        .map_err(LoginError::Join)?
        .map_err(LoginError::StoreWrite)
}

/// Saves `login` as the signed-in person: deletes the tokens of the login
/// it replaces when the key changes, stores `tokens`, and writes the login
/// file at `login_file`, creating its folder. File and store work runs off
/// the async workers.
///
/// # Errors
///
/// The error of reading the old login file, deleting the old tokens
/// ([`LoginError::StoreWrite`]), storing the new ones, or writing the file
/// ([`LoginError::Write`]). [`LoginError::LockTimeout`] if another process
/// holds the shared login lock too long and [`LoginError::LockUnusable`] if it
/// cannot be taken at all; both change nothing.
pub async fn save_login(
    store: &TokenStore,
    login_file: &Path,
    login: &Login,
    tokens: &HumanTokens,
) -> Result<(), LoginError> {
    let key = login.key().map_err(LoginError::Key)?;
    let token_json = serde_json::to_string(tokens).map_err(LoginError::Encode)?;
    let json = serde_json::to_vec_pretty(login).map_err(LoginError::Encode)?;
    let (store, path) = (store.clone(), login_file.to_owned());
    tokio::task::spawn_blocking(move || {
        let _lock = LoginLock::hold(&path, SIGN_IN_LOCK_TIMEOUT)?;
        if let Some(old) = Login::read(&path)? {
            let old_key = old.key().map_err(LoginError::Key)?;
            if old_key != key {
                store.delete(&old_key).map_err(LoginError::StoreWrite)?;
            }
        }
        store
            .set(&key, &token_json)
            .map_err(LoginError::StoreWrite)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| LoginError::Write {
                path: dir.to_owned(),
                source,
            })?;
        }
        std::fs::write(&path, json).map_err(|source| LoginError::Write { path, source })
    })
    .await
    .map_err(LoginError::Join)?
}

/// Checks that a login or logout could take the shared login lock now, by
/// taking it and letting go at once. Call it before the browser sign-in, so a
/// lock that is busy or unusable fails before the person is sent to GitHub.
/// It holds nothing afterwards: [`save_login`] takes the lock again around
/// its own writes, and a lock that goes bad in between still fails there.
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use agmen_client::{check_login_lock, login_file};
/// check_login_lock(&login_file()?).await?;
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// [`LoginError::LockTimeout`] when another process holds the lock past the
/// wait a login gets, and [`LoginError::LockUnusable`] when it cannot be
/// opened or locked. Either way nothing was changed.
pub async fn check_login_lock(login_file: &Path) -> Result<(), LoginError> {
    let path = login_file.to_owned();
    tokio::task::spawn_blocking(move || LoginLock::hold(&path, SIGN_IN_LOCK_TIMEOUT).map(drop))
        .await
        .map_err(LoginError::Join)?
}

/// `login`'s tokens from `store`, renewed through the hub first when the
/// access token is about to run out (the renewed pair is stored).
///
/// The hub spends the refresh token before the renewed pair is stored, and
/// `agmen logout`, or `agmen login` as someone else, can run in another
/// process meanwhile. So the pair is stored only if `login_file` still names
/// `login`'s person at `login`'s hub and their tokens are still in `store`.
/// The check runs inside the store's write, as late as the store allows (a
/// directory store runs it after the new file is written and synced, just
/// before it replaces the old one). A shared advisory lock beside the login
/// file covers the check and write, and login and logout's file and token
/// changes. A lock held past the bounded wait drops the pair as a changed
/// login would, and logs a warning. A lock file that cannot be used at all
/// (not a held lock) warns once, then logs repeats at debug, and leaves the
/// check on the login file as the only guard, since dropping the pair would
/// waste a refresh the hub already spent. The lock is
/// never held during the hub request.
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use agmen_client::{Login, TokenStore, human_tokens, login_file};
///
/// let file = login_file()?;
/// let login = Login::read(&file)?.ok_or("not signed in")?;
/// let store = TokenStore::from_env();
/// let tokens = human_tokens(&reqwest::Client::new(), &store, &file, &login).await?;
/// println!("token good until {}", tokens.expires_at());
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// [`LoginError::NotSignedIn`] when the store has no readable tokens for
/// `login`, the hub refuses the refresh token (400), or the person signed out
/// or in as someone else while the hub renewed them (the renewed pair is
/// dropped and logged at info), else the error of the store, the login file or
/// the hub.
pub async fn human_tokens(
    http: &reqwest::Client,
    store: &TokenStore,
    login_file: &Path,
    login: &Login,
) -> Result<HumanTokens, LoginError> {
    crate::check_hub_url(&login.hub_url)?;
    let key = login.key().map_err(LoginError::Key)?;
    let reader = store.clone();
    let stored = tokio::task::spawn_blocking(move || reader.get(&key))
        .await
        .map_err(LoginError::Join)?
        .map_err(LoginError::StoreRead)?
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
    // Logged here, off the blocking pool, and with no token in it.
    match renew_outcome(store, login_file, login, &renewed).await? {
        Renewal::Stored => {}
        Renewal::SignInChanged => {
            tracing::info!(
                path = %login_file.display(),
                "discarded a renewed token pair: the sign-in changed while the hub renewed it"
            );
            return Err(LoginError::NotSignedIn);
        }
        Renewal::LockBusy(timeout) => {
            tracing::warn!(
                path = %login_file.display(),
                "discarded a renewed token pair: login.lock stayed busy for {timeout:?}"
            );
            return Err(LoginError::NotSignedIn);
        }
    }
    Ok(renewed)
}

/// What became of a renewed pair.
enum Renewal {
    Stored,
    /// The sign-in changed while the hub renewed it.
    SignInChanged,
    /// Another process held the login lock past a renewal's wait.
    LockBusy(std::time::Duration),
}

/// [`renew_outcome`] as whether the pair was stored.
#[cfg(test)]
async fn store_renewed(
    store: &TokenStore,
    login_file: &Path,
    login: &Login,
    renewed: &HumanTokens,
) -> Result<bool, LoginError> {
    renew_outcome(store, login_file, login, renewed)
        .await
        .map(|outcome| matches!(outcome, Renewal::Stored))
}

/// Stores `renewed` as `login`'s tokens if `login` is still the signed-in
/// person, and says what became of it. The login lock covers the check and the
/// write ([`TokenStore::set_if`]), including the keychain path.
async fn renew_outcome(
    store: &TokenStore,
    login_file: &Path,
    login: &Login,
    renewed: &HumanTokens,
) -> Result<Renewal, LoginError> {
    let key = login.key().map_err(LoginError::Key)?;
    let json = serde_json::to_string(renewed).map_err(LoginError::Encode)?;
    let (store, login_file, login) = (store.clone(), login_file.to_owned(), login.clone());
    tokio::task::spawn_blocking(move || {
        // A lock held too long drops the pair. A lock file that cannot be used
        // at all must not cost a refresh the hub already spent, so then the
        // renewal falls back to the login file check alone.
        let _lock = match LoginLock::hold(&login_file, RENEWAL_LOCK_TIMEOUT) {
            Ok(lock) => Some(lock),
            Err(LoginError::LockTimeout { timeout, .. }) => {
                return Ok(Renewal::LockBusy(timeout));
            }
            Err(LoginError::LockUnusable { path, error }) => {
                // The renewal runs on every expiry, so a lock that stays
                // unusable warns once, and again only if the error changes.
                if lock::fell_back(&path, &error) {
                    tracing::warn!(
                        path = %path.display(),
                        error = %error,
                        "login.lock is unusable; the renewal checks the login file without it"
                    );
                } else {
                    tracing::debug!(
                        path = %path.display(),
                        error = %error,
                        "login.lock is still unusable; the renewal checks the login file without it"
                    );
                }
                None
            }
            Err(error) => return Err(error),
        };
        let mut unconfirmed = None;
        let stored = store
            .set_if(&key, &json, || {
                still_signed_in(&store, &key, &login_file, &login).unwrap_or_else(|error| {
                    unconfirmed = Some(error);
                    false
                })
            })
            .map_err(LoginError::StoreWrite)?;
        match unconfirmed {
            Some(error) => Err(error),
            None if stored => Ok(Renewal::Stored),
            None => Ok(Renewal::SignInChanged),
        }
    })
    .await
    .map_err(LoginError::Join)?
}

/// Whether `login` is still the signed-in person: the login file names the
/// same person at the same hub, and their tokens are still in `store`. The
/// CLI's logout deletes the tokens before it removes the login file, so tokens
/// already gone mean a logout is under way.
fn still_signed_in(
    store: &TokenStore,
    key: &TokenKey,
    login_file: &Path,
    login: &Login,
) -> Result<bool, LoginError> {
    let same = Login::read(login_file)?
        .is_some_and(|now| now.human_id == login.human_id && now.hub_url == login.hub_url);
    Ok(same && store.get(key).map_err(LoginError::StoreRead)?.is_some())
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "login_lock_tests.rs"]
mod lock_tests;
