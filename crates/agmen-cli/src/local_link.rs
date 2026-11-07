//! An identity's authenticated calls to the local origin through agmend.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agmen_client::{
    ClientEvent, Cursors, IdentityFile, Outbox, Page, STATE_DB, TokenStore, read_local_credential,
    retry_delay,
};
use agmen_local::ErrorCode;
use agmen_proto::{ClientEnvelope, DeliveryReply, IdentityId, ProjectId, Seq, Stream, Timestamp};
use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::agmend::{Agmend, AgmendError, LaunchCredential, LocalToken, Watch};
use crate::client::blocking;
use crate::cmd::lanes::{ItemList, LaneList, PathCheck};
use crate::joined::Recorded;
use crate::output::{Code, Fail};

const WATCH_CAPACITY: usize = 256;
const WATCH_MAX_EVENTS: usize = 1_000_000;
const WATCH_SEND_WAIT: Duration = Duration::from_secs(5);
const OUTBOX_POLL: Duration = Duration::from_millis(250);

/// What an agmend from before watch resume answers a `watch` that carries
/// `after`: its watch params are `{project}` with `deny_unknown_fields`, and
/// the console wraps serde's complaint as `bad_request` text
/// `bad params: {error}`. No other refusal means that agmend is older.
const OLDER_AGMEND_REFUSAL: &str = "bad params: unknown field `after`, expected `project`";
const OLDER_AGMEND_NOTICE: &str =
    "agmend is older and cannot resume a watch; resume is off until agmend restarts";

/// Whether this process has said that agmend is older, whichever link met it.
static OLDER_AGMEND_SAID: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
struct WatchResume {
    cursors: Option<Cursors>,
    positions: BTreeMap<Stream, u64>,
    /// Whether the open connection was asked to resume from `positions`. An
    /// agmend from before resume starts at the heads and replays nothing, so
    /// on its connection a message may skip ahead of `positions`.
    resumed: bool,
    said: &'static AtomicBool,
}

/// Where a local watch keeps its place across reconnects. agmend keeps none:
/// each watch starts after the seqs this side sends. Only `agmen watch` owns
/// the identity's durable delivery cursors (`agmen.db` in its state folder).
#[derive(Debug, Clone, Copy)]
pub(crate) enum WatchState<'a> {
    /// `agmen watch`: starts after the saved cursors in this state folder and
    /// saves each seq once the message is queued for output.
    Delivery(&'a Path),
    /// `agmen mcp`: wakes tools. Starts at the current heads and keeps its
    /// place in memory, so it never moves the delivery cursors.
    Notifications,
}

impl WatchResume {
    /// One line for the person: `agmen watch` writes it to stderr and keeps its
    /// stdout lines alone; `agmen mcp` logs it, as it does for an older agmend.
    fn notice(&self, message: &str) {
        if self.cursors.is_some() {
            // A broken stderr must not end the watch.
            let _printed = writeln!(std::io::stderr(), "agmen: {message}");
        } else {
            tracing::warn!("{message}");
        }
    }

    /// Records how a connection opened. The first connection in this process
    /// to an agmend that cannot resume gets one line: `agmen watch`, the only
    /// reader with delivery cursors, prints it to stderr and leaves its stdout
    /// lines alone; `agmen mcp` logs it and never answers a tool with it.
    /// Every reconnect asks to resume again, so resume returns once agmend
    /// restarts on a version that takes `after`.
    fn opened(&mut self, resumed: bool) {
        self.resumed = resumed;
        if resumed || self.said.swap(true, Ordering::Relaxed) {
            return;
        }
        if self.cursors.is_some() {
            // eprintln! panics on a broken pipe, and a notice must not end the watch.
            let _printed = writeln!(std::io::stderr(), "agmen: {OLDER_AGMEND_NOTICE}");
        } else {
            tracing::warn!("{OLDER_AGMEND_NOTICE}");
        }
    }

