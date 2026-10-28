//! agmend, the per-user agmen daemon: a local API on a Unix socket that the
//! desktop app (and later the CLI) calls, acting for the person `agmen
//! login` signed in. The desktop holds no tokens; agmend reads the CLI's
//! login and calls the hub over HTTP.
//!
//! Methods: `hello {api}`, `whoami`, `console_snapshot {project, root?}`,
//! `chat_history {project, before, limit}`, `console_watch {project}`,
//! `post {project, envelope}` and `file_text {project, root, worktree,
//! path}`. Frames and errors are agmen-local's.
//!
//! The `agmend` binary calls [`Config::from_env`], [`Agmend::bind`] and
//! [`Agmend::serve`]; tests do the same with a [`Config`] that points at a
//! temporary folder and a fake hub.

#![warn(missing_docs)]

mod api;
mod budget;
mod console;
mod feed;
mod git;
pub mod host;
mod hub;
mod identities;
mod listener;
mod local;
mod login;
mod socket;
mod sse;
mod state;
mod worktree;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agmen_client::{PathError, TokenStore};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::console::Console;
use crate::feed::Feeds;
use crate::hub::Hub;
use crate::identities::Identities;
use crate::local::Local;
use crate::login::Credentials;

/// Shared context for all local API connections.
#[derive(Debug)]
pub(crate) struct Core {
    host: Arc<host::Host>,
    console: Arc<Console>,
    identities: Arc<Identities>,
    state_dir: PathBuf,
    schema_scan: Arc<Semaphore>,
    shutdown: tokio::sync::Notify,
}

/// The socket's file name in the state folder (agmen-client's, which the
/// desktop app connects to).
pub const SOCKET_NAME: &str = agmen_client::AGMEND_SOCKET;

/// Read-only rollback snapshot of every existing daemon store.
///
/// # Errors
/// Returns an error for missing stores, incomplete discovery or unavailable snapshots.
pub fn store_snapshot(dir: &Path) -> Result<serde_json::Value, AgmendError> {
    let stores = state::snapshot(dir).map_err(|source| AgmendError::Store {
        path: dir.to_owned(),
        source: Box::new(source),
    })?;
    Ok(
        serde_json::json!({"stores": stores.into_iter().map(|(path, schema_version)| serde_json::json!({"path":path,"schema_version":schema_version})).collect::<Vec<_>>()}),
    )
}
/// The most local API connections open at once; more are closed at once.
const MAX_CONNECTIONS: usize = 64;
/// How often a watched project's roster is re-read.
const ROSTER_EVERY: Duration = Duration::from_secs(15);
/// How often a watch with a checkout re-reads its roles, worktrees and git
/// facts (Task 8 review fix 3).
const LOCAL_EVERY: Duration = Duration::from_secs(15);

