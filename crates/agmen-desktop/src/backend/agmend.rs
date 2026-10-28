//! [`AgmendBackend`]: the project chat and roster, live from agmend's local
//! socket, and since Task 8 the roles, worktrees, git facts and file texts
//! agmend reads from the person's checkout.
//!
//! Three threads share one [`Live`] state behind a mutex:
//!
//! - GPUI's main thread reads snapshots, subscribes and queues sends.
//! - The watch thread owns the `console_watch` connection. It reconnects
//!   with backoff (1, 2, 4 ... 30 s) and pushes each event to subscribers.
//! - The request thread owns a second connection and runs one job at a
//!   time: a snapshot once a watch is connected, a post, a file's text,
//!   or an older page of the chat (final review fix 1).
//!
//! Only `Send` data crosses threads: agmen-local and agmen-proto types and
//! [`Update`]s. The backend itself stays on the main thread as an
//! `Rc<dyn Backend>`.
//!
//! agmend's watch contract (its `feed.rs`): every watch opens with
//! `Hub { connected: false }`, then `Hub { connected: true }` once the hub
//! stream is up. A snapshot taken after that overlaps the stream and
//! misses nothing. So the backend asks for one then, keeps the watch
//! messages that arrive before the answer, and merges both by
//! `(stream, seq)`, which is also how a reconnect shows each message once.

use std::collections::{BTreeMap, VecDeque};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use agmen_local::console::{
    ConsoleEvent, ConsoleSnapshot, FileTextEntry, GitFactsEntry, LocalData, Me, RosterEntry, Status,
};
use agmen_local::{
    API, ApiError, ErrorCode, HelloAnswer, Outcome, ReadyAnswer, Request, Response, frame,
};
use agmen_proto::{
    Body, ClientEnvelope, IdentityId, IdentityKind, MAX_BODY_BYTES, Mention, MessageId, Payload,
    Seq, StampedEnvelope, Stream, Timestamp,
};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use gpui::SharedString;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::agmend_local;
use super::{
    Action, AgentRow, AgentStatus, Backend, BackendError, Draft, FileText, FileTextReply, GitFacts,
    Link, ListenerInfo, ManagedWork, Older, PersonGroup, Snapshot, StatusSummary, Update,
    WorktreeInfo,
};

/// The most messages held per project; the oldest go first (the plan's
/// Global Constraints).
pub(super) const MAX_MESSAGES: usize = 2000;
/// Posts waiting for the request thread. Past this a send is refused, so
/// the main thread never blocks on a slow agmend.
const MAX_QUEUED: usize = 32;
/// How long one request waits on agmend before the connection is dropped.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The deepest delegation chain the roster nests.
const MAX_DEPTH: u8 = 8;

/// The reconnect backoff: `first`, doubling up to `cap`.
#[derive(Debug, Clone, Copy)]
pub(super) struct Timing {
    pub(super) first: Duration,
    pub(super) cap: Duration,
}

/// 1, 2, 4 ... capped at 30 s (the plan's Global Constraints).
const BACKOFF: Timing = Timing {
    first: Duration::from_secs(1),
    cap: Duration::from_secs(30),
};

/// The daemon socket shared with Settings by the live window.
#[derive(Debug, Clone)]
pub(crate) struct HostSocket(pub(crate) PathBuf);
impl gpui::Global for HostSocket {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HostState {
    Running,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HostStatus {
    pub(crate) configured: bool,
    pub(crate) state: HostState,
    pub(crate) last_error: Option<String>,
    pub(crate) public_url: Option<String>,
}

/// The live backend. Dropping it ends the watch and both threads.
#[derive(Debug)]
pub(crate) struct AgmendBackend {
    hello: HelloAnswer,
    ready: ReadyAnswer,
    shared: Arc<Shared>,
    jobs: SyncSender<Job>,
    /// Dropped with the backend, which wakes the watch thread from its
    /// backoff wait so it can exit.
    _stop: Sender<()>,
}

#[derive(Debug)]
struct Shared {
    identity: HelloAnswer,
    socket: PathBuf,
    project: String,
    /// Task 8: the person's checkout of the project, which agmend reads
    /// worktrees, git facts and project roles from.
    root: Option<PathBuf>,
    live: Mutex<Live>,
}

/// `console_snapshot` and `console_watch` params: the project, and the
/// checkout when the window found one.
fn checkout_params(project: &str, root: Option<&Path>) -> Value {
    match root {
        Some(root) => json!({ "project": project, "root": root }),
        None => json!({ "project": project }),
    }
}

/// Task 8: what agmend read from disk, taken out of a snapshot.
fn take_local(console: &mut ConsoleSnapshot) -> LocalData {
    LocalData {
        roles: std::mem::take(&mut console.roles),
        worktrees: std::mem::take(&mut console.worktrees),
        git: std::mem::take(&mut console.git),
    }
}

impl Shared {
    fn checkout_params(&self) -> Value {
        checkout_params(&self.project, self.root.as_deref())
    }

    fn lock(&self) -> MutexGuard<'_, Live> {
        // A panic while the lock was held leaves plain data, still readable.
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What the backend knows now, and who hears about changes.
#[derive(Debug)]
struct Live {
    project: String,
    managed: Vec<ManagedAgent>,
    agent_error: Option<String>,
    agent_read_error: Option<String>,
    me: Me,
    roster: Vec<RosterEntry>,
    local: LocalData,
    held: Held,
    /// Final review fix 1: the older page on its way, or why it failed.
    paging: Paging,
    link: Link,
    /// Watch messages that came before the pending snapshot's answer, at
    /// most [`MAX_MESSAGES`] (more would be trimmed anyway); `None` while
    /// no snapshot is pending.
    early: Option<VecDeque<StampedEnvelope>>,
    /// Bumped on each `Hub` event, so a snapshot answer for an earlier
    /// state of the watch is dropped.
    generation: u64,
    /// Whether the current watch reached [`Link::Live`], which resets the
    /// backoff.
    went_live: bool,
    /// The open watch connection, so dropping the backend or a failed
    /// snapshot can end it.
    watch: Option<UnixStream>,
    /// Why the request thread ended the watch, when it did.
    ended: Option<Link>,
    /// Set when the backend is dropped.
    closed: bool,
    subscribers: Vec<UnboundedSender<Update>>,
}

impl Live {
    fn snapshot(&self) -> Snapshot {
        let mut snapshot = snapshot_from(ConsoleSnapshot {
            project: self.project.clone(),
            me: self.me.clone(),
            roster: self.roster.clone(),
            messages: self.held.to_vec(),
            older: self.held.older,
            roles: self.local.roles.clone(),
            worktrees: self.local.worktrees.clone(),
            git: self.local.git.clone(),
        });
        // Final review fix 1: what paging back is doing.
        snapshot.older = match (&self.paging, self.held.older) {
            (Paging::Loading, _) => Older::Loading,
            (_, false) => Older::Start,
            (Paging::Failed(why), true) => Older::Failed(why.clone()),
            (Paging::Idle, true) => Older::More,
        };
        snapshot.agent_error = self
            .agent_error
            .clone()
            .or_else(|| self.agent_read_error.clone());
        if self
            .managed
            .iter()
            .any(|agent| agent.project == self.project)
            && !snapshot.roster.iter().any(|group| group.you)
        {
            snapshot.roster.push(PersonGroup {
                id: self.me.id.clone(),
                person: self.me.handle.clone(),
                you: true,
                agents: Vec::new(),
            });
        }
        for agent in &self.managed {
            if agent.project != self.project {
                continue;
            }
            snapshot
                .managed_agents
                .insert(agent.identity.clone(), agent.state.label().into());
            snapshot.managed_work.insert(
                agent.identity.clone(),
                ManagedWork {
                    worktree: agent.worktree.clone(),
                    branch: agent.branch.clone().filter(|branch| !branch.is_empty()),
                    joined: agent.source.as_deref() == Some("joined"),
                },
            );
            if let Some(group) = snapshot.roster.iter_mut().find(|group| group.you) {
                let status = match agent.state {
                    // Someone works in it interactively.
                    ManagedState::Running | ManagedState::Attached => AgentStatus::Working,
                    ManagedState::Idle => AgentStatus::Idle,
                    ManagedState::Offline => AgentStatus::Offline,
                };
                if let Some(row) = group.agents.iter_mut().find(|row| row.id == agent.identity) {
                    row.status = status;
                } else {
                    group.agents.push(AgentRow {
                        id: agent.identity.clone(),
                        handle: agent.handle.clone(),
                        name: agent
                            .handle
                            .rsplit('/')
                            .next()
                            .unwrap_or(&agent.handle)
                            .to_owned(),
                        role: "engineer".into(),
                        harness: "claude".into(),
                        status,
                        depth: 1,
                        item: None,
                    });
                }
            }
        }
        snapshot.newer = self.held.newer;
        snapshot.waiting = self.held.waiting;
        snapshot
    }

