//! `agmen mcp`: the stdio MCP server the Claude Code plugin's `.mcp.json`
//! starts, and OpenCode or Codex can (docs/mcp-setup.md). It acts as the
//! identity of the git worktree it runs in, through one persistent
//! `<id>-mcp` broker session (ADR-0002) whose outbox keeps sending what the
//! hub has not answered yet. That session publishes no presence (ruling
//! R65): `agmen watch` does.
//!
//! The identity is loaded when the server starts and, until one is
//! connected, again on each tool call. Each load runs the trust test on
//! `.agmen/identity.json` first (rulings R42 to R42b, agmen-client's
//! `find_identity`). Outside a joined worktree, with a file the test
//! refuses, or with `--identity` naming another identity, every tool
//! answers that it is not joined, as text and not as an MCP error, and no
//! token is read or sent and nothing connects. Claude Code starts this
//! server before the plugin's own trust check runs, so the test here is
//! the one that counts.
//!
//! While it runs, the server sends one HEARTBEAT every 5 minutes for the
//! claims `claims.db` holds, so their leases stay alive while the agent
//! edits without calling a tool. It connects at startup and, whenever no
//! session is connected (connecting failed, or the session stopped on its
//! own), again on the next beat, through the same path a tool call takes.
//!
//! Tool results are text: `recorded #<seq> (<stream>)`, `queued id=<id>
//! (not yet recorded)` or `rejected: <reason>` for what a tool sent, plus
//! what it read. Every hub- or peer-supplied string in them goes through
//! the sanitizer (ruling R63), since results reach the model.

mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agmen_client::{
    ClientError, ClientEvent, ClientKind, Events, IdentityError, Joined, Session, SessionConfig,
    TokenStore, find_identity,
};
use agmen_proto::IdentityId;
use anyhow::Context as _;
use clap::Args;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::service::ServerInitializeError;
use rmcp::transport::IntoTransport;
use rmcp::{RoleServer, ServiceExt as _};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::client::blocking;
use crate::cmd::claim::heartbeat_envelope;
use crate::joined::{Agent, Via, client_error};
use crate::output::{Code, Fail, NOT_JOINED, clean, clean_lines, explain};
use crate::paths;

/// How often the claims this identity holds get a HEARTBEAT.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5 * 60);
/// The shortest heartbeat period [`McpOptions::with_heartbeat`] takes.
const HEARTBEAT_MIN: Duration = Duration::from_secs(1);
/// The first wait before the heartbeat task tries to connect again after a
/// failure; it doubles up to the heartbeat period.
const RECONNECT_FIRST: Duration = Duration::from_secs(1);
/// How long a tool waits for the hub to record what it sent.
const ANSWER_WAIT: Duration = Duration::from_secs(5);

/// `agmen mcp`'s arguments.
#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    /// Act only as this identity id: when the worktree's identity file
    /// names another, every tool answers that it is not joined.
    #[arg(long, value_name = "ID")]
    identity: Option<IdentityId>,
}

/// `agmen mcp`: serves the MCP tools on stdin and stdout until the client
/// goes away.
pub(crate) async fn run(args: McpArgs) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let mut options = McpOptions::new(cwd, TokenStore::from_env(), paths::state_base()?);
    if let Some(identity) = args.identity {
        options = options.with_identity(identity);
    }
    serve(rmcp::transport::stdio(), options).await?;
    Ok(())
}

/// Where [`serve`] finds the identity it acts as, its token and its state,
/// and how often it does things.
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::time::Duration;
///
/// use agmen_cli::McpOptions;
/// use agmen_client::TokenStore;
///
/// let options = McpOptions::new(
///     "/work/repo",
///     TokenStore::Dir("/tmp/agmen-test/tokens".into()),
///     "/tmp/agmen-test/state",
/// )
/// .with_identity("idn7k2q9".parse()?)
/// .with_heartbeat(Duration::from_secs(60));
/// # drop(options);
/// # Ok(()) }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct McpOptions {
    cwd: PathBuf,
    tokens: TokenStore,
    state_base: PathBuf,
    identity: Option<IdentityId>,
    heartbeat: Duration,
    answer_wait: Duration,
    session_expiry: Option<Duration>,
}

