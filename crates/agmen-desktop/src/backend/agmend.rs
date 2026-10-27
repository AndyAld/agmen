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
//!   time: a snapshot once a watch is connected, a post, or a file's text.
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
use agmen_local::{API, ApiError, ErrorCode, Outcome, Request, Response, frame};
use agmen_proto::{
    Body, ClientEnvelope, IdentityKind, MAX_BODY_BYTES, Mention, MessageId, Payload, Seq,
    StampedEnvelope, Stream, Timestamp,
};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::channel::oneshot;
use gpui::SharedString;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::agmend_local;
use super::{
    Action, AgentRow, AgentStatus, Backend, BackendError, Draft, FileText, FileTextReply, GitFacts,
    Link, ListenerInfo, PersonGroup, Snapshot, StatusSummary, Update, WorktreeInfo,
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

/// The live backend. Dropping it ends the watch and both threads.
#[derive(Debug)]
pub(crate) struct AgmendBackend {
    shared: Arc<Shared>,
    jobs: SyncSender<Job>,
    /// Dropped with the backend, which wakes the watch thread from its
    /// backoff wait so it can exit.
    _stop: Sender<()>,
}

#[derive(Debug)]
struct Shared {
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
    me: Me,
    roster: Vec<RosterEntry>,
    local: LocalData,
    held: Held,
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
    ended: Option<String>,
    /// Set when the backend is dropped.
    closed: bool,
    subscribers: Vec<UnboundedSender<Update>>,
}

impl Live {
    fn snapshot(&self) -> Snapshot {
        snapshot_from(ConsoleSnapshot {
            project: self.project.clone(),
            me: self.me.clone(),
            roster: self.roster.clone(),
            messages: self.held.to_vec(),
            // shortcut: no `older` in Snapshot until scroll-back pages
            // history; at most the newest 2000 messages show until then.
            older: false,
            roles: self.local.roles.clone(),
            worktrees: self.local.worktrees.clone(),
            git: self.local.git.clone(),
        })
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
            Added::Nothing => {}
        }
    }
}

/// The chat as held: at most [`MAX_MESSAGES`], keyed and ordered by
/// `(stream, seq)`. The console is the project feed, whose seqs are the
/// chat's order.
#[derive(Debug, Default)]
pub(super) struct Held(BTreeMap<(Stream, Seq), StampedEnvelope>);

/// What [`Held::insert`] did.
#[derive(Debug, PartialEq, Eq)]
enum Added {
    New,
    /// Added, and the oldest message dropped to stay at the cap.
    Trimmed,
    /// Held already, or older than everything kept at the cap.
    Nothing,
}

impl Held {
    fn insert(&mut self, env: StampedEnvelope) -> Added {
        let key = (env.stream.clone(), env.seq);
        if self.0.contains_key(&key) {
            return Added::Nothing;
        }
        self.0.insert(key.clone(), env);
        if self.0.len() <= MAX_MESSAGES {
            return Added::New;
        }
        match self.0.pop_first() {
            Some((oldest, _)) if oldest == key => Added::Nothing,
            _ => Added::Trimmed,
        }
    }

    /// Takes a fresh snapshot's page. When the page starts past the newest
    /// held message, the messages between are missing, so the old page goes
    /// rather than leave a silent hole in the chat.
    pub(super) fn merge(&mut self, fresh: Vec<StampedEnvelope>) {
        let gap = match (self.0.last_key_value(), fresh.first()) {
            (Some(((stream, newest), _)), Some(first)) => {
                *stream == first.stream && first.seq.0 > newest.0.saturating_add(1)
            }
            _ => false,
        };
        if gap {
            self.0.clear();
        }
        for env in fresh {
            self.insert(env);
        }
    }

    pub(super) fn to_vec(&self) -> Vec<StampedEnvelope> {
        self.0.values().cloned().collect()
    }

    fn find(&self, id: &MessageId) -> Option<&StampedEnvelope> {
        self.0.values().find(|m| &m.client.id == id)
    }
}

/// A job for the request thread.
#[derive(Debug)]
enum Job {
    /// A snapshot for the watch in the given generation.
    Snapshot { generation: u64 },
    Post {
        draft: Draft,
        envelope: Box<ClientEnvelope>,
    },
    /// Task 8: one file's text, for a file view that opened.
    FileText {
        worktree: String,
        path: String,
        reply: oneshot::Sender<Result<FileText, String>>,
    },
}