    // shortcut: subscribers' channels are unbounded, as the Backend trait
    // returns them (ADR-0011). The main thread drains them every frame;
    // bound them if a stalled window ever grows one.
    fn publish(&mut self, update: &Update) {
        // A dropped receiver means that view is gone; forget its sender.
        self.subscribers
            .retain(|tx| tx.unbounded_send(update.clone()).is_ok());
    }

    fn set_link(&mut self, link: Link) {
        if self.link != link {
            self.link = link.clone();
            self.publish(&Update::Link(link));
        }
    }

    fn publish_snapshot(&mut self) {
        let snapshot = self.snapshot();
        self.publish(&Update::Snapshot(Box::new(snapshot)));
    }

    /// Adds one watch message and tells subscribers what changed.
    fn hear(&mut self, env: StampedEnvelope) {
        match self.held.insert(env.clone()) {
            Added::New => self.publish(&Update::Message(Box::new(env))),
            // A full snapshot, so the views drop the oldest message too and
            // stay at the cap.
            Added::Trimmed => self.publish_snapshot(),
            // Not held, but the view still hears it, for its alerts.
            Added::Waiting => self.publish(&Update::Waiting {
                message: Box::new(env),
                waiting: self.held.waiting,
            }),
            Added::Nothing => {}
        }
    }
}

/// The chat as held: at most [`MAX_MESSAGES`], keyed and ordered by
/// `(stream, seq)`. The console is the project feed, whose seqs are the
/// chat's order.
///
/// Final review fix 1: the held messages are always one run with no hole.
/// New messages push the oldest out. Paging back pushes the newest out
/// instead, since the person is reading the page they scrolled to; the
/// chat then stops taking new messages (`newer`) until the person goes back
/// to the newest page, which replaces the whole run.
#[derive(Debug, Default)]
pub(super) struct Held {
    messages: BTreeMap<(Stream, Seq), StampedEnvelope>,
    /// agmend has messages before the first one held.
    pub(super) older: bool,
    /// Paging back let messages after the last one held go.
    pub(super) newer: bool,
    /// Live messages heard since `newer` was set, none of them held.
    pub(super) waiting: usize,
    /// The newest of those, so a redelivery is not counted twice.
    waiting_last: Option<(Stream, Seq)>,
}

/// What [`Held::insert`] did.
#[derive(Debug, PartialEq, Eq)]
enum Added {
    New,
    /// Added, and the oldest message dropped to stay at the cap.
    Trimmed,
    /// Held already, older than everything kept at the cap, or newer than
    /// a chat that paged back past the cap.
    Nothing,
    /// A new message the chat does not hold, since it reads back past the
    /// cap; the view alerts on it and counts it.
    Waiting,
}

impl Held {
    /// Adds one message the watch heard.
    fn insert(&mut self, env: StampedEnvelope) -> Added {
        let key = (env.stream.clone(), env.seq);
        if self.newer {
            // Held messages and ones already counted are not new. The rest
            // wait for the jump to the newest page, but still reach the
            // alerts, and are counted for the view.
            let last = self.messages.last_key_value().map(|(held, _)| held);
            let seen = last.is_some_and(|last| key <= *last)
                || self.waiting_last.as_ref().is_some_and(|last| key <= *last);
            if seen {
                return Added::Nothing;
            }
            self.waiting = self.waiting.saturating_add(1);
            self.waiting_last = Some(key);
            return Added::Waiting;
        }
        if self.messages.contains_key(&key) {
            return Added::Nothing;
        }
        self.messages.insert(key.clone(), env);
        if self.messages.len() <= MAX_MESSAGES {
            return Added::New;
        }
        self.older = true;
        match self.messages.pop_first() {
            Some((oldest, _)) if oldest == key => Added::Nothing,
            _ => Added::Trimmed,
        }
    }

    /// Takes a fresh snapshot's page, the newest messages, and whether
    /// older ones exist. When the page starts past the newest held message,
    /// the messages between are missing, so the old page goes rather than
    /// leave a silent hole in the chat; a chat that paged back past the cap
    /// goes the same way.
    pub(super) fn merge(&mut self, fresh: Vec<StampedEnvelope>, older: bool) {
        let gap = match (self.messages.last_key_value(), fresh.first()) {
            (Some(((stream, newest), _)), Some(first)) => {
                *stream == first.stream && first.seq.0 > newest.0.saturating_add(1)
            }
            _ => false,
        };
        let starts_earlier = match (self.messages.first_key_value(), fresh.first()) {
            (Some((oldest, _)), Some(first)) => (first.stream.clone(), first.seq) < *oldest,
            (None, _) => true,
            (Some(_), None) => false,
        };
        if gap || self.newer {
            self.messages.clear();
            self.newer = false;
            self.waiting = 0;
            self.waiting_last = None;
        }
        if self.messages.is_empty() || starts_earlier {
            self.older = older;
        }
        for env in fresh {
            self.insert(env);
        }
    }

    /// Takes the `chat_history` page asked for before seq `before`, and
    /// whether older ones exist; `false` when the chat no longer starts at
    /// `before`, so the page would leave a hole. Past the cap the newest
    /// messages go.
    pub(super) fn prepend(&mut self, before: Seq, page: Vec<StampedEnvelope>, older: bool) -> bool {
        if self.first_seq() != Some(before) {
            return false;
        }
        for env in page.into_iter().filter(|env| env.seq < before) {
            self.messages
                .entry((env.stream.clone(), env.seq))
                .or_insert(env);
        }
        self.older = older;
        while self.messages.len() > MAX_MESSAGES {
            self.messages.pop_last();
            self.newer = true;
        }
        true
    }

    /// The seq of the oldest message held.
    fn first_seq(&self) -> Option<Seq> {
        self.messages.first_key_value().map(|((_, seq), _)| *seq)
    }