    async fn deliver(&mut self, tx: &mpsc::Sender<ClientEvent>, event: ClientEvent) -> bool {
        let position = if let ClientEvent::Message { message } = &event {
            let Some(last) = self.positions.get(&message.stream) else {
                tracing::warn!("local watch received another identity's stream");
                return false;
            };
            if message.seq.0 <= *last {
                return true;
            }
            if self.resumed && message.seq.0 != last.saturating_add(1) {
                tracing::warn!("local watch received a sequence gap");
                return false;
            }
            Some((message.stream.clone(), message.seq.0))
        } else {
            None
        };
        if !watch_send(tx, event).await {
            return false;
        }
        // Save after sending: a crash between the two repeats a message, never loses one.
        if let Some((stream, seq)) = position {
            if let Some(cursors) = self.cursors.clone() {
                let saved_stream = stream.clone();
                if let Err(error) = blocking(move || Ok(cursors.skip_to(&saved_stream, seq)?)).await
                {
                    tracing::warn!(%error, "saving local watch cursor failed");
                    return false;
                }
            }
            self.positions.insert(stream, seq);
        }
        true
    }
}

/// Credentials stay here and are never sent to a hub or broker.
#[derive(Debug)]
pub(crate) struct LocalLink {
    pub(crate) socket: PathBuf,
    identity: IdentityId,
    project: ProjectId,
    token: LocalToken,
    watch_task: Mutex<Option<JoinHandle<()>>>,
    failure: Arc<Mutex<Option<anyhow::Error>>>,
    said: &'static AtomicBool,
}

impl LocalLink {
    /// Uses a matching launch credential, otherwise the saved local credential.
    pub(crate) async fn load(identity: &IdentityFile, tokens: &TokenStore) -> Result<Self> {
        let launch = if std::env::var_os("AGMEN_IDENTITY").as_deref()
            == Some(std::ffi::OsStr::new(identity.identity_id.as_str()))
        {
            LaunchCredential::from_env()?
        } else {
            None
        };
        let token = match launch {
            Some(launch) => launch.token,
            None => {
                let (tokens, id) = (tokens.clone(), identity.identity_id.clone());
                let credential = blocking(move || Ok(read_local_credential(&tokens, &id)?))
                    .await?
                    .ok_or_else(|| {
                        Fail::new(
                            Code::NotLoggedIn,
                            format!("start agmend: no local token for {}", identity.handle),
                        )
                    })?;
                if credential.origin != "local" || credential.owner_id != identity.owner_id {
                    anyhow::bail!("the local identity credential does not match this worktree");
                }
                credential.local_token.parse()?
            }
        };
        Ok(Self {
            socket: agmen_client::agmend_socket()?,
            identity: identity.identity_id.clone(),
            project: identity.project.clone(),
            token,
            watch_task: Mutex::new(None),
            failure: Arc::new(Mutex::new(None)),
            said: &OLDER_AGMEND_SAID,
        })
    }

    /// Reads authenticated local events without opening a broker connection.
    /// Dropping the receiver or this link closes the watch and its presence.
    /// An agmend from before resume gets a watch without it, and one notice.
    pub(crate) async fn watch(&self, state: WatchState<'_>) -> Result<mpsc::Receiver<ClientEvent>> {
        let streams = [Stream::Feed, Stream::Inbox(self.identity.clone())];
        let mut resume = match state {
            WatchState::Delivery(dir) => {
                let (dir, said) = (dir.to_owned(), self.said);
                blocking(move || {
                    std::fs::create_dir_all(&dir).context("creating local watch state folder")?;
                    let cursors = Cursors::open(dir.join(STATE_DB))?;
                    let mut positions = BTreeMap::new();
                    for stream in streams {
                        positions.insert(stream.clone(), cursors.last(&stream)?);
                    }
                    Ok(WatchResume {
                        cursors: Some(cursors),
                        positions,
                        resumed: true,
                        said,
                    })
                })
                .await?
            }
            WatchState::Notifications => {
                let mut positions = BTreeMap::new();
                for stream in streams {
                    positions.insert(stream.clone(), self.page(&stream, 0, 1).await?.head);
                }
                WatchResume {
                    cursors: None,
                    positions,
                    resumed: true,
                    said: self.said,
                }
            }
        };
        // A first open that fails is the caller's to hear about: `agmen mcp`
        // connects while it holds its slot lock, so waiting here would hang
        // every tool call. Only a watch that was open and then lost keeps trying.
        let (watch, resumed) = open_watch(
            &self.socket,
            &self.identity,
            &self.project,
            &self.token,
            &resume.positions,
        )
        .await
        .map_err(|error| local_error(redact_error(error, &self.token)))?;
        resume.opened(resumed);
        let (tx, rx) = mpsc::channel(WATCH_CAPACITY);
        let (socket, identity, project, token) = (
            self.socket.clone(),
            self.identity.clone(),
            self.project.clone(),
            self.token.clone(),
        );
        let mut held = self
            .watch_task
            .lock()
            .map_err(|_| anyhow::anyhow!("local watch task lock failed"))?;
        if let Some(previous) = held.take() {
            previous.abort();
        }
        let failure = self.failure.clone();
        *failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *held = Some(tokio::spawn(async move {
            if let Err(error) =
                follow_watch(watch, &tx, socket, identity, project, token.clone(), resume).await
            {
                let error = watch_failure(redact_error(error, &token));
                *failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
            }
        }));
        Ok(rx)
    }

