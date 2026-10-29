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
//!
//! agmend can also sign a person in itself (`login_start`, `login_status`):
//! the hub's device flow from agmen-client, run by one background task, so
//! the desktop app needs no terminal. The device code stays inside that
//! task; the app sees only the user code and the page to open.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agmen_client::{
    DeviceStart, HumanTokens, Login, LoginError, TokenKey, TokenStore, device_start, device_wait,
    human_tokens, save_login, signed_in_as,
};
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{IdentityId, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use url::Url;

use crate::console::{bad_request, params};

/// The login and token the last hub call used.
type Cache = Mutex<Option<(Login, HumanTokens)>>;

/// Where the one sign-in agmend runs for the person stands.
#[derive(Debug)]
enum Status {
    /// No sign-in has been started since agmend began.
    Idle,
    /// The person has the code to confirm.
    Pending {
        user_code: String,
        verification_uri: Url,
        expires_at: Timestamp,
    },
    /// Signed in, and saved.
    SignedIn { hub: Url, handle: String },
    /// The person declined at GitHub.
    Declined,
    /// The code ran out unconfirmed.
    Expired,
    /// Anything else, as a short message that holds no token or code.
    Failed(String),
}

/// The one sign-in: its status and the task that runs it.
#[derive(Debug)]
struct Flow {
    /// Counts starts, so a replaced task cannot overwrite its successor's
    /// status.
    generation: u64,
    status: Status,
    task: Option<JoinHandle<()>>,
}

/// The signed-in person's login and access token.
#[derive(Debug)]
pub(crate) struct Credentials {
    login_file: PathBuf,
    store: TokenStore,
    http: reqwest::Client,
    cache: Arc<Cache>,
    flow: Arc<Mutex<Flow>>,
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

    /// Deletes one identity's credential, as `agmen leave` does.
    pub(crate) async fn delete_token(&self, origin: &Url, id: &IdentityId) -> Result<(), ApiError> {
        let key = TokenKey::new(origin, id).map_err(|_| token_error())?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.delete(&key))
            .await
            .map_err(|_| token_error())?
            .map_err(|_| token_error())
    }

    pub(crate) fn new(login_file: PathBuf, store: TokenStore, http: reqwest::Client) -> Self {
        Self {
            login_file,
            store,
            http,
            cache: Arc::default(),
            flow: Arc::new(Mutex::new(Flow {
                generation: 0,
                status: Status::Idle,
                task: None,
            })),
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
        forget_cached(&self.cache).await;
    }

    /// `login_start {hub?}`: starts the hub's device sign-in and answers
    /// `{user_code, verification_uri, expires_in, interval}`. One task then
    /// waits for the person, saves the login and forgets the cached token;
    /// a new start replaces an earlier one. `hub` defaults to the current
    /// login's hub.
    ///
    /// # Errors
    ///
    /// `bad_request` when no hub is named and nobody is signed in, or the
    /// hub URL is not one to send credentials to; `hub` when the hub's
    /// device start fails.
    pub(crate) async fn login_start(&self, raw: Value) -> Result<Value, ApiError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Start {
            #[serde(default)]
            hub: Option<String>,
        }
        let Start { hub } = params(raw)?;
        let hub = match hub {
            Some(hub) => hub
                .parse::<Url>()
                .map_err(|_| bad_request("the hub is not a URL"))?,
            None => self
                .login()
                .await
                .map(|login| login.hub_url)
                .map_err(|_| bad_request("name the hub to sign in at"))?,
        };
        let start = device_start(&self.http, &hub)
            .await
            .map_err(|error| match error {
                LoginError::HubUrl(error) => bad_request(error.to_string()),
                // Display only: the chain behind it is the transport's.
                error => ApiError {
                    code: ErrorCode::Hub,
                    message: format!("starting the sign-in failed: {error}"),
                },
            })?;
        let answer = json!({
            "user_code": start.user_code,
            "verification_uri": start.verification_uri,
            "expires_in": start.expires_in,
            "interval": start.interval,
        });
        let expires_at = Timestamp::now()
            .checked_add(Duration::from_secs(start.expires_in))
            .unwrap_or(Timestamp::MAX);
        let mut flow = self.flow.lock().await;
        if let Some(previous) = flow.task.take() {
            previous.abort();
        }
        flow.generation += 1;
        flow.status = Status::Pending {
            user_code: start.user_code.clone(),
            verification_uri: start.verification_uri.clone(),
            expires_at,
        };
        let job = Job {
            http: self.http.clone(),
            store: self.store.clone(),
            login_file: self.login_file.clone(),
            cache: self.cache.clone(),
            flow: self.flow.clone(),
            generation: flow.generation,
        };
        flow.task = Some(tokio::spawn(job.run(hub, start)));
        Ok(answer)
    }

    /// `login_status {}`: `{state}` with fields by state: `idle`,
    /// `pending` (`user_code`, `verification_uri`, `expires_at`),
    /// `signed_in` (`hub`, `handle`), `declined`, `expired`, or `failed`
    /// (`message`).
    ///
    /// # Errors
    ///
    /// `bad_request` for params.
    pub(crate) async fn login_status(&self, raw: Value) -> Result<Value, ApiError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct NoParams {}
        params::<NoParams>(raw)?;
        let flow = self.flow.lock().await;
        Ok(match &flow.status {
            Status::Idle => json!({"state": "idle"}),
            Status::Pending {
                user_code,
                verification_uri,
                expires_at,
            } => json!({
                "state": "pending",
                "user_code": user_code,
                "verification_uri": verification_uri,
                "expires_at": expires_at,
            }),
            Status::SignedIn { hub, handle } => {
                json!({"state": "signed_in", "hub": hub, "handle": handle})
            }
            Status::Declined => json!({"state": "declined"}),
            Status::Expired => json!({"state": "expired"}),
            Status::Failed(message) => json!({"state": "failed", "message": message}),
        })
    }
}