    pub(super) fn to_vec(&self) -> Vec<StampedEnvelope> {
        self.messages.values().cloned().collect()
    }

    fn find(&self, id: &MessageId) -> Option<&StampedEnvelope> {
        self.messages.values().find(|m| &m.client.id == id)
    }
}

/// Final review fix 1: what paging back is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Paging {
    Idle,
    /// A `chat_history` request is on the request thread.
    Loading,
    /// The last one failed, for this reason.
    Failed(String),
}

/// The largest `chat_history` page agmend serves (the plan's Global
/// Constraints).
const PAGE: u32 = 500;

/// A job for the request thread.
#[derive(Debug)]
enum Job {
    Agent {
        method: &'static str,
        params: Value,
    },
    /// A snapshot for the watch in the given generation.
    Snapshot {
        generation: u64,
    },
    Post {
        draft: Draft,
        envelope: Box<ClientEnvelope>,
    },
    /// Final review fix 1: the page before the oldest held message.
    History {
        before: Seq,
    },
    /// Task 8: one file's text, for a file view that opened.
    FileText {
        worktree: String,
        path: String,
        reply: oneshot::Sender<Result<FileText, String>>,
    },
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct ManagedAgent {
    identity: IdentityId,
    project: String,
    handle: String,
    state: ManagedState,
    #[serde(default)]
    worktree: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    /// `launched` or `joined`; older daemons leave it out.
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum ManagedState {
    Running,
    Idle,
    Offline,
    /// An interactive Claude Code holds the agent's owner session.
    Attached,
}
impl ManagedState {
    const fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Offline => "offline",
            Self::Attached => "attached",
        }
    }
}

impl AgmendBackend {
    /// An agent action's params: the agent, this project, and the folder
    /// of an agent joined by hand when there is one.
    fn agent_target(&self, identity: &IdentityId, root: Option<std::path::PathBuf>) -> Value {
        let mut params = json!({"identity": identity, "project": self.shared.project});
        if let Some(root) = root {
            params["root"] = json!(root);
        }
        params
    }

    fn queue_agent(&self, method: &'static str, params: Value) -> Result<(), BackendError> {
        self.jobs
            .try_send(Job::Agent { method, params })
            .map_err(|error| match error {
                TrySendError::Full(_) => BackendError::Agmend("agent request queue is full".into()),
                TrySendError::Disconnected(_) => BackendError::NotConnected,
            })
    }