    /// The terminal reason is retained for watch and every subsequent MCP call.
    pub(crate) fn check_watch(&self) -> Result<()> {
        match &*self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            // An anyhow error is not Clone; a `Fail` is, and carries the exit code.
            Some(error) => Err(match error.downcast_ref::<Fail>() {
                Some(fail) => fail.clone().into(),
                None => anyhow::Error::msg(format!("{error:#}")),
            }),
            None => Ok(()),
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_raw(method, params).await.map_err(local_error)
    }

    async fn call_raw(&self, method: &str, params: Value) -> Result<Value, AgmendError> {
        let mut daemon = Agmend::connect_checked(&self.socket).await?;
        daemon
            .call_as(&self.identity, &self.token, method, params)
            .await
            .map_err(|error| redact_error(error, &self.token))
    }

    pub(crate) async fn post(&self, envelope: &ClientEnvelope) -> Result<Recorded> {
        posted(
            envelope,
            self.call("post", json!({"envelope": envelope})).await?,
        )
    }

    /// Sends the existing identity outbox while watch runs, with its usual backoff.
    // shortcut: looks every 250 ms for as long as the watch runs, though only the
    // HELLO is ever queued; sleep until the outbox's next due time once the client
    // crate makes `next_due` public.
    pub(crate) async fn send_outbox(&self, outbox: Outbox) -> Result<()> {
        loop {
            let db = outbox.clone();
            let due = blocking(move || Ok(db.due(Timestamp::now())?)).await?;
            for envelope in due {
                let answer = self.call_raw("post", json!({"envelope": envelope})).await;
                let (db, id) = (outbox.clone(), envelope.id.clone());
                match answer {
                    Ok(answer) => {
                        let recorded = posted(&envelope, answer)?;
                        let reply = DeliveryReply::Ack {
                            id,
                            stream: recorded.stream,
                            seq: Seq(recorded.seq),
                        };
                        blocking(move || {
                            db.apply(&reply)?;
                            Ok(())
                        })
                        .await?;
                    }
                    Err(error) if retryable(&error) => {
                        tracing::debug!(%error, "local outbox will retry");
                        blocking(move || {
                            db.sent(&id, Timestamp::now())?;
                            Ok(())
                        })
                        .await?;
                    }
                    Err(error) => return Err(watch_failure(error)),
                }
            }
            tokio::time::sleep(OUTBOX_POLL).await;
        }
    }

    pub(crate) async fn page(&self, stream: &Stream, after: u64, limit: u32) -> Result<Page> {
        serde_json::from_value(
            self.call(
                "page",
                json!({"stream": stream, "after": after, "limit": limit}),
            )
            .await?,
        )
        .context("agmend's page answer is invalid")
    }

    pub(crate) async fn state(&self, key: &str) -> Result<Value> {
        #[derive(Deserialize)]
        struct State {
            value: Value,
        }
        let answer: State = serde_json::from_value(
            self.call("state", json!({"project": self.project, "key": key}))
                .await?,
        )
        .context("agmend's state answer is invalid")?;
        Ok(answer.value)
    }

    pub(crate) async fn lanes(&self) -> Result<LaneList> {
        serde_json::from_value(self.call("lanes", json!({})).await?)
            .context("agmend's lanes answer is invalid")
    }