/// Drops the cached token, so the next hub call reads the store again.
async fn forget_cached(cache: &Cache) {
    *cache.lock().await = None;
}

/// What the sign-in task owns: clones, never `&Credentials`.
struct Job {
    http: reqwest::Client,
    store: TokenStore,
    login_file: PathBuf,
    cache: Arc<Cache>,
    flow: Arc<Mutex<Flow>>,
    generation: u64,
}

impl Job {
    /// Waits for the person, then saves the login and reports how it ended,
    /// unless a newer start replaced this one.
    ///
    /// The save runs under the flow lock, after a generation check. A new
    /// `login_start` takes the same lock before it aborts this task, so the
    /// abort cannot land inside `save_login` (a blocking file write cannot
    /// be stopped), and a replaced flow that wakes late finds its
    /// generation gone and saves nothing. The person is never signed in as
    /// the replaced flow's account.
    async fn run(self, hub: Url, start: DeviceStart) {
        let waited = self.wait(&hub, &start).await;
        let mut flow = self.flow.lock().await;
        if flow.generation != self.generation {
            return;
        }
        flow.status = match waited {
            Ok((login, tokens)) => {
                match save_login(&self.store, &self.login_file, &login, &tokens).await {
                    Ok(()) => {
                        forget_cached(&self.cache).await;
                        Status::SignedIn {
                            hub: login.hub_url,
                            handle: login.handle,
                        }
                    }
                    Err(error) => failed(&error),
                }
            }
            Err(LoginError::Declined) => Status::Declined,
            Err(LoginError::Expired) => Status::Expired,
            Err(error) => failed(&error),
        };
    }

    /// The person's confirmation: the tokens and who they are.
    async fn wait(
        &self,
        hub: &Url,
        start: &DeviceStart,
    ) -> Result<(Login, HumanTokens), LoginError> {
        let tokens = device_wait(&self.http, hub, start).await?;
        let login = signed_in_as(&self.http, hub, tokens.access_token()).await?;
        Ok((login, tokens))
    }
}

/// A sign-in that failed, as the short message `login_status` shows.
fn failed(error: &LoginError) -> Status {
    // Display only: no source chain, and no token or code is in any
    // variant's text.
    tracing::info!(%error, "device sign-in failed");
    Status::Failed(error.to_string())
}

fn token_error() -> ApiError {
    ApiError {
        code: ErrorCode::Unauthorized,
        message: "could not read identity credential".into(),
    }
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