    pub(crate) fn shutdown_owned(
        socket: &Path,
        exe: &Path,
        bound: Duration,
    ) -> Result<(), BackendError> {
        let deadline = std::time::Instant::now() + bound.min(Duration::from_secs(15));
        let mut conn = match Conn::connect(socket) {
            Ok(conn) => conn,
            Err(BackendError::NotConnected) => return Ok(()),
            Err(error) => return Err(error),
        };
        let timeout = bound
            .min(Duration::from_secs(2))
            .max(Duration::from_millis(1));
        conn.stream
            .set_read_timeout(Some(timeout))
            .map_err(io_failed)?;
        conn.stream
            .set_write_timeout(Some(timeout))
            .map_err(io_failed)?;
        let hello: HelloAnswer = parse(conn.call("hello", json!({"api": API}))?)?;
        if hello.exe.as_deref() != Some(exe)
            || hello.version.as_deref() != Some(env!("CARGO_PKG_VERSION"))
            || hello.api != API
        {
            return Err(BackendError::Agmend(
                "agmend ownership changed; keeping it running".into(),
            ));
        }
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or_else(|| BackendError::Agmend("shutdown deadline expired".into()))?;
        conn.stream
            .set_read_timeout(Some(remaining))
            .map_err(io_failed)?;
        conn.stream
            .set_write_timeout(Some(remaining))
            .map_err(io_failed)?;
        conn.call("shutdown", json!({}))?;
        for _ in 0..150 {
            if !socket.exists() {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(
                Duration::from_millis(100)
                    .min(deadline.saturating_duration_since(std::time::Instant::now())),
            );
        }
        Err(BackendError::Agmend(
            "agmend did not exit within 15 s".into(),
        ))
    }

    /// Reads identity before enforcing compatibility, for the setup screen.
    pub(crate) fn probe(socket: &Path) -> Result<(HelloAnswer, Option<ReadyAnswer>), BackendError> {
        let mut conn = Conn::connect(socket)?;
        conn.stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(io_failed)?;
        conn.stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(io_failed)?;
        let hello = parse::<HelloAnswer>(conn.call("hello", json!({"api": API}))?)?;
        let ready =
            if hello.version.as_deref() == Some(env!("CARGO_PKG_VERSION")) && hello.api == API {
                Some(parse::<ReadyAnswer>(conn.call("ready", json!({}))?)?)
            } else {
                None
            };
        Ok((hello, ready))
    }

    /// Reads host status, or requests a restart, without contacting the hub.
    pub(crate) fn host_status(socket: &Path, restart: bool) -> Result<HostStatus, BackendError> {
        Self::host_status_before(
            socket,
            restart,
            std::time::Instant::now() + Duration::from_secs(2),
        )
    }

    pub(crate) fn host_status_before(
        socket: &Path,
        restart: bool,
        deadline: std::time::Instant,
    ) -> Result<HostStatus, BackendError> {
        let mut conn = Conn::open_until(socket, deadline)?;
        parse(conn.call(
            if restart {
                "host_restart"
            } else {
                "host_status"
            },
            json!({}),
        )?)
    }
    pub(crate) fn identity(&self) -> &HelloAnswer {
        &self.hello
    }

    pub(crate) fn readiness(&self) -> &ReadyAnswer {
        &self.ready
    }
    /// Connects to agmend at `socket` and opens `project`'s chat, with the
    /// local data of the checkout at `root` when the window found one.
    ///
    /// # Errors
    ///
    /// [`BackendError::NotConnected`] when nothing answers at `socket`, and
    /// [`BackendError::Agmend`] when the socket is not the person's own or
    /// agmend refuses the project, such as when the person is signed out.
    pub(crate) fn connect(
        socket: &Path,
        project: &str,
        root: Option<&Path>,
    ) -> Result<Self, BackendError> {
        Self::connect_with(socket, project, root, BACKOFF)
    }

    pub(super) fn connect_with(
        socket: &Path,
        project: &str,
        root: Option<&Path>,
        timing: Timing,
    ) -> Result<Self, BackendError> {
        Self::connect_before(socket, project, root, timing, None)
    }

    /// Bounds the first snapshot while a locally hosted hub is starting.
    fn connect_before(
        socket: &Path,
        project: &str,
        root: Option<&Path>,
        timing: Timing,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self, BackendError> {
        let mut conn = match deadline {
            Some(deadline) => Conn::open_until(socket, deadline)?,
            None => Conn::open(socket)?,
        };
        let hello = conn
            .hello
            .clone()
            .ok_or_else(|| BackendError::Agmend("missing daemon identity".into()))?;
        let ready = conn
            .ready
            .clone()
            .ok_or_else(|| BackendError::Agmend("missing daemon readiness".into()))?;
        if !ready.ready {
            return Err(BackendError::Agmend(
                "agmend stores are not ready; retry after startup".into(),
            ));
        }
        let mut console = match conn.call("console_snapshot", checkout_params(project, root)) {
            Ok(value) => parse::<ConsoleSnapshot>(value)?,
            // agmend is up but the hub is not: open on who the person is,
            // and the watch fills the chat in once the hub is back.
            Err(Fail::Refused(error)) if error.code == ErrorCode::Hub && deadline.is_none() => {
                let me = parse::<Me>(conn.call("whoami", Value::Null)?)?;
                ConsoleSnapshot {
                    project: project.to_owned(),
                    me,
                    roster: Vec::new(),
                    messages: Vec::new(),
                    older: false,
                    roles: Vec::new(),
                    worktrees: Vec::new(),
                    git: GitFactsEntry::default(),
                }
            }
            Err(fail) => return Err(fail.into()),
        };
        let local = take_local(&mut console);
        let mut held = Held::default();
        held.merge(console.messages, console.older);
        let shared = Arc::new(Shared {
            identity: hello.clone(),
            socket: socket.to_owned(),
            project: project.to_owned(),
            root: root.map(Path::to_owned),
            live: Mutex::new(Live {
                project: console.project,
                me: console.me,
                roster: console.roster,
                managed: Vec::new(),
                agent_error: None,
                agent_read_error: None,
                local,
                held,
                paging: Paging::Idle,
                link: Link::HubDown { why: None },
                early: None,
                generation: 0,
                went_live: false,
                watch: None,
                ended: None,
                closed: false,
                subscribers: Vec::new(),
            }),
        });
        let (jobs, queue) = mpsc::sync_channel(MAX_QUEUED);
        let (stop, stopped) = mpsc::channel();
        let spawn_failed = |err: std::io::Error| {
            BackendError::Agmend(format!("starting the agmend link failed: {err}"))
        };
        let requests = shared.clone();
        conn.clear_deadline()?;
        std::thread::Builder::new()
            .name("agmend-requests".into())
            .spawn(move || serve_jobs(&requests, &queue, Some(conn)))
            .map_err(spawn_failed)?;
        let watcher = shared.clone();
        let watch_jobs = jobs.clone();
        std::thread::Builder::new()
            .name("agmend-watch".into())
            .spawn(move || watch_loop(&watcher, &watch_jobs, &stopped, timing))
            .map_err(spawn_failed)?;
        Ok(Self {
            hello,
            ready,
            shared,
            jobs,
            _stop: stop,
        })
    }

    pub(crate) fn connect_host(
        socket: &Path,
        project: &str,
        root: Option<&Path>,
        deadline: std::time::Instant,
    ) -> Result<Self, BackendError> {
        Self::connect_before(socket, project, root, BACKOFF, Some(deadline))
    }
}

impl Drop for AgmendBackend {
    fn drop(&mut self) {
        let mut live = self.shared.lock();
        live.closed = true;
        if let Some(watch) = live.watch.take() {
            // Unblocks the watch thread's read; a socket already gone is
            // the same outcome.
            drop(watch.shutdown(std::net::Shutdown::Both));
        }
    }
}

impl Backend for AgmendBackend {
    fn snapshot(&self) -> Snapshot {
        self.shared.lock().snapshot()
    }

    fn send(&self, draft: Draft) -> Result<(), BackendError> {
        if draft.body.len() > MAX_BODY_BYTES {
            return Err(BackendError::BodyTooLarge {
                len: draft.body.len(),
                max: MAX_BODY_BYTES,
            });
        }
        if draft.model.is_some() {
            // The wire has no model field; agmend does not report one yet.
            return Err(BackendError::Unsupported(
                "running one message on a chosen model",
            ));
        }
        let envelope = {
            let live = self.shared.lock();
            if live.link != Link::Live {
                return Err(BackendError::NotConnected);
            }
            envelope_for(&draft, live.held.messages.values())?
        };
        let job = Job::Post {
            draft,
            envelope: Box::new(envelope),
        };
        self.jobs.try_send(job).map_err(|err| match err {
            TrySendError::Full(_) => BackendError::Agmend(format!(
                "{MAX_QUEUED} messages are still waiting to send; try again in a moment"
            )),
            TrySendError::Disconnected(_) => BackendError::NotConnected,
        })
    }

    fn act(&self, action: Action) -> Result<(), BackendError> {
        let unsupported = |what| Err(BackendError::Unsupported(what));
        match action {
            Action::LaunchAgent {
                name,
                model,
                budget_cents,
            } => {
                let mut params = json!({"project": self.shared.project, "name": name, "model": model, "budget_usd": f64::from(budget_cents) / 100.0});
                if let Some(root) = &self.shared.root {
                    params["root"] = json!(root);
                }
                self.queue_agent("agent_launch", params)
            }
            Action::StopAgent(identity) => {
                self.queue_agent("agent_stop", json!({"identity": identity}))
            }
            Action::StartAgent { identity, root } => {
                self.queue_agent("agent_start", self.agent_target(&identity, root))
            }
            Action::AttachAgent { identity, root } => {
                self.queue_agent("agent_attach", self.agent_target(&identity, root))
            }
            Action::DetachAgent(identity) => {
                self.queue_agent("agent_detach", json!({"identity": identity}))
            }
            Action::RemoveAgent { identity, root } => {
                self.queue_agent("agent_remove", self.agent_target(&identity, root))
            }
            // As in the fixture: an alert's action is an answer to its sender.
            Action::AnswerAlert { alert, choice } => {
                let sender = self
                    .shared
                    .lock()
                    .held
                    .find(&alert)
                    .map(|m| m.from.clone())
                    .ok_or(BackendError::Unsupported(
                        "answering an alert the chat lacks",
                    ))?;
                self.send(Draft {
                    to: vec![sender],
                    body: choice,
                    reply_to: Some(alert),
                    model: None,
                })
            }
            // Snoozing is the window's own state (Task 13); nothing to send.
            Action::SnoozeAlert(_) => Ok(()),
            Action::Apply(_) => unsupported("applying a settings change"),
            Action::Cancel(_) => unsupported("cancelling a settings change"),
            Action::Undo(_) => unsupported("undoing a settings change"),
            Action::TakeOver(_) => unsupported("taking over an agent"),
            Action::HandBack(_) => unsupported("handing an agent back"),
            Action::SwitchAgent { .. } => unsupported("switching an agent"),
            Action::ScanAgents => unsupported("looking for terminal agents"),
            Action::ApproveSpec { .. } => unsupported("approving a spec"),
            Action::ApproveShare => unsupported("approving your share"),
            Action::KickOff => unsupported("kicking off a plan"),
            Action::SpawnWorker { .. } => unsupported("starting a worker"),
        }
    }

    fn subscribe(&self) -> UnboundedReceiver<Update> {
        let (tx, rx) = unbounded();
        let mut live = self.shared.lock();
        // The receiver is still here, so this send cannot fail.
        if tx.unbounded_send(Update::Link(live.link.clone())).is_ok() {
            live.subscribers.push(tx);
        }
        rx
    }

    fn load_older(&self) {
        let mut live = self.shared.lock();
        if live.paging == Paging::Loading || !live.held.older {
            return;
        }
        let Some(before) = live.held.first_seq() else {
            return;
        };
        // Never blocks, so holding the lock is fine.
        live.paging = match self.jobs.try_send(Job::History { before }) {
            Ok(()) => Paging::Loading,
            Err(TrySendError::Full(_)) => {
                Paging::Failed("agmend is busy; scroll up again in a moment".to_owned())
            }
            Err(TrySendError::Disconnected(_)) => {
                Paging::Failed("not connected to agmend".to_owned())
            }
        };
        live.publish_snapshot();
    }

    fn load_newest(&self) {
        let mut live = self.shared.lock();
        // A snapshot already pending brings the newest page too.
        if !live.held.newer || live.link != Link::Live || live.early.is_some() {
            return;
        }
        // The same path a watch takes once the hub is up: watch messages
        // from now wait for the answer, which replaces the paged-back run.
        live.generation += 1;
        let generation = live.generation;
        if self.jobs.try_send(Job::Snapshot { generation }).is_ok() {
            live.early = Some(VecDeque::new());
        }
    }

    fn file_text(&self, worktree: &WorktreeInfo, path: &str) -> Option<FileTextReply> {
        // Only a worktree agmend listed has a folder to read in.
        let folder = worktree.root.clone()?;
        let (reply, answer) = oneshot::channel();
        let job = Job::FileText {
            worktree: folder,
            path: path.to_owned(),
            reply,
        };
        if let Err(err) = self.jobs.try_send(job) {
            let why = match &err {
                TrySendError::Full(_) => "agmend is busy; open the file again in a moment",
                TrySendError::Disconnected(_) => "not connected to agmend",
            };
            if let TrySendError::Full(Job::FileText { reply, .. })
            | TrySendError::Disconnected(Job::FileText { reply, .. }) = err
            {
                // The view may have closed; nothing then waits.
                drop(reply.send(Err(why.to_owned())));
            }
        }
        Some(answer)
    }
}

// The watch thread.

fn watch_loop(shared: &Shared, jobs: &SyncSender<Job>, stop: &Receiver<()>, timing: Timing) {
    let mut delay = timing.first;
    loop {
        let link = watch_once(shared, jobs);
        {
            let mut live = shared.lock();
            if live.closed {
                return;
            }
            live.watch = None;
            live.early = None;
            if std::mem::take(&mut live.went_live) {
                delay = timing.first;
            }
            // A snapshot that failed ended the watch, and says which link.
            let link = live.ended.take().unwrap_or(link);
            live.set_link(link);
        }
        match stop.recv_timeout(delay) {
            Err(RecvTimeoutError::Timeout) => {}
            // The backend was dropped.
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
        delay = next_delay(delay, timing);
    }
}

/// The wait after `delay`: double it, up to the cap.
fn next_delay(delay: Duration, timing: Timing) -> Duration {
    delay.saturating_mul(2).min(timing.cap)
}

/// Runs one watch until it ends, and says why as the link to show until
/// the next try: [`Link::Refused`] when agmend answered with an error,
/// such as a person signed out or a console that fell behind, and
/// [`Link::AgmendDown`] when the socket failed (final review fix 2).
fn watch_once(shared: &Shared, jobs: &SyncSender<Job>) -> Link {
    let down = |why: String| Link::AgmendDown { why };
    let mut stream = match open_watch(shared) {
        Ok(Some(stream)) => stream,
        Ok(None) => return down("the window closed".to_owned()),
        Err(BackendError::NotConnected) => return down("nothing answers at its socket".to_owned()),
        Err(err) => return down(err.to_string()),
    };
    loop {
        let response: Response = match frame::read(&mut stream) {
            Ok(response) => response,
            Err(err) => return down(format!("agmend's watch ended: {err}")),
        };
        let value = match response.outcome {
            Outcome::Ok { result } => result,
            // A refused or lagging watch: agmend closes it after this.
            Outcome::Err { error } => {
                return Link::Refused { why: error.message };
            }
        };
        // An event a newer agmend added; the rest of the watch still holds.
        let Ok(event) = serde_json::from_value::<ConsoleEvent>(value) else {
            continue;
        };
        // Only the request thread stopping fails here: this app's side.
        if let Err(why) = on_event(shared, jobs, event) {
            return down(why);
        }
    }
}

/// A greeted connection that now streams `project`'s events, or `None`
/// when the backend was dropped meanwhile.
fn open_watch(shared: &Shared) -> Result<Option<UnixStream>, BackendError> {
    let mut conn = Conn::open(&shared.socket)?;
    if conn.hello.as_ref() != Some(&shared.identity) {
        return Err(BackendError::Agmend(
            "agmend identity changed; Retry to check service ownership".into(),
        ));
    }
    // With the checkout, so agmend sends its changes (Task 8 review fix 3).
    let request = Request::new(conn.next_id, "console_watch", shared.checkout_params());
    frame::write(&mut conn.stream, &request).map_err(lost)?;
    // Events can be minutes apart; the watch waits for the next one.
    conn.stream.set_read_timeout(None).map_err(io_failed)?;
    let handle = conn.stream.try_clone().map_err(io_failed)?;
    let mut live = shared.lock();
    if live.closed {
        return Ok(None);
    }
    live.watch = Some(handle);
    Ok(Some(conn.stream))
}

fn on_event(shared: &Shared, jobs: &SyncSender<Job>, event: ConsoleEvent) -> Result<(), String> {
    let mut live = shared.lock();
    match event {
        ConsoleEvent::Hub {
            connected: false,
            error,
        } => {
            live.generation += 1;
            live.early = None;
            live.set_link(Link::HubDown {
                why: error.map(|e| e.message),
            });
        }
        ConsoleEvent::Hub {
            connected: true, ..
        } => {
            live.generation += 1;
            live.early = Some(VecDeque::new());
            let generation = live.generation;
            drop(live);
            // Blocks only while the request thread's queue is full; the
            // watch catches up after.
            jobs.send(Job::Snapshot { generation })
                .map_err(|_| "the agmend request thread stopped".to_owned())?;
        }
        ConsoleEvent::Message(env) => match &mut live.early {
            Some(early) => {
                early.push_back(env);
                if early.len() > MAX_MESSAGES {
                    early.pop_front();
                }
            }
            None => live.hear(env),
        },
        ConsoleEvent::Roster(roster) => {
            live.roster = roster;
            live.publish_snapshot();
        }
        // Task 8 review fix 3: the checkout changed on disk. A snapshot
        // answer still pending can replace it (see take_snapshot).
        ConsoleEvent::Local(local) => {
            live.local = *local;
            live.publish_snapshot();
        }
    }
    Ok(())
}

// The request thread.

fn serve_jobs(shared: &Shared, queue: &Receiver<Job>, mut conn: Option<Conn>) {
    // Ends when the backend and the watch thread have both gone.
    let interval = Duration::from_secs(2);
    let mut next_poll = std::time::Instant::now() + interval;
    loop {
        if shared.lock().closed {
            return;
        }
        if std::time::Instant::now() >= next_poll {
            refresh_agents(shared, &mut conn);
            next_poll = std::time::Instant::now() + interval;
        }
        let job = match queue
            .recv_timeout(next_poll.saturating_duration_since(std::time::Instant::now()))
        {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match job {
            Job::Agent { method, params } => {
                // Launch is not idempotent: never retry after losing its answer.
                let result =
                    open_for_request(&shared.socket, &shared.identity).and_then(|mut fresh| {
                        fresh.call(method, params).map_err(|error| match error {
                            Fail::Refused(error) => RequestError::Refused(error.message),
                            Fail::Lost(why) => RequestError::Down(why),
                        })
                    });
                shared.lock().agent_error = result.err().map(|error| error.to_string());
                refresh_agents(shared, &mut conn);
                shared.lock().publish_snapshot();
            }
            Job::Snapshot { generation } => {
                let answer = request(
                    shared,
                    &mut conn,
                    "console_snapshot",
                    shared.checkout_params(),
                )
                .and_then(|value| {
                    parse::<ConsoleSnapshot>(value)
                        .map_err(|e| RequestError::Refused(e.to_string()))
                });
                take_snapshot(shared, generation, answer);
            }
            Job::Post { draft, envelope } => {
                let params = json!({ "project": shared.project, "envelope": envelope });
                // A post that lands comes back on the watch.
                // shortcut: `request` retries a lost answer on a reused
                // connection once, with this same envelope, so the hub
                // dedupes it. What remains: the
                // answer is lost on a fresh connection or on that retry
                // (agmend died or took over REQUEST_TIMEOUT after taking
                // the post), the post landed anyway, and NotSent puts the
                // draft back. Sending it again mints a new id in
                // envelope_for, so the chat shows it twice. Ceiling: one
                // extra copy per resend of such a post. Revisit if people
                // see doubles; the fix is to keep the envelope id with the
                // returned draft, or drop the NotSent once the watch hears
                // that id.
                if let Err(why) = request(shared, &mut conn, "post", params) {
                    let why = why.to_string();
                    shared.lock().publish(&Update::NotSent { draft, why });
                }
            }
            Job::History { before } => {
                let params = json!({ "project": shared.project, "before": before, "limit": PAGE });
                let answer = request(shared, &mut conn, "chat_history", params)
                    .and_then(|value| {
                        parse::<HistoryPage>(value)
                            .map_err(|e| RequestError::Refused(e.to_string()))
                    })
                    .map_err(|e| e.to_string());
                take_history(shared, before, answer);
            }
            Job::FileText {
                worktree,
                path,
                reply,
            } => {
                let params = json!({
                    "project": shared.project,
                    "root": shared.root,
                    "worktree": worktree,
                    "path": path,
                });
                let answer = request(shared, &mut conn, "file_text", params)
                    .and_then(|value| {
                        parse::<FileTextEntry>(value)
                            .map(agmend_local::file_text_from)
                            .map_err(|e| RequestError::Refused(e.to_string()))
                    })
                    .map_err(|e| e.to_string());
                // The file view may have closed meanwhile.
                drop(reply.send(answer));
            }
        }
    }
}

fn refresh_agents(shared: &Shared, conn: &mut Option<Conn>) {
    #[derive(serde::Deserialize)]
    struct Agents {
        agents: Vec<ManagedAgent>,
    }
    let result = request(shared, conn, "agent_list", json!({})).and_then(|value| {
        parse::<Agents>(value).map_err(|error| RequestError::Refused(error.to_string()))
    });
    let mut live = shared.lock();
    match result {
        Ok(answer) => {
            if live.managed == answer.agents && live.agent_read_error.is_none() {
                return;
            }
            live.managed = answer.agents;
            live.agent_read_error = None;
        }
        Err(error) => {
            let why = format!("Reading managed agents: {error}");
            if live.agent_read_error.as_ref() == Some(&why) {
                return;
            }
            live.agent_read_error = Some(why);
        }
    }
    live.publish_snapshot();
}

/// `chat_history`'s answer (agmend's console.rs `History`).
#[derive(Debug, serde::Deserialize)]
struct HistoryPage {
    messages: Vec<StampedEnvelope>,
    older: bool,
}

/// Final review fix 1: applies an older page, or says why there is none.
fn take_history(shared: &Shared, before: Seq, answer: Result<HistoryPage, String>) {
    let mut live = shared.lock();
    live.paging = match answer {
        // A page for a chat a snapshot replaced meanwhile is dropped; the
        // next scroll to the top asks again.
        Ok(page) => {
            live.held.prepend(before, page.messages, page.older);
            Paging::Idle
        }
        Err(why) => Paging::Failed(why),
    };
    live.publish_snapshot();
}

/// Applies a snapshot answer, or ends the watch so it starts over.
fn take_snapshot(shared: &Shared, generation: u64, answer: Result<ConsoleSnapshot, RequestError>) {
    let mut live = shared.lock();
    if live.generation != generation || live.early.is_none() {
        // The watch moved on since it asked.
        return;
    }
    match answer {
        Ok(mut console) => {
            // shortcut: agmend reads the snapshot's local data (worktrees,
            // role files, git facts) while it answers, so a Local event the
            // watch applied before this answer arrived can be newer, and
            // this overwrites it. agmend sends Local only on a change, so
            // the older read stays until the next disk change it sees.
            // Ceiling: a change that lands during the snapshot's round
            // trip, once per snapshot request: each time the hub link comes
            // up and each time "Show the newest" loads the newest page.
            // Revisit if stale git facts show after a reconnect or after
            // "Show the newest"; the fix is a counter bumped on each Local
            // event, read when the request goes out, that keeps the newer
            // data here.
            live.local = take_local(&mut console);
            live.project = console.project;
            live.me = console.me;
            live.roster = console.roster;
            live.held.merge(console.messages, console.older);
            for env in live.early.take().unwrap_or_default() {
                live.held.insert(env);
            }
            live.went_live = true;
            live.publish_snapshot();
            live.set_link(Link::Live);
        }
        // No snapshot is queued again until the watch restarts, so a
        // watch left open would sit unread in `early`. Either way it ends
        // and starts over through its own backoff; only the link differs.
        Err(err) => {
            live.ended = Some(match err {
                RequestError::Down(why) => Link::AgmendDown {
                    why: format!("the chat's snapshot could not reach agmend: {why}"),
                },
                RequestError::Refused(why) => Link::Refused {
                    why: format!("the chat's snapshot failed: {why}"),
                },
            });
            if let Some(watch) = live.watch.take() {
                // The watch thread sees its read fail and reconnects.
                drop(watch.shutdown(std::net::Shutdown::Both));
            }
        }
    }
}

/// Why a request on the request connection failed.
#[derive(Debug)]
enum RequestError {
    /// Agmend could not be reached or spoke out of step, even on a fresh
    /// connection. This says nothing about the watch, which has its own.
    Down(String),
    /// agmend answered with an error.
    Refused(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Self::Down(why) | Self::Refused(why)) = self;
        f.write_str(why)
    }
}

/// One request on the request connection, opening it first when needed.
///
/// A reused connection may belong to an agmend that has since restarted.
/// When it fails at the connection level (not with an error answer), it is
/// dropped and the request goes once more on a fresh, greeted connection.
/// Every request is safe to repeat: a `post` carries its envelope's id, and
/// the hub dedupes on `(from, id)` (protocol 5.2 step 3), so a second try
/// of a post agmend already took gets the original ACK, not a second message.
/// A connection that failed is never kept.
fn request(
    shared: &Shared,
    conn: &mut Option<Conn>,
    method: &str,
    params: Value,
) -> Result<Value, RequestError> {
    let (mut open, reused) = match conn.take() {
        Some(open) => (open, true),
        None => (open_for_request(&shared.socket, &shared.identity)?, false),
    };
    let mut result = open.call(method, params.clone());
    if reused && matches!(result, Err(Fail::Lost(_))) {
        open = open_for_request(&shared.socket, &shared.identity)?;
        result = open.call(method, params);
    }
    match result {
        Ok(value) => {
            *conn = Some(open);
            Ok(value)
        }
        Err(Fail::Refused(error)) => {
            *conn = Some(open);
            Err(RequestError::Refused(error.message))
        }
        Err(Fail::Lost(why)) => Err(RequestError::Down(why)),
    }
}

/// A greeted connection, with a refused `hello` (an API version mismatch,
/// say) told apart from a socket that failed.
fn open_for_request(socket: &Path, expected: &HelloAnswer) -> Result<Conn, RequestError> {
    let conn = Conn::open(socket).map_err(|error| match error {
        BackendError::NotConnected => RequestError::Down(error.to_string()),
        _ => RequestError::Refused(error.to_string()),
    })?;
    if conn.hello.as_ref() != Some(expected) {
        return Err(RequestError::Refused(
            "agmend identity changed; Retry to check service ownership".into(),
        ));
    }
    Ok(conn)
}

// Connections.

/// A connection to agmend that has said `hello`.
#[derive(Debug)]
struct Conn {
    hello: Option<HelloAnswer>,
    ready: Option<ReadyAnswer>,
    stream: UnixStream,
    next_id: u64,
    deadline: Option<std::time::Instant>,
}

/// Why a request failed.
#[derive(Debug)]
enum Fail {
    /// The connection broke or agmend answered out of step.
    Lost(String),
    /// agmend answered with an error.
    Refused(ApiError),
}

impl From<Fail> for BackendError {
    fn from(fail: Fail) -> Self {
        match fail {
            Fail::Lost(_) => Self::NotConnected,
            Fail::Refused(error) => Self::Agmend(error.message),
        }
    }
}

fn lost(err: frame::FrameError) -> BackendError {
    BackendError::Agmend(format!("talking to agmend failed: {err}"))
}

fn io_failed(err: std::io::Error) -> BackendError {
    BackendError::Agmend(format!("setting up agmend's socket failed: {err}"))
}

impl Conn {
    /// Checks the socket is the person's own, connects and says `hello`.
    fn open(socket: &Path) -> Result<Self, BackendError> {
        let mut conn = Self::open_until(socket, std::time::Instant::now() + REQUEST_TIMEOUT)?;
        conn.clear_deadline()?;
        Ok(conn)
    }

    /// Ends a startup deadline: later calls get REQUEST_TIMEOUT each.
    fn clear_deadline(&mut self) -> Result<(), BackendError> {
        self.deadline = None;
        self.stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(io_failed)?;
        self.stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .map_err(io_failed)
    }

    fn open_until(socket: &Path, deadline: std::time::Instant) -> Result<Self, BackendError> {
        let mut conn = Self::connect(socket)?;
        conn.deadline = Some(deadline);
        let hello: HelloAnswer = parse(conn.call("hello", json!({ "api": API }))?)?;
        if hello.api != API {
            return Err(BackendError::Agmend(format!(
                "agmend API {} does not match {API}",
                hello.api
            )));
        }
        if hello.version.as_deref() != Some(env!("CARGO_PKG_VERSION")) {
            return Err(BackendError::Agmend(format!(
                "agmend version {:?} does not match {}; switch services with consent",
                hello.version,
                env!("CARGO_PKG_VERSION")
            )));
        }
        let ready: ReadyAnswer = parse(conn.call("ready", json!({}))?)?;
        if !ready.ready {
            return Err(BackendError::Agmend("agmend stores are not ready".into()));
        }
        conn.ready = Some(ready);
        conn.hello = Some(hello);
        Ok(conn)
    }

    /// Checks the socket is the person's own and connects, before `hello`.
    fn connect(socket: &Path) -> Result<Self, BackendError> {
        check_socket(socket, rustix::process::geteuid().as_raw())?;
        let stream = UnixStream::connect(socket).map_err(|_| BackendError::NotConnected)?;
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(io_failed)?;
        stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .map_err(io_failed)?;
        Ok(Self {
            stream,
            next_id: 1,
            hello: None,
            ready: None,
            deadline: None,
        })
    }

    /// Without a deadline the socket keeps the timeouts last set on it:
    /// REQUEST_TIMEOUT from `connect`, or a shorter bound from `probe`.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, Fail> {
        if let Some(deadline) = self.deadline {
            let timeout = deadline.saturating_duration_since(std::time::Instant::now());
            if timeout.is_zero() {
                return Err(Fail::Lost("agmend request deadline expired".into()));
            }
            self.stream
                .set_read_timeout(Some(timeout))
                .map_err(|error| Fail::Lost(error.to_string()))?;
            self.stream
                .set_write_timeout(Some(timeout))
                .map_err(|error| Fail::Lost(error.to_string()))?;
        }
        let id = self.next_id;
        self.next_id += 1;
        let broke = |err: frame::FrameError| Fail::Lost(format!("talking to agmend failed: {err}"));
        frame::write(&mut self.stream, &Request::new(id, method, params)).map_err(broke)?;
        if let Some(deadline) = self.deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(Fail::Lost("agmend request deadline expired".into()));
            }
            self.stream
                .set_read_timeout(Some(remaining))
                .map_err(|error| Fail::Lost(error.to_string()))?;
        }
        let response: Response = frame::read(&mut self.stream).map_err(broke)?;
        if response.id != id {
            return Err(Fail::Lost(format!(
                "agmend answered request {} while this app waited on {id}",
                response.id
            )));
        }
        match response.outcome {
            Outcome::Ok { result } => Ok(result),
            Outcome::Err { error } => Err(Fail::Refused(error)),
        }
    }
}

/// Refuses a socket another user could have made or can reach: anything
/// but a socket of mode 0600 owned by `uid`, the person's own uid.
///
/// # Errors
///
/// [`BackendError::NotConnected`] when nothing is at `socket`, and
/// [`BackendError::Agmend`] when it is not the person's own socket.
pub(super) fn check_socket(socket: &Path, uid: u32) -> Result<(), BackendError> {
    let shown = socket.display();
    let meta = match std::fs::metadata(socket) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(BackendError::NotConnected);
        }
        Err(err) => return Err(BackendError::Agmend(format!("cannot read {shown}: {err}"))),
    };
    if !meta.file_type().is_socket() {
        return Err(BackendError::Agmend(format!(
            "{shown} is not a socket, so it is not agmend's"
        )));
    }
    if meta.uid() != uid {
        return Err(BackendError::Agmend(format!(
            "{shown} is owned by uid {}, not by you (uid {uid}); refusing to talk to it",
            meta.uid()
        )));
    }
    let mode = meta.mode() & 0o777;
    if mode != 0o600 {
        return Err(BackendError::Agmend(format!(
            "{shown} has mode {mode:04o}, so other users can reach it; agmend's socket is 0600"
        )));
    }
    Ok(())
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, BackendError> {
    serde_json::from_value(value).map_err(|err| {
        BackendError::Agmend(format!("agmend sent an answer this app cannot read: {err}"))
    })
}

// Mapping agmend's data to the window's.

/// The window's snapshot from agmend's: the chat, the person, the roster,
/// and (Task 8) the role files, worktrees and git facts. Every field
/// agmend has no source for yet stays empty.
#[must_use]
pub(super) fn snapshot_from(console: ConsoleSnapshot) -> Snapshot {
    let ConsoleSnapshot {
        project,
        me,
        roster,
        messages,
        older,
        roles,
        worktrees,
        git,
    } = console;
    // The person's own listener; another person's never takes their
    // messages by default.
    let listener = roster
        .iter()
        .find(|e| e.kind == IdentityKind::Listener && person_of(&e.handle) == me.handle);
    let online = roster.iter().filter(|e| e.status == Status::Online).count();
    let groups = person_groups(&roster, &me.handle);
    let roles = super::role_files::role_files(
        roles
            .into_iter()
            .map(|file| {
                (
                    file.name,
                    agmend_local::role_layer(file.layer),
                    SharedString::from(file.text),
                )
            })
            .collect(),
        &groups,
    );
    let worktrees = agmend_local::worktrees_from(worktrees, &me);
    Snapshot {
        project,
        listener: listener.map(listener_info),
        roster: groups,
        managed_agents: BTreeMap::new(),
        managed_work: BTreeMap::new(),
        agent_error: None,
        me: me.handle,
        me_id: me.id,
        agents: BTreeMap::new(),
        status: StatusSummary {
            online: u32::try_from(online).unwrap_or(u32::MAX),
            worktrees: u32::try_from(worktrees.len()).unwrap_or(u32::MAX),
            spend_today_usd: 0.0,
        },
        messages,
        older: if older { Older::More } else { Older::Start },
        newer: false,
        waiting: 0,
        changes: BTreeMap::new(),
        plan: None,
        git: GitFacts {
            branches: git.branches.into_iter().collect(),
            commits: git.commits.into_iter().collect(),
            files: git.files.into_iter().collect(),
        },
        sessions: BTreeMap::new(),
        roles,
        worktrees,
        take_overs: BTreeMap::new(),
        installed_agents: Vec::new(),
        agent_scans: 0,
        models: Vec::new(),
        message_models: BTreeMap::new(),
        standing_model: None,
    }
}

/// The roster status as the title bar words it.
const fn status_word(status: Status) -> &'static str {
    match status {
        Status::Online => "online",
        Status::Away => "away",
        Status::Offline => "offline",
    }
}

