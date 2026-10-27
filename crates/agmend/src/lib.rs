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
mod console;
mod feed;
mod git;
mod hub;
mod local;
mod login;
mod socket;
mod sse;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agmen_client::{PathError, TokenStore};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::console::Console;
use crate::feed::Feeds;
use crate::hub::Hub;
use crate::local::Local;
use crate::login::Credentials;

/// The socket's file name in the state folder (agmen-client's, which the
/// desktop app connects to).
pub const SOCKET_NAME: &str = agmen_client::AGMEND_SOCKET;
/// The most local API connections open at once; more are closed at once.
const MAX_CONNECTIONS: usize = 64;
/// How often a watched project's roster is re-read.
const ROSTER_EVERY: Duration = Duration::from_secs(15);

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
}

/// Where agmend listens and whose login it reads.
#[derive(Debug, Clone)]
pub struct Config {
    socket: PathBuf,
    login_file: PathBuf,
    tokens: TokenStore,
    roster_every: Duration,
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
        Self {
            socket: socket.into(),
            login_file: login_file.into(),
            tokens,
            roster_every: ROSTER_EVERY,
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
        let config = Self::new(
            agmen_client::agmend_socket()?,
            agmen_client::login_file()?,
            TokenStore::from_env(),
        );
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
    console: Arc<Console>,
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
        let credentials = Credentials::new(config.login_file, config.tokens, http.clone());
        let hub = Arc::new(Hub::new(http, credentials));
        let feeds = Feeds::new(hub.clone(), config.roster_every);
        let bound = socket::bind(&config.socket)?;
        Ok(Self {
            bound,
            console: Arc::new(Console {
                hub,
                feeds,
                local: Local {
                    personal_roles: config.personal_roles,
                },
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
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let mut tasks = JoinSet::new();
        loop {
            while tasks.try_join_next().is_some() {}
            let stream = match self.bound.listener.accept().await {
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
            let console = self.console.clone();
            tasks.spawn(async move {
                api::serve(stream, console).await;
                drop(slot);
            });
        }
    }
}
