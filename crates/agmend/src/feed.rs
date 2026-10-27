//! One events stream per watched project, shared by every console watching
//! it, plus the roster poll.
//!
//! The first `console_watch` for a project starts its task; the task ends
//! once no console is left. The task reads the hub's events stream and
//! sends each new feed message into a [`broadcast`] channel of
//! [`CHANNEL`] events, which every watching console reads. A console that
//! falls that far behind is disconnected; it reconnects and takes a new
//! snapshot, so the hub stream never waits for a slow console.
//!
//! # Positions and reconnects
//!
//! The task keeps its last seq on every stream it has seen (at most
//! [`MAX_POSITIONS`]) and resumes from them with `?after=`, dropping any
//! seq at or below them, so a reconnect sends each message once. The first
//! connect starts the feed at its head (`state?key=head`): a console's
//! snapshot holds the history. Inbox events move positions but are not
//! sent: the console is the project chat, as the snapshot and history are.
//! A failed connect retries after 1, 2, 4 ... capped at 30 s; a stream that
//! ends after connecting (the token expired) retries after 1 s.
//!
//! # The console's contract
//!
//! Each watch's first frame is `Hub { connected }`, sent after the console
//! joined the channel. Every `Hub { connected: true }` is sent after the
//! task's positions are set, so a snapshot the console takes after it
//! overlaps the stream and misses nothing; the console drops the overlap by
//! `(stream, seq)`.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use agmen_local::console::{ConsoleEvent, RosterEntry};
use agmen_proto::{ProjectId, StampedEnvelope, Stream};
use tokio::sync::broadcast;
use tokio::time::MissedTickBehavior;

use crate::hub::{Hub, HubError};
use crate::sse::{SseError, SseEvent, SseParser};

/// Events buffered per project for the slowest console.
pub(crate) const CHANNEL: usize = 256;
/// The most streams whose positions are kept; the hub takes 256 `after`
/// entries.
const MAX_POSITIONS: usize = 256;
/// The first reconnect delay and the cap (Global Constraints).
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Every watched project's feed.
#[derive(Debug)]
pub(crate) struct Feeds {
    hub: Arc<Hub>,
    roster_every: Duration,
    feeds: Mutex<HashMap<ProjectId, Arc<Feed>>>,
}

/// One project's shared feed.
#[derive(Debug)]
struct Feed {
    tx: broadcast::Sender<ConsoleEvent>,
    /// Whether the events stream is up; changed and read under this lock
    /// together with sends of `Hub` events, so a new console's first frame
    /// is never older than the events after it.
    connected: Mutex<bool>,
}

impl Feed {
    fn set_connected(&self, connected: bool) {
        let mut now = self
            .connected
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *now != connected {
            *now = connected;
            // No receivers is fine: the task checks for that and ends.
            let _sent = self.tx.send(ConsoleEvent::Hub { connected });
        }
    }

    fn send(&self, event: ConsoleEvent) {
        // No receivers is fine, as above.
        let _sent = self.tx.send(event);
    }
}

/// A console's view of one project's feed.
#[derive(Debug)]
pub(crate) struct Watch {
    /// The first frame to send.
    pub(crate) first: ConsoleEvent,
    /// Everything after it.
    pub(crate) rx: broadcast::Receiver<ConsoleEvent>,
}

impl Feeds {
    pub(crate) fn new(hub: Arc<Hub>, roster_every: Duration) -> Arc<Self> {
        Arc::new(Self {
            hub,
            roster_every,
            feeds: Mutex::new(HashMap::new()),
        })
    }

    /// Joins `project`'s feed, starting it if no console watches it. The
    /// console joins before the task starts, so the task never sees an
    /// empty channel and ends before its first console.
    pub(crate) fn watch(self: &Arc<Self>, project: &ProjectId) -> Watch {
        let mut feeds = self.feeds.lock().unwrap_or_else(PoisonError::into_inner);
        let (feed, started) = match feeds.get(project) {
            Some(feed) => (feed.clone(), false),
            None => {
                let feed = Arc::new(Feed {
                    tx: broadcast::channel(CHANNEL).0,
                    connected: Mutex::new(false),
                });
                feeds.insert(project.clone(), feed.clone());
                (feed, true)
            }
        };
        let connected = feed
            .connected
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let rx = feed.tx.subscribe();
        let first = ConsoleEvent::Hub {
            connected: *connected,
        };
        drop(connected);
        drop(feeds);
        if started {
            tokio::spawn(run(self.clone(), project.clone(), feed));
        }
        Watch { first, rx }
    }

    /// Whether `feed` has no console left; if so it leaves the map, under
    /// the same lock [`Feeds::watch`] joins under, so no console joins a
    /// feed that is ending.
    fn retire_if_idle(&self, project: &ProjectId, feed: &Arc<Feed>) -> bool {
        let mut feeds = self.feeds.lock().unwrap_or_else(PoisonError::into_inner);
        if feed.tx.receiver_count() > 0 {
            return false;
        }
        if feeds.get(project).is_some_and(|f| Arc::ptr_eq(f, feed)) {
            feeds.remove(project);
        }
        true
    }
}

/// How one events connection ended.
enum End {
    /// No console is left.
    Idle,
    /// The stream ended or failed after it connected.
    Dropped,
}