/// The person a handle belongs to: `andy` for `andy/fx1` and for `andy`.
fn person_of(handle: &str) -> &str {
    handle.split_once('/').map_or(handle, |(person, _)| person)
}

/// The coding agent a HELLO named, without its version:
/// `claude-code` for `claude-code/2.1.310`.
fn harness_of(entry: &RosterEntry) -> String {
    entry
        .agent
        .as_deref()
        .map(|agent| person_of(agent).to_owned())
        .unwrap_or_default()
}

/// The roster as people and their agents, in `state/roster`'s order.
fn person_groups(roster: &[RosterEntry], me: &str) -> Vec<PersonGroup> {
    let mut groups: Vec<PersonGroup> = Vec::new();
    for entry in roster {
        match entry.kind {
            IdentityKind::Human => groups.push(PersonGroup {
                id: entry.identity_id.clone(),
                person: entry.handle.clone(),
                you: entry.handle == me,
                agents: Vec::new(),
            }),
            // The roster lists every human member (protocol 2), and an
            // identity belongs to a member, so its group is there.
            IdentityKind::Agent | IdentityKind::Coordinator | IdentityKind::Listener => {
                let person = person_of(&entry.handle);
                if let Some(group) = groups.iter_mut().find(|g| g.person == person) {
                    group.agents.push(agent_row(entry, roster));
                }
            }
            IdentityKind::Hub => {}
        }
    }
    groups
}