/// Why agmend could not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgmendError {
    /// Another agmend holds the lock.
    #[error("agmend is already running (it holds {})", lock.display())]
    AlreadyRunning {
        /// The lock file.
        lock: PathBuf,
    },
    /// A file step failed.
    #[error("{what} failed for {}", path.display())]
    Io {
        /// What agmend was doing.
        what: &'static str,
        /// The file.
        path: PathBuf,
        /// What failed.
        #[source]
        source: std::io::Error,
    },
    /// The socket path has no folder.
    #[error("the socket path {} has no folder", .0.display())]
    SocketPath(PathBuf),
    /// The state or config folder is unknown.
    #[error(transparent)]
    Path(#[from] PathError),
    /// The HTTP client could not be built.
    #[error("building the HTTP client failed")]
    Http(#[source] reqwest::Error),
    /// Opening the daemon's durable database failed.
    #[error("opening daemon state at {} failed", path.display())]
    Store {
        /// Database path.
        path: PathBuf,
        /// Database or blocking task failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Where agmend listens and whose login it reads.
#[derive(Debug, Clone)]
pub struct Config {
    host_log: PathBuf,
    socket: PathBuf,
    login_file: PathBuf,
    tokens: TokenStore,
    roster_every: Duration,
    local_every: Duration,
    personal_roles: Option<PathBuf>,
}

impl Config {
    /// A config with the socket at `socket`, the CLI's login file at
    /// `login_file` and its tokens in `tokens`.
    #[must_use]
    pub fn new(
        socket: impl Into<PathBuf>,
        login_file: impl Into<PathBuf>,
        tokens: TokenStore,
    ) -> Self {
        let socket = socket.into();
        Self {
            host_log: socket.with_file_name("hub.log"),
            socket,
            login_file: login_file.into(),
            tokens,
            roster_every: ROSTER_EVERY,
            local_every: LOCAL_EVERY,
            personal_roles: None,
        }
    }

    /// This user's config: the socket in the state folder
    /// (`$XDG_STATE_HOME/agmen/agmend.sock`, on macOS
    /// `~/Library/Application Support/agmen/agmend.sock`), the CLI's login
    /// file and token store, and personal roles in `~/.config/agmen/roles`.
    ///
    /// # Errors
    ///
    /// [`AgmendError::Path`] when `HOME` is not set.
    pub fn from_env() -> Result<Self, AgmendError> {
        let mut config = Self::new(
            agmen_client::agmend_socket()?,
            agmen_client::login_file()?,
            TokenStore::from_env(),
        );
        if let Some(home) = std::env::home_dir() {
            config.host_log = home.join("Library/Logs/agmen/hub.log");
        }
        // Spec 3.5 and the desktop's settings use ~/.config/agmen on every
        // platform, not agmen-client's config folder.
        Ok(match std::env::home_dir() {
            Some(home) => config.personal_roles(home.join(".config/agmen/roles")),
            None => config,
        })
    }

    /// Re-reads each watched roster this often instead of every 15 s.
    #[must_use]
    pub const fn roster_every(mut self, every: Duration) -> Self {
        self.roster_every = every;
        self
    }

    /// Re-reads a watched checkout's local data this often instead of
    /// every 15 s.
    #[must_use]
    pub const fn local_every(mut self, every: Duration) -> Self {
        self.local_every = every;
        self
    }

    /// Reads the person's own role files from `dir` (spec 3.5's personal
    /// layer) instead of none.
    #[must_use]
    pub fn personal_roles(mut self, dir: impl Into<PathBuf>) -> Self {
        self.personal_roles = Some(dir.into());
        self
    }

    /// The socket path.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

/// A bound agmend, ready to serve.
#[derive(Debug)]
pub struct Agmend {
    bound: socket::Bound,
    core: Arc<Core>,
    opening: tokio::task::JoinHandle<()>,
}

impl Agmend {
    /// Takes the per-user lock and binds the socket (mode 0600). Must run
    /// inside a tokio runtime.
    ///
    /// # Errors
    ///
    /// [`AgmendError::AlreadyRunning`] when another agmend holds the lock,
    /// else the step that failed.
    pub fn bind(config: Config) -> Result<Self, AgmendError> {
        let http = Hub::http().map_err(AgmendError::Http)?;
        let credentials = Arc::new(Credentials::new(
            config.login_file,
            config.tokens,
            http.clone(),
        ));
        let hub = Arc::new(Hub::new(http, credentials.clone()));
        let feeds = Feeds::new(hub.clone(), config.roster_every);
        let bound = socket::bind(&config.socket)?;
        let path = config.socket.with_file_name("agmend.db");
        let database = path.clone();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        // bind is synchronous for compatibility. SQLite runs on a blocking
        // worker; the worker owns the lock even if this bounded wait expires.
        let opening = tokio::task::spawn_blocking(move || {
            let result = state::open_stores(&database).map(|store| (bound, store));
            // A timed-out caller has gone away; dropping the result closes
            // the database and releases the socket lock.
            if send.send(result).is_err() {
                tracing::debug!("daemon state opener's caller left");
            }
        });
        let result = receive.recv_timeout(Duration::from_secs(10));
        if result.is_err() {
            opening.abort();
        }
        let (bound, store) = result
            .map_err(|source| AgmendError::Store {
                path: path.clone(),
                source: Box::new(source),
            })?
            .map_err(|source| AgmendError::Store {
                path,
                source: Box::new(source),
            })?;
        let store = Arc::new(std::sync::Mutex::new(store));
        let identities = Arc::new(Identities::new(
            credentials,
            store.clone(),
            config.socket.with_file_name(""),
            hub.clone(),
        ));
        Ok(Self {
            bound,
            opening,
            core: Arc::new(Core {
                host: host::Host::new(&config.socket.with_file_name(""), config.host_log),
                state_dir: config.socket.with_file_name(""),
                schema_scan: Arc::new(Semaphore::new(1)),
                shutdown: tokio::sync::Notify::new(),
                identities,
                console: Arc::new(Console {
                    hub,
                    feeds,
                    local: Local {
                        personal_roles: config.personal_roles,
                    },
                    local_every: config.local_every,
                }),
            }),
        })
    }

    /// The socket path.
    #[must_use]
    pub fn socket(&self) -> &Path {
        self.bound.path()
    }

    /// Accepts connections until the future is dropped, one task each.
    /// Dropping it removes the socket and releases the lock.
    pub async fn serve(self) {
        self.serve_until(std::future::pending()).await;
    }

    /// Serves until `shutdown` resolves, then joins daemon-owned collectors
    /// and sessions, waiting at most 5 s per task and 10 s in total.
    pub async fn serve_until(self, shutdown: impl std::future::Future<Output = ()>) {
        if let Err(error) = self.opening.await {
            tracing::warn!(%error, "daemon state opener failed");
            return;
        }
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let hosting = self.core.host.clone().supervise();
        tokio::pin!(hosting);
        let mut host_stopped = false;
        let mut tasks = JoinSet::new();
        let identities = self.core.identities.clone();
        tasks.spawn(async move {
            identities.supervise().await;
        });
        tokio::pin!(shutdown);
        loop {
            while tasks.try_join_next().is_some() {}
            let accepted = tokio::select! {
                () = &mut shutdown => break,
                () = self.core.shutdown.notified() => break,
                () = &mut hosting => { host_stopped = true; break; }
                accepted = self.bound.listener.accept() => accepted,
            };
            let stream = match accepted {
                Ok((stream, _)) => stream,
                Err(err) => {
                    // Out of file descriptors, most likely; wait for some
                    // connections to close.
                    tracing::warn!(error = %err, "accepting a connection failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            if !self.bound.is_own(&stream) {
                tracing::warn!("refused a connection from another user");
                continue;
            }
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                tracing::warn!("refused a connection: {MAX_CONNECTIONS} are open");
                continue;
            };
            let core = self.core.clone();
            tasks.spawn(async move {
                api::serve(stream, core).await;
                drop(slot);
            });
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        self.core.identities.shutdown().await;
        self.core.host.stop();
        if !host_stopped {
            hosting.await;
        }
    }
}