impl McpOptions {
    /// Options for a server whose working directory is `cwd`, which reads
    /// identity tokens from `tokens`, and which keeps each identity's state
    /// in a folder named after it in `state_base`. It acts as whatever
    /// identity the worktree holds, sends a HEARTBEAT every 5 minutes, and
    /// waits up to 5 s for the hub to record what a tool sends.
    pub fn new(
        cwd: impl Into<PathBuf>,
        tokens: TokenStore,
        state_base: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            tokens,
            state_base: state_base.into(),
            identity: None,
            heartbeat: HEARTBEAT_EVERY,
            answer_wait: ANSWER_WAIT,
            session_expiry: None,
        }
    }

    /// Acts only as `identity`: when the worktree's identity file names
    /// another, every tool answers that it is not joined.
    pub fn with_identity(mut self, identity: IdentityId) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Sends the HEARTBEAT every `every` (at least 1 s) instead of every 5
    /// minutes.
    pub fn with_heartbeat(mut self, every: Duration) -> Self {
        self.heartbeat = every.max(HEARTBEAT_MIN);
        self
    }

    /// Waits up to `wait` for the hub to record what a tool sends; past it
    /// the tool answers `queued`.
    pub fn with_answer_wait(mut self, wait: Duration) -> Self {
        self.answer_wait = wait;
        self
    }

    /// Has the broker keep the `<id>-mcp` session for `expiry` after the
    /// server stops, instead of agmen-client's default of a day. Tests set
    /// zero, so they leave no session behind.
    pub fn with_session_expiry(mut self, expiry: Duration) -> Self {
        self.session_expiry = Some(expiry);
        self
    }
}

/// Why [`serve`] stopped with an error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// The client did not complete the MCP handshake.
    #[error("the MCP handshake failed")]
    Handshake(#[source] Box<ServerInitializeError>),
    /// The task that served the client failed.
    #[error("the MCP server stopped unexpectedly")]
    Stopped(#[source] tokio::task::JoinError),
}

/// Serves the agmen MCP tools over `transport` until the client goes away,
/// then closes the identity's broker session.
///
/// ```no_run
/// # async fn example() -> Result<(), agmen_cli::McpError> {
/// use agmen_cli::{McpOptions, serve};
/// use agmen_client::TokenStore;
///
/// let options = McpOptions::new("/work/repo", TokenStore::Keychain, "/home/andy/.local/state/agmen");
/// serve((tokio::io::stdin(), tokio::io::stdout()), options).await?;
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// [`McpError::Handshake`] when the client does not complete the MCP
/// handshake, and [`McpError::Stopped`] when serving fails.
pub async fn serve<T, E, A>(transport: T, options: McpOptions) -> Result<(), McpError>
where
    T: IntoTransport<RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let server = Server::new(options);
    // Connects now and keeps a session for the HEARTBEAT, so held claims
    // get it without a tool call.
    let heartbeat = Task::spawn(heartbeat(server.clone()));
    let served = match server.clone().serve(transport).await {
        Ok(running) => running.waiting().await.map(drop).map_err(McpError::Stopped),
        Err(err) => Err(McpError::Handshake(Box::new(err))),
    };
    heartbeat.stop().await;
    server.shut_down().await;
    served
}

/// The MCP server: the tools in `tools.rs`, and the identity they act as.
#[derive(Debug, Clone)]
pub(crate) struct Server {
    shared: Arc<Shared>,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug)]
struct Shared {
    options: McpOptions,
    /// The connected identity; `None` until one connects.
    slot: Mutex<Option<Connected>>,
}

/// The identity's broker session while it runs, and what the tools need.
#[derive(Debug)]
pub(crate) struct Live {
    pub(crate) agent: Agent,
    session: Session,
    /// Woken on every delivery the session reports.
    answered: Arc<Notify>,
    /// Set once the session has stopped on its own.
    stopped: Arc<AtomicBool>,
    wait: Duration,
}

impl Live {
    /// How the tools send: through this session.
    pub(crate) fn via(&self) -> Via<'_> {
        Via::Held {
            session: &self.session,
            answered: &self.answered,
            wait: self.wait,
        }
    }
}

/// A connected identity and the task that reads its session's events.
#[derive(Debug)]
struct Connected {
    live: Arc<Live>,
    pump: Task,
}