impl AgmendBackend {
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
        let mut conn = Conn::open(socket)?;
        let mut console = match conn.call("console_snapshot", checkout_params(project, root)) {
            Ok(value) => parse::<ConsoleSnapshot>(value)?,
            // agmend is up but the hub is not: open on who the person is,
            // and the watch fills the chat in once the hub is back.
            Err(Fail::Refused(error)) if error.code == ErrorCode::Hub => {
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
        held.merge(console.messages);
        let shared = Arc::new(Shared {
            socket: socket.to_owned(),
            project: project.to_owned(),
            root: root.map(Path::to_owned),
            live: Mutex::new(Live {
                project: console.project,
                me: console.me,
                roster: console.roster,
                local,
                held,
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
            shared,
            jobs,
            _stop: stop,
        })
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
            envelope_for(&draft, live.held.0.values())?
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
        let why = watch_once(shared, jobs);
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
            let why = live.ended.take().unwrap_or(why);
            live.set_link(Link::AgmendDown { why });
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

/// Runs one watch until it ends, and says why it ended.
fn watch_once(shared: &Shared, jobs: &SyncSender<Job>) -> String {
    let mut stream = match open_watch(shared) {
        Ok(Some(stream)) => stream,
        Ok(None) => return "the window closed".to_owned(),
        Err(err) => return err.to_string(),
    };
    loop {
        let response: Response = match frame::read(&mut stream) {
            Ok(response) => response,
            Err(err) => return format!("agmend's watch ended: {err}"),
        };
        let value = match response.outcome {
            Outcome::Ok { result } => result,
            // A refused or lagging watch: agmend closes it after this.
            Outcome::Err { error } => return error.message,
        };
        // An event a newer agmend added; the rest of the watch still holds.
        let Ok(event) = serde_json::from_value::<ConsoleEvent>(value) else {
            continue;
        };
        if let Err(why) = on_event(shared, jobs, event) {
            return why;
        }
    }
}

/// A greeted connection that now streams `project`'s events, or `None`
/// when the backend was dropped meanwhile.
fn open_watch(shared: &Shared) -> Result<Option<UnixStream>, BackendError> {
    let mut conn = Conn::open(&shared.socket)?;
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
        // answer still pending replaces it with its own read, which is at
        // most one tick older; the next change sends it again.
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
    for job in queue {
        match job {
            Job::Snapshot { generation } => {
                let answer = request(
                    shared,
                    &mut conn,
                    "console_snapshot",
                    shared.checkout_params(),
                )
                .and_then(|value| parse::<ConsoleSnapshot>(value).map_err(|e| e.to_string()));
                take_snapshot(shared, generation, answer);
            }
            Job::Post { draft, envelope } => {
                let params = json!({ "project": shared.project, "envelope": envelope });
                // A post that lands comes back on the watch.
                if let Err(why) = request(shared, &mut conn, "post", params) {
                    shared.lock().publish(&Update::NotSent { draft, why });
                }
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
                let answer = request(shared, &mut conn, "file_text", params).and_then(|value| {
                    parse::<FileTextEntry>(value)
                        .map(agmend_local::file_text_from)
                        .map_err(|e| e.to_string())
                });
                // The file view may have closed meanwhile.
                drop(reply.send(answer));
            }
        }
    }
}

/// Applies a snapshot answer, or ends the watch so it starts over.
fn take_snapshot(shared: &Shared, generation: u64, answer: Result<ConsoleSnapshot, String>) {
    let mut live = shared.lock();
    if live.generation != generation || live.early.is_none() {
        // The watch moved on since it asked.
        return;
    }
    match answer {
        Ok(mut console) => {
            live.local = take_local(&mut console);
            live.project = console.project;
            live.me = console.me;
            live.roster = console.roster;
            live.held.merge(console.messages);
            for env in live.early.take().unwrap_or_default() {
                live.held.insert(env);
            }
            live.went_live = true;
            live.publish_snapshot();
            live.set_link(Link::Live);
        }
        Err(why) => {
            live.ended = Some(format!("the chat's snapshot failed: {why}"));
            if let Some(watch) = live.watch.take() {
                // The watch thread sees its read fail and reconnects.
                drop(watch.shutdown(std::net::Shutdown::Both));
            }
        }
    }
}

/// One request on the request connection, opening it first when needed.
/// A connection that failed is dropped, so the next job opens a new one.
fn request(
    shared: &Shared,
    conn: &mut Option<Conn>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let mut open = match conn.take() {
        Some(open) => open,
        None => Conn::open(&shared.socket).map_err(|err| err.to_string())?,
    };
    match open.call(method, params) {
        Ok(value) => {
            *conn = Some(open);
            Ok(value)
        }
        Err(Fail::Refused(error)) => {
            *conn = Some(open);
            Err(error.message)
        }
        Err(Fail::Lost(why)) => Err(why),
    }
}

// Connections.

/// A connection to agmend that has said `hello`.
#[derive(Debug)]
struct Conn {
    stream: UnixStream,
    next_id: u64,
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
        check_socket(socket, rustix::process::geteuid().as_raw())?;
        let stream = UnixStream::connect(socket).map_err(|_| BackendError::NotConnected)?;
        stream
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(io_failed)?;
        stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .map_err(io_failed)?;
        let mut conn = Self { stream, next_id: 1 };
        conn.call("hello", json!({ "api": API }))?;
        Ok(conn)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, Fail> {
        let id = self.next_id;
        self.next_id += 1;
        let broke = |err: frame::FrameError| Fail::Lost(format!("talking to agmend failed: {err}"));
        frame::write(&mut self.stream, &Request::new(id, method, params)).map_err(broke)?;
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
        older: _,
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
        me: me.handle,
        me_id: me.id,
        agents: BTreeMap::new(),
        status: StatusSummary {
            online: u32::try_from(online).unwrap_or(u32::MAX),
            worktrees: u32::try_from(worktrees.len()).unwrap_or(u32::MAX),
            spend_today_usd: 0.0,
        },
        messages,
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
