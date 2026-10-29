//! One in-process agmend for one person's home, launching with the e2e
//! fake `claude`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agmen_client::{ClientKind, HubClient, IdentityFile, TokenKey, TokenStore};
use agmen_local::{ApiError, ErrorCode, Outcome, Request, Response, frame};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::UnixStream;

use super::fake_claude::FakeClaude;
use super::{Login, Sessions};
use crate::support::{Home, WAIT};

/// `sockaddr_un.sun_path` on macOS holds 104 bytes, and one of them is the
/// trailing NUL, so the path itself stays under that.
const MAX_SOCKET_BYTES: usize = 103;

/// A running agmend bound for one [`Home`].
#[derive(Debug)]
pub struct Daemon {
    socket: PathBuf,
    /// Short folder under `/tmp`. Kept so dropping the daemon removes it.
    _dir: TempDir,
    fake: FakeClaude,
    /// The home's token store, where `agent_launch` keeps each agent's token.
    tokens: TokenStore,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Daemon {
    /// Binds agmend on a short socket under `/tmp`, using `home`'s login
    /// and token store, `fake`, and the `agmen-e2e` binary.
    pub async fn start(home: &Home, fake: FakeClaude) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("ag")
            .tempdir_in("/tmp")
            .expect("a short socket folder under /tmp");
        let socket = dir.path().join(agmend::SOCKET_NAME);
        let bytes = socket.as_os_str().len();
        assert!(
            bytes <= MAX_SOCKET_BYTES,
            "socket path is {bytes} bytes (limit {MAX_SOCKET_BYTES}): {}",
            socket.display()
        );
        let config = agmend::Config::new(&socket, home.login_file(), home.tokens())
            .roster_every(Duration::from_millis(100))
            .test_programs(
                fake.program(),
                super::fake_claude::agmen(),
                fake.launch_path(),
            );
        let agmend = agmend::Agmend::bind(config)
            .unwrap_or_else(|err| panic!("binding agmend at {}: {err}", socket.display()));
        let task = tokio::spawn(async move {
            agmend.serve().await;
        });
        // Built before the wait, so a failed wait drops it: the task is
        // aborted and the folder removed.
        let daemon = Self {
            socket,
            _dir: dir,
            fake,
            tokens: home.tokens(),
            task: Some(task),
        };
        wait_until_listening(&daemon.socket).await;
        daemon
    }

    /// The fake `claude` this daemon launches.
    pub fn claude(&self) -> &FakeClaude {
        &self.fake
    }

    /// A client that has already said hello.
    pub async fn client(&self) -> Client {
        let mut client = Client::connect(&self.socket).await;
        let hello = client
            .call("hello", json!({"api": 1}))
            .await
            .unwrap_or_else(|err| panic!("hello: {err:?}"));
        assert_eq!(hello["api"], 1, "hello answered {hello}");
        client
    }

    /// Calls `agent_launch` with `params` and returns its answer. agmend
    /// keeps a persistent broker session for the agent that outlives the
    /// daemon, so `sessions` ends it after the test, as it does for
    /// `agmen watch`.
    pub async fn launch(&self, sessions: &Sessions, params: Value) -> Value {
        let launched = self
            .client()
            .await
            .call("agent_launch", params)
            .await
            .unwrap_or_else(|err| panic!("agent_launch: {err:?}"));
        track_agent(sessions, &self.tokens, &launched).await;
        launched
    }

    /// Asks agmend to shut down, then waits until it has exited.
    pub async fn stop(mut self) {
        let mut client = self.client().await;
        client
            .call("shutdown", json!({}))
            .await
            .unwrap_or_else(|err| panic!("shutdown: {err:?}"));
        let mut task = self.task.take().expect("the daemon task");
        if tokio::time::timeout(WAIT, &mut task).await.is_err() {
            task.abort();
            panic!("agmend did not exit within {WAIT:?}");
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Listens within 5 s. `start` returns only after this, so `client` can connect.
async fn wait_until_listening(socket: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(socket).await.is_err() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "agmend did not listen on {}",
            socket.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Records the `<id>-watch` session agmend opens for the agent that
/// `agent_launch` answered with `launched`, so [`run`](super::run) ends it.
/// The broker token comes from the agent's own token, which the launch
/// stored in `tokens`, while the test's hub still runs.
async fn track_agent(sessions: &Sessions, tokens: &TokenStore, launched: &Value) {
    let folder = launched["worktree"]
        .as_str()
        .unwrap_or_else(|| panic!("agent_launch returned {launched}"));
    let raw = std::fs::read(Path::new(folder).join(".agmen/identity.json"))
        .expect("agent_launch wrote the agent's identity file");
    let identity: IdentityFile = serde_json::from_slice(&raw).expect("a valid identity file");
    let token = tokens
        .get(&TokenKey::new(&identity.hub_url, &identity.identity_id).unwrap())
        .unwrap()
        .expect("agent_launch stored the agent's token");
    let hub = HubClient::new(
        identity.hub_url,
        identity.project,
        identity.identity_id,
        token,
    )
    .unwrap();
    let login = Login {
        identity: hub.identity().to_string(),
        token: hub.broker_token().await.unwrap(),
    };
    let client_id = format!("{}-{}", login.identity, ClientKind::Watch.suffix());
    sessions.0.lock().unwrap().insert(client_id, login);
}

/// One local API connection.
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
    next: u64,
}

impl Client {
    async fn connect(socket: &Path) -> Self {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(stream) = UnixStream::connect(socket).await {
                return Self { stream, next: 1 };
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "agmend is not listening at {}",
                socket.display()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Calls `method` and returns its result.
    ///
    /// # Errors
    ///
    /// The daemon's error, or a hub-coded error when the socket fails or
    /// the answer does not arrive within [`WAIT`].
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiError> {
        let id = self.next;
        self.next += 1;
        let request = Request::new(id, method, params);
        let response = tokio::time::timeout(WAIT, read_response(&mut self.stream, &request, id))
            .await
            .unwrap_or_else(|_| {
                Err(io_error(format!(
                    "agmend did not answer within {} s",
                    WAIT.as_secs()
                )))
            })?;
        match response.outcome {
            Outcome::Ok { result } => Ok(result),
            Outcome::Err { error } => Err(error),
        }
    }
}

async fn read_response(
    stream: &mut UnixStream,
    request: &Request,
    id: u64,
) -> Result<Response, ApiError> {
    frame::write_async(stream, request)
        .await
        .map_err(io_error)?;
    let response: Response = frame::read_async(stream).await.map_err(io_error)?;
    if response.id != id {
        return Err(io_error(format!(
            "response id {} does not match {id}",
            response.id
        )));
    }
    Ok(response)
}

fn io_error(error: impl std::fmt::Display) -> ApiError {
    ApiError {
        code: ErrorCode::Hub,
        message: error.to_string(),
    }
}