/// The feed task: connect, read, reconnect, until no console is left.
async fn run(feeds: Arc<Feeds>, project: ProjectId, feed: Arc<Feed>) {
    let mut state = State::default();
    let mut backoff = BACKOFF_START;
    let mut roster = RosterPoll::new(feeds.roster_every);
    loop {
        if feeds.retire_if_idle(&project, &feed) {
            return;
        }
        match connect(&feeds.hub, &project, &mut state).await {
            Ok(response) => {
                backoff = BACKOFF_START;
                feed.set_connected(true);
                let end = read(&feeds, &project, &feed, &mut state, &mut roster, response).await;
                feed.set_connected(false);
                if matches!(end, End::Idle) {
                    return;
                }
                tokio::time::sleep(BACKOFF_START).await;
            }
            Err(err) => {
                tracing::warn!(%project, error = %err, "the hub's events stream did not open");
                feed.set_connected(false);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_CAP);
            }
        }
    }
}

/// Where the task is on each stream.
#[derive(Debug, Default)]
struct State {
    positions: BTreeMap<Stream, u64>,
    last_id: Option<String>,
}

impl State {
    /// Takes `message` if it is past the stream's position; true when it
    /// is new.
    fn advance(&mut self, message: &StampedEnvelope) -> bool {
        let seq = message.seq.0;
        if let Some(at) = self.positions.get_mut(&message.stream) {
            if seq <= *at {
                return false;
            }
            *at = seq;
            return true;
        }
        if self.positions.len() >= MAX_POSITIONS {
            // shortcut: a stream past the cap is sent but not tracked, so a
            // reconnect replays it; a person with 255 inboxes would need the
            // hub to page positions.
            return true;
        }
        self.positions.insert(message.stream.clone(), seq);
        true
    }
}

/// Opens the events stream, setting the feed's starting position first.
async fn connect(
    hub: &Hub,
    project: &ProjectId,
    state: &mut State,
) -> Result<reqwest::Response, HubError> {
    if let Entry::Vacant(feed) = state.positions.entry(Stream::Feed) {
        feed.insert(hub.head(project).await?);
    }
    hub.events(project, &state.positions, state.last_id.as_deref())
        .await
}

/// Reads one events connection until it ends or no console is left,
/// polling the roster between chunks.
async fn read(
    feeds: &Feeds,
    project: &ProjectId,
    feed: &Arc<Feed>,
    state: &mut State,
    roster: &mut RosterPoll,
    mut response: reqwest::Response,
) -> End {
    let mut parser = SseParser::default();
    loop {
        tokio::select! {
            // Cancel safe: reqwest yields a chunk only when it is whole, so
            // a chunk() dropped for the tick loses nothing.
            chunk = response.chunk() => {
                let chunk = match chunk {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => return End::Dropped,
                    Err(err) => {
                        tracing::info!(%project, error = %err, "the hub's events stream broke");
                        return End::Dropped;
                    }
                };
                match parser.push(&chunk) {
                    Ok(events) => {
                        for event in events {
                            handle(project, feed, state, event);
                        }
                    }
                    Err(err) => {
                        log_bad_stream(project, &err);
                        return End::Dropped;
                    }
                }
            }
            () = roster.tick() => {
                if feeds.retire_if_idle(project, feed) {
                    return End::Idle;
                }
                if let Some(rows) = roster.poll(&feeds.hub, project).await {
                    feed.send(ConsoleEvent::Roster(rows));
                }
            }
        }
    }
}

fn log_bad_stream(project: &ProjectId, err: &SseError) {
    tracing::warn!(%project, error = %err, "the hub's events stream is malformed");
}

/// Applies one event: a new feed message goes to the consoles.
fn handle(project: &ProjectId, feed: &Feed, state: &mut State, event: SseEvent) {
    let message: StampedEnvelope = match serde_json::from_str(&event.data) {
        Ok(message) => message,
        Err(err) => {
            tracing::warn!(%project, error = %err, "skipping an event that is not a message");
            return;
        }
    };
    if event.id.is_some() {
        state.last_id = event.id;
    }
    if state.advance(&message) && message.stream == Stream::Feed {
        feed.send(ConsoleEvent::Message(message));
    }
}

/// The roster, re-read on a timer.
///
/// shortcut: polls `state?key=roster` every 15 s; replace with the hub's
/// state stream once it sends one.
struct RosterPoll {
    interval: tokio::time::Interval,
    last: Option<Vec<RosterEntry>>,
}

impl RosterPoll {
    fn new(every: Duration) -> Self {
        let mut interval = tokio::time::interval(every);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self {
            interval,
            last: None,
        }
    }

    /// Waits for the next poll. Cancel safe, as `Interval::tick` is.
    async fn tick(&mut self) {
        self.interval.tick().await;
    }

    /// Reads the roster; the new rows when they differ from the last read.
    /// The first read only sets the baseline: the console's snapshot
    /// carries the roster.
    async fn poll(&mut self, hub: &Hub, project: &ProjectId) -> Option<Vec<RosterEntry>> {
        let rows = match hub.roster(project).await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::info!(%project, error = %err, "reading the roster failed");
                return None;
            }
        };
        let changed = self.last.as_ref().is_some_and(|last| *last != rows);
        let first = self.last.is_none();
        self.last = Some(rows.clone());
        (changed && !first).then_some(rows)
    }
}