    pub(crate) async fn items(&self) -> Result<ItemList> {
        serde_json::from_value(self.call("items", json!({})).await?)
            .context("agmend's items answer is invalid")
    }

    pub(crate) async fn check_path(&self, paths: Vec<String>) -> Result<Vec<PathCheck>> {
        #[derive(Deserialize)]
        struct Checks {
            checks: Vec<PathCheck>,
        }
        let answer: Checks =
            serde_json::from_value(self.call("check_path", json!({"paths": paths})).await?)
                .context("agmend's check_path answer is invalid")?;
        Ok(answer.checks)
    }
}

#[cfg(test)]
impl LocalLink {
    /// A link to a fake agmend at `socket`, for tests whose token is not real.
    pub(crate) fn at(socket: PathBuf, identity: &IdentityFile, token: LocalToken) -> Self {
        Self {
            socket,
            identity: identity.identity_id.clone(),
            project: identity.project.clone(),
            token,
            watch_task: Mutex::new(None),
            failure: Arc::new(Mutex::new(None)),
            // Its own flag, so one test's notice does not silence another's.
            said: Box::leak(Box::new(AtomicBool::new(false))),
        }
    }
}

impl Drop for LocalLink {
    fn drop(&mut self) {
        let task = self
            .watch_task
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(task) = task.take() {
            task.abort();
        }
    }
}

/// Opens a watch that resumes after `after`, and says whether it does. An
/// agmend from before resume refuses `after`, and only that, with
/// [`OLDER_AGMEND_REFUSAL`]. The watch then opens again as it did before
/// resume: at the heads, and with no limit on silence, since that agmend
/// sends no keepalive.
async fn open_watch(
    socket: &std::path::Path,
    identity: &IdentityId,
    project: &ProjectId,
    token: &LocalToken,
    after: &BTreeMap<Stream, u64>,
) -> Result<(Watch, bool), AgmendError> {
    let resuming = json!({"project": project, "after": after});
    match Agmend::connect_checked(socket)
        .await?
        .watch_as(identity, token, resuming)
        .await
    {
        Ok(watch) => Ok((watch, true)),
        Err(AgmendError::Refused {
            code: ErrorCode::BadRequest,
            message,
        }) if message == OLDER_AGMEND_REFUSAL => {
            let watch = Agmend::connect_checked(socket)
                .await?
                .watch_as(identity, token, json!({"project": project}))
                .await?;
            Ok((watch.without_keepalive(), false))
        }
        Err(error) => Err(error),
    }
}

async fn watch_send(tx: &mpsc::Sender<ClientEvent>, event: ClientEvent) -> bool {
    matches!(
        tokio::time::timeout(WATCH_SEND_WAIT, tx.send(event)).await,
        Ok(Ok(()))
    )
}

/// Opens the watch again after a loss. It tries until it gets in, waiting as
/// the hub path does (1 s, doubling to 30 s), because an agmend that restarts
/// or upgrades can be gone a while and `positions` make the new watch replay
/// what was missed. It says so once when the wait starts and once when it
/// ends, not once per try. It returns only an error [`retryable`] does not
/// list.
async fn reconnect_watch(
    socket: &Path,
    identity: &IdentityId,
    project: &ProjectId,
    token: &LocalToken,
    resume: &mut WatchResume,
) -> Result<Watch, AgmendError> {
    resume.notice("waiting for agmend; the local watch will reconnect");
    let mut failures = 1_u32;
    loop {
        tokio::time::sleep(retry_delay(failures)).await;
        match open_watch(socket, identity, project, token, &resume.positions).await {
            Ok((watch, resumed)) => {
                resume.opened(resumed);
                resume.notice("reconnected to agmend");
                return Ok(watch);
            }
            Err(error) if retryable(&error) => {
                tracing::debug!(error = %redact_error(error, token), "local watch reconnect failed");
                failures = failures.saturating_add(1);
            }
            Err(error) => return Err(error),
        }
    }
}