fn agent_row(entry: &RosterEntry, roster: &[RosterEntry]) -> AgentRow {
    let name = entry
        .handle
        .split_once('/')
        .map_or(entry.handle.as_str(), |(_, name)| name);
    AgentRow {
        id: entry.identity_id.clone(),
        handle: entry.handle.clone(),
        name: name.to_owned(),
        // shortcut: the roster carries a kind, not a role; the kind stands
        // in until agmend reports roles (Task 8).
        role: entry.kind.as_str().to_owned(),
        harness: harness_of(entry),
        status: match entry.status {
            Status::Online => AgentStatus::Working,
            Status::Away => AgentStatus::Idle,
            // Task 7: not reachable, so it must not read idle.
            Status::Offline => AgentStatus::Offline,
        },
        depth: depth(entry, roster),
        item: None,
    }
}

/// How many `delegated_by` links lead up from `entry`, at most
/// [`MAX_DEPTH`], so a cycle in the roster ends.
fn depth(entry: &RosterEntry, roster: &[RosterEntry]) -> u8 {
    let mut depth = 0;
    let mut parent = entry.delegated_by.as_ref();
    while depth < MAX_DEPTH {
        let Some(up) = parent.and_then(|id| roster.iter().find(|e| &e.identity_id == id)) else {
            break;
        };
        depth += 1;
        parent = up.delegated_by.as_ref();
    }
    depth
}