impl Connected {
    /// Stops the pump, then sends DISCONNECT, so the broker keeps the
    /// session for the next start.
    ///
    /// # Errors
    ///
    /// The error that stopped the session earlier, if one did.
    async fn close(self) -> Result<(), ClientError> {
        self.pump.stop().await;
        match Arc::try_unwrap(self.live) {
            Ok(live) => live.session.close().await,
            Err(_) => {
                // A tool call that outlived its client still holds it; the
                // session stops without DISCONNECT when that call ends.
                tracing::debug!("the broker session is still in use; it is dropped later");
                Ok(())
            }
        }
    }
}

/// A spawned task that is stopped when this is dropped.
#[derive(Debug)]
struct Task(Option<JoinHandle<()>>);

impl Task {
    fn spawn(work: impl Future<Output = ()> + Send + 'static) -> Self {
        Self(Some(tokio::spawn(work)))
    }

    /// Stops the task and waits until it has stopped.
    async fn stop(mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
            if let Err(err) = handle.await
                && err.is_panic()
            {
                tracing::error!(error = %err, "an agmen mcp task panicked");
            }
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }
}

impl Server {
    fn new(options: McpOptions) -> Self {
        Self {
            shared: Arc::new(Shared {
                options,
                slot: Mutex::new(None),
            }),
            tool_router: Self::tool_router(),
        }
    }

    /// Runs `work` with the connected identity and turns what it returns
    /// into the tool's result.
    pub(crate) async fn respond<F, Fut>(&self, work: F) -> CallToolResult
    where
        F: FnOnce(Arc<Live>) -> Fut,
        Fut: Future<Output = anyhow::Result<String>>,
    {
        match self.live().await {
            Ok(live) => match work(live).await {
                Ok(answer) => text(answer),
                Err(err) => failure(&err),
            },
            Err(answer) => answer,
        }
    }

    /// The working directory paths are relative to.
    pub(crate) fn cwd(&self) -> &std::path::Path {
        &self.shared.options.cwd
    }

    /// The connected identity, connecting first when none is. `Err` holds
    /// the tool's answer: not joined, or what failed.
    async fn live(&self) -> Result<Arc<Live>, CallToolResult> {
        let mut slot = self.shared.slot.lock().await;
        if let Some(connected) = slot.as_ref()
            && !connected.live.stopped.load(Ordering::SeqCst)
        {
            return Ok(connected.live.clone());
        }
        if let Some(stopped) = slot.take() {
            // The session stopped on its own: say why once, and connect
            // again on the next call.
            let err = match stopped.close().await {
                Err(err) => client_error(err, "the broker session stopped"),
                Ok(()) => anyhow::anyhow!("the broker session stopped"),
            };
            return Err(failure(&err));
        }
        let joined = self.trusted().await?;
        let connected = self.connect(joined).await.map_err(|err| failure(&err))?;
        let live = connected.live.clone();
        *slot = Some(connected);
        Ok(live)
    }

    /// The worktree's identity file, once it passed the trust test and
    /// names the identity `--identity` asks for.
    async fn trusted(&self) -> Result<Joined, CallToolResult> {
        let cwd = self.shared.options.cwd.clone();
        let found = blocking(move || Ok(find_identity(&cwd)))
            .await
            .map_err(|err| failure(&err))?;
        let joined = match found {
            Ok(Some(joined)) => joined,
            Ok(None) => return Err(text(NOT_JOINED)),
            Err(err @ IdentityError::Untrusted { .. }) => {
                tracing::warn!("{err}");
                return Err(text(NOT_JOINED));
            }
            Err(err) => {
                let err = anyhow::Error::new(err).context("loading .agmen/identity.json failed");
                return Err(failure(&err));
            }
        };
        let id = &joined.identity.identity_id;
        if let Some(want) = &self.shared.options.identity
            && want != id
        {
            return Err(text(format!(
                "not joined as {want}: this worktree's identity is {id}"
            )));
        }
        Ok(joined)
    }