async fn follow_watch(
    mut watch: Watch,
    tx: &mpsc::Sender<ClientEvent>,
    socket: PathBuf,
    identity: IdentityId,
    project: ProjectId,
    token: LocalToken,
    mut resume: WatchResume,
) -> Result<(), AgmendError> {
    if !watch_send(tx, ClientEvent::Connection { connected: true }).await {
        return Ok(());
    }
    for _ in 0..WATCH_MAX_EVENTS {
        let result = tokio::select! {
            () = tx.closed() => return Ok(()),
            result = watch.recv() => result,
        };
        match result {
            Ok(event) => {
                if !resume.deliver(tx, event).await {
                    return Ok(());
                }
                continue;
            }
            Err(error) if retryable(&error) => {
                tracing::debug!(error = %redact_error(error, &token), "local watch disconnected");
            }
            Err(error) => return Err(error),
        }
        // Drop a partially read frame before any reconnect, including a stalled one.
        drop(watch);
        if !watch_send(tx, ClientEvent::Connection { connected: false }).await {
            return Ok(());
        }
        watch = tokio::select! {
            () = tx.closed() => return Ok(()),
            result = reconnect_watch(&socket, &identity, &project, &token, &mut resume) => result?,
        };
        if !watch_send(tx, ClientEvent::Connection { connected: true }).await {
            return Ok(());
        }
    }
    tracing::warn!("local watch reached its event limit");
    Ok(())
}

fn posted(envelope: &ClientEnvelope, answer: Value) -> Result<Recorded> {
    #[derive(Deserialize)]
    struct Posted {
        stream: Stream,
        seq: u64,
    }
    let posted: Posted =
        serde_json::from_value(answer).context("agmend's post answer is invalid")?;
    Ok(Recorded {
        id: envelope.id.clone(),
        stream: posted.stream,
        seq: posted.seq,
    })
}

/// Whether a failed connect, call or watch is worth another try. An error not
/// listed here ends the watch, so a refusal that never clears is heard instead
/// of waited on.
fn retryable(error: &AgmendError) -> bool {
    match error {
        AgmendError::NotRunning
        | AgmendError::Timeout { .. }
        | AgmendError::Io(_)
        | AgmendError::Frame(_) => true,
        // agmend binds its socket and only then sets mode 0600; the mode is
        // checked again on every try, before anything is sent.
        AgmendError::Socket(agmen_client::AgmendSocketError::OpenMode { .. }) => true,
        AgmendError::Refused {
            code: ErrorCode::Hub | ErrorCode::Unknown,
            message,
        } => matches!(
            message.as_str(),
            "busy" | "local.db is busy; try again" | "the local origin has stopped"
        ),
        _ => false,
    }
}

/// What ends `agmen watch` or answers an `agmen mcp` tool when a watch stops
/// for good. A refused identity is the local twin of the hub path's
/// `client_error`: exit 3 and the same hint. Anything else keeps the exit code
/// [`local_error`] gives it.
fn watch_failure(error: AgmendError) -> anyhow::Error {
    match error {
        AgmendError::Refused {
            code: ErrorCode::Unauthorized | ErrorCode::Forbidden,
            message,
        } => Fail::new(
            Code::NotLoggedIn,
            format!(
                "agmend refused this worktree's identity ({message}); it may have been revoked: {}",
                crate::joined::REVOKED_HINT,
            ),
        )
        .into(),
        other => local_error(other),
    }
}

fn redact_error(error: AgmendError, token: &LocalToken) -> AgmendError {
    match error {
        AgmendError::Refused { code, message } => AgmendError::Refused {
            code,
            message: token.redact(&message),
        },
        other => other,
    }
}

/// Keep daemon messages intact; only a final protocol rejection exits 4.
fn local_error(error: AgmendError) -> anyhow::Error {
    match error {
        AgmendError::Refused {
            code: ErrorCode::Hub,
            message,
        } if message.starts_with("hub rejected identity message: ") => {
            Fail::rejected(message.trim_start_matches("hub rejected identity message: ")).into()
        }
        AgmendError::Refused {
            code: ErrorCode::Unauthorized | ErrorCode::Forbidden,
            message,
        } => Fail::new(Code::NotLoggedIn, message).into(),
        AgmendError::Refused { message, .. } => anyhow::Error::msg(message),
        other => other.into(),
    }
}

#[cfg(test)]
#[path = "local_link_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "local_link_older_tests.rs"]
mod older_tests;