/// The Listener panel: the listener's identity, handle, agent and state;
/// no details, since agmend reports no policy or spend yet.
fn listener_info(listener: &RosterEntry) -> ListenerInfo {
    ListenerInfo {
        id: listener.identity_id.clone(),
        handle: listener.handle.clone(),
        agent: harness_of(listener),
        // The title bar's "listener: codex · offline".
        state: status_word(listener.status).to_owned(),
        details: None,
    }
}

// Outgoing messages.

/// The person's draft as the envelope agmend posts. FYI and ANSWER take no
/// `to` (protocol 5), so the draft's recipients are mentioned instead.
fn envelope_for<'a>(
    draft: &Draft,
    chat: impl IntoIterator<Item = &'a StampedEnvelope>,
) -> Result<ClientEnvelope, BackendError> {
    let body = Body::try_from(draft.body.clone()).map_err(|_| BackendError::BodyTooLarge {
        len: draft.body.len(),
        max: MAX_BODY_BYTES,
    })?;
    let payload = if super::answers_a_question(chat, draft.reply_to.as_ref()) {
        Payload::Answer {}
    } else {
        Payload::Fyi {}
    };
    Ok(ClientEnvelope {
        v: 1,
        id: new_message_id()?,
        reply_to: draft.reply_to.clone(),
        to: Vec::new(),
        mentions: draft.to.iter().cloned().map(Mention::Identity).collect(),
        items: Vec::new(),
        lane: None,
        body,
        valid_until: None,
        sent_at: Timestamp::now(),
        payload,
    })
}

// shortcut: the same ULID maker as agmen-cli's joined.rs; move one into
// agmen-proto when a third client needs it.
/// A fresh ULID: 48 bits of Unix milliseconds, then 80 random bits, in
/// Crockford base32.
fn new_message_id() -> Result<MessageId, BackendError> {
    const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let failed = |why: String| BackendError::Agmend(format!("making a message id failed: {why}"));
    let ms = u128::try_from(Timestamp::now().as_millisecond())
        .map_err(|_| failed("the clock is before 1970".into()))?
        & ((1 << 48) - 1);
    let mut random = [0_u8; 10];
    getrandom::fill(&mut random).map_err(|err| failed(err.to_string()))?;
    let mut value = random
        .iter()
        .fold(ms, |value, byte| (value << 8) | u128::from(*byte));
    let mut out = [b'0'; 26];
    for slot in out.iter_mut().rev() {
        *slot = CROCKFORD[usize::from(u8::try_from(value & 31).unwrap_or(0))];
        value >>= 5;
    }
    String::from_utf8_lossy(&out)
        .parse()
        .map_err(|_| failed("a generated ULID did not parse".into()))
}

#[cfg(test)]
#[path = "agmend_tests.rs"]
mod tests;