    /// Loads the token of `joined`'s identity, connects its `<id>-mcp`
    /// session and starts the task that reads its events.
    async fn connect(&self, joined: Joined) -> anyhow::Result<Connected> {
        let options = &self.shared.options;
        let agent = Agent::load(joined, options.tokens.clone(), &options.state_base).await?;
        let mut config = SessionConfig::new(
            ClientKind::Mcp,
            agent.topics(),
            agent.hub.clone(),
            agent.identity.broker_url.clone(),
            agent.state_dir.clone(),
        );
        if let Some(expiry) = options.session_expiry {
            config = config.with_session_expiry(expiry);
        }
        let (session, events) = Session::connect(config)
            .await
            .map_err(|err| client_error(err, "connecting to the broker failed"))?;
        let answered = Arc::new(Notify::new());
        let stopped = Arc::new(AtomicBool::new(false));
        let pump = Task::spawn(pump(events, answered.clone(), stopped.clone()));
        let live = Arc::new(Live {
            agent,
            session,
            answered,
            stopped,
            wait: options.answer_wait,
        });
        Ok(Connected { live, pump })
    }

    /// Closes the connected identity's session, if one is connected.
    async fn shut_down(&self) {
        let connected = self.shared.slot.lock().await.take();
        if let Some(connected) = connected
            && let Err(err) = connected.close().await
        {
            tracing::warn!(error = %err, "the broker session had failed");
        }
    }
}

/// Reads the session's events: wakes the tools waiting for an answer on
/// every delivery, and marks the session stopped when they end.
async fn pump(mut events: Events, answered: Arc<Notify>, stopped: Arc<AtomicBool>) {
    while let Some(event) = events.recv().await {
        if matches!(event, ClientEvent::Delivery { .. }) {
            answered.notify_waiters();
        }
    }
    stopped.store(true, Ordering::SeqCst);
    answered.notify_waiters();
}

/// Connects at once, then every heartbeat period queues one HEARTBEAT
/// naming the claims `claims.db` holds, until the server stops. Each round
/// takes the session through [`Server::live`], as a tool call does, so a
/// session that failed to connect or stopped on its own is connected
/// again. After a failure it tries again sooner, waiting 1 s and doubling
/// up to the period. The first beat is due one period after startup and
/// each next one a period after the last; a round that connects after a
/// beat fell due sends it at once, so an outage delays a beat only until
/// the reconnect. Not joined waits a whole period. The outbox sends what
/// is queued; nothing waits for the hub's answer.
async fn heartbeat(server: Server) {
    let every = server.shared.options.heartbeat;
    let mut retry = RECONNECT_FIRST.min(every);
    let mut due = Instant::now() + every;
    loop {
        let wait = match server.live().await {
            Ok(live) => {
                if Instant::now() >= due {
                    send_heartbeat(&live).await;
                    due = Instant::now() + every;
                }
                retry = RECONNECT_FIRST.min(every);
                due.saturating_duration_since(Instant::now())
            }
            Err(answer) if answer.is_error == Some(true) => {
                tracing::warn!(
                    "agmen mcp has no broker session; trying again in {retry:?}: {}",
                    result_text(&answer)
                );
                let wait = retry;
                retry = retry.saturating_mul(2).min(every);
                wait
            }
            // Not joined: no news, and nothing to beat for.
            Err(_) => every,
        };
        tokio::time::sleep(wait).await;
    }
}

/// Queues one HEARTBEAT naming the claims `claims.db` holds, if it holds
/// any. A failure is logged; the next beat tries again.
async fn send_heartbeat(live: &Live) {
    let sent = match heartbeat_envelope(&live.agent).await {
        Ok(Some(env)) => live
            .session
            .publish(env)
            .await
            .context("queueing the HEARTBEAT failed"),
        Ok(None) => Ok(()),
        Err(err) => Err(err),
    };
    if let Err(err) = sent {
        tracing::warn!("{}", clean(&format!("{err:#}")));
    }
}

/// A tool result with `answer` as its text.
fn text(answer: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(answer)])
}

/// The result for a tool that failed. Rejected and queued are answers the
/// model acts on, so they are text; anything else is a tool error that
/// says what failed. Both go through the sanitizer.
fn failure(err: &anyhow::Error) -> CallToolResult {
    if let Some(fail) = err.downcast_ref::<Fail>()
        && matches!(fail.code, Code::Rejected | Code::Queued)
    {
        return text(clean_lines(&fail.message));
    }
    CallToolResult::error(vec![ContentBlock::text(explain(err).0)])
}

/// The text of `result`, for a log line.
fn result_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}
