//! `GET /v1/projects/{p}/events` (protocol section 6): a member's chat as
//! server-sent events, for the web console and agmend. The stream carries
//! the feed and every inbox the person may read (the rule and the page
//! query are replay's, `replay.rs`): their own and those of the identities
//! they own in the project. Each event's id is `<stream>:<seq>` and its
//! data is the `StampedEnvelope`. A keep-alive comment goes out every 15 s.
//!
//! # Resuming
//!
//! One `<stream>:<seq>` cannot say where the reader is on the other
//! streams, and seqs on different streams are not ordered with each other,
//! so a reconnect cannot resume every stream from `Last-Event-ID` alone.
//! The reader also passes `?after=<stream>:<seq>,<stream>:<seq>,...`, its
//! last seq on each stream it has seen. Each stream resumes after the later
//! of its `after` entry and `Last-Event-ID`, or from the start when neither
//! names it. A browser's own reconnect keeps the original URL, so its
//! `after` can be stale: the stream of `Last-Event-ID` still resumes
//! exactly, and the others replay from the stale position, which the reader
//! drops by `(stream, seq)` (protocol 5.3). An entry for a stream the
//! person may not read is ignored, so naming one shows nothing.
//!
//! # Live delivery
//!
//! A connection does not take messages from a channel that could drop
//! them; it reads the database. Each pass reads the heads of its streams
//! and pages (at most [`MAX_PAGE`]) every stream past its position, sending
//! only seqs above the position, which moves with each event sent. Seqs
//! commit in order with no gaps (protocol 5.2), so a read after seq N sees
//! N + 1 next or nothing, and replay and live delivery are the same loop
//! with no seam where a message could be lost or sent twice. A pass runs
//! when ingest commits in the stream's project (`IngestState::stored`, an
//! in-process watch per project, [`Wakes`], which coalesces bursts and
//! leaves other projects' streams asleep) and at least every second, which
//! covers commits that
//! do not signal it (the lane and lease sweeps) and a second hub replica,
//! should one ever run (today the sweeper requires one; see `sweeper.rs`).
//! Postgres LISTEN/NOTIFY would only replace the wake-up and needs a
//! dedicated connection, so it is left until the poll costs too much. Each
//! pass also reloads the person's streams: an identity minted while
//! connected is covered from the next pass, and a person who stops being a
//! member is disconnected.
//!
//! Each pass also checks the access token the stream was opened with, so
//! a stream ends within a second of its token expiring or being deleted
//! (a logout). The reader reconnects with a fresh token.
//!
//! # Bounds
//!
//! Events go through a channel of [`CHANNEL`] events per connection. A
//! client that leaves an event unread for [`SEND_WAIT`] is disconnected;
//! it reconnects and replays, so nothing builds up in the hub. A hub holds
//! at most [`MAX_STREAMS`] streams open, and [`MAX_STREAMS_PER_HUMAN`] for
//! one person: past the person's cap the answer is 429 `too_many_streams`,
//! past the hub's 503 `hub_busy`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use agmen_proto::{ProjectId, Stream};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use serde::Deserialize;
use tokio::sync::mpsc::error::SendTimeoutError;
use tokio::sync::{mpsc, watch};

use crate::auth::{Caller, bearer};
use crate::db::hash_secret;
use crate::http::{ApiError, Hub, error_chain};
use crate::identities::HumanId;
use crate::ingest::stream_key;
use crate::lanes::member_of;
use crate::replay::{MAX_PAGE, fetch_page, human_streams};

/// How often an idle stream sends a keep-alive comment.
const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// The longest a connection waits between passes with no wake-up.
///
/// shortcut: every connection reads its stream heads once a second. Wake
/// from Postgres LISTEN/NOTIFY instead when many connections make that
/// poll show up in the database's load.
const POLL: Duration = Duration::from_secs(1);
/// Events queued per connection.
const CHANNEL: usize = 64;
/// How long one event may wait for room in the channel before the
/// connection is dropped as too slow.
const SEND_WAIT: Duration = Duration::from_secs(30);
/// The most `after` entries a reader may name.
const MAX_AFTER: usize = 256;
/// The most events streams one hub holds open.
///
/// shortcut: fixed caps, sized so the passes of every open stream (three
/// short queries at least once a second each) leave the pool's 10
/// connections room for ingest and replay. Make them settings, or move
/// the wake-up to LISTEN/NOTIFY, when one hub serves more people.
const MAX_STREAMS: usize = 128;
/// The most events streams one person may hold open, across tabs and
/// devices.
const MAX_STREAMS_PER_HUMAN: usize = 8;

/// The query string: `after`, the reader's last seq per stream.
#[derive(Debug, Deserialize)]
pub(crate) struct EventsQuery {
    after: Option<String>,
}

fn invalid(description: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", description)
}

/// `GET /v1/projects/{p}/events?after=` (a member's access token), with an
/// optional `Last-Event-ID` (see the module docs).
pub(crate) async fn events(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
    headers: HeaderMap,
    query: Result<Query<EventsQuery>, QueryRejection>,
) -> Result<Sse<KeepAliveStream<Events>>, ApiError> {
    let human = caller.human()?;
    // The extractor accepted this token; keep its hash to check it again
    // on every pass.
    let token = hash_secret(bearer(&headers).ok_or_else(ApiError::unauthorized)?);
    let (project, _) = member_of(&hub, Caller::Human(human.clone()), &project).await?;
    let Query(query) = query.map_err(|err| invalid(err.body_text()))?;
    let mut positions = match &query.after {
        Some(after) => parse_after(after)?,
        None => BTreeMap::new(),
    };
    if let Some(last) = headers.get("last-event-id") {
        let last = last
            .to_str()
            .map_err(|_| invalid("Last-Event-ID is not <stream>:<seq>"))?;
        if !last.is_empty() {
            let (stream, seq) = parse_position(last)
                .ok_or_else(|| invalid("Last-Event-ID is not <stream>:<seq>"))?;
            let at = positions.entry(stream).or_insert(0);
            *at = (*at).max(seq);
        }
    }
    let slot = take_slot(&hub, &human)?;
    let (tx, rx) = mpsc::channel(CHANNEL);
    let conn = Conn {
        hub,
        human,
        project,
        token,
        _slot: slot,
    };
    // Not joined: the task ends by itself once the response is dropped,
    // because every send and every wait also watches for the channel
    // closing. Its slot goes with it.
    tokio::spawn(pump(conn, positions, tx));
    Ok(Sse::new(Events { rx }).keep_alive(KeepAlive::new().interval(KEEP_ALIVE)))
}

/// The response body's events, as the connection's task sends them.
#[derive(Debug)]
pub(crate) struct Events {
    rx: mpsc::Receiver<Event>,
}

impl futures_core::Stream for Events {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|event| event.map(Ok))
    }
}

/// `<stream>:<seq>,...` as a position per stream; a stream named twice
/// keeps the later seq.
fn parse_after(after: &str) -> Result<BTreeMap<Stream, u64>, ApiError> {
    let mut positions = BTreeMap::new();
    if after.is_empty() {
        return Ok(positions);
    }
    for (n, entry) in after.split(',').enumerate() {
        if n >= MAX_AFTER {
            return Err(invalid(format!(
                "after names more than {MAX_AFTER} streams"
            )));
        }
        let (stream, seq) = parse_position(entry)
            .ok_or_else(|| invalid("after is not a list of <stream>:<seq>"))?;
        let at = positions.entry(stream).or_insert(0);
        *at = (*at).max(seq);
    }
    Ok(positions)
}

/// One `<stream>:<seq>`, the seq within what the database can hold.
fn parse_position(text: &str) -> Option<(Stream, u64)> {
    let (stream, seq) = text.rsplit_once(':')?;
    if !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let seq: u64 = seq.parse().ok()?;
    i64::try_from(seq).ok()?;
    Some((stream.parse().ok()?, seq))
}

/// Why a connection's task stopped without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The client went away.
    Closed,
    /// The client left an event unread for [`SEND_WAIT`].
    TooSlow,
    /// The person is no longer a member of the project.
    NotAMember,
    /// The access token expired or was deleted.
    TokenGone,
}

/// One open stream: who it is for, the hash of the token it was opened
/// with, and its place among the hub's streams.
#[derive(Debug)]
struct Conn {
    hub: Hub,
    human: HumanId,
    project: ProjectId,
    token: Vec<u8>,
    _slot: Slot,
}

/// Runs one connection until it ends, and logs why.
async fn pump(conn: Conn, mut positions: BTreeMap<Stream, u64>, tx: mpsc::Sender<Event>) {
    let project = &conn.project;
    match deliver(&conn, &mut positions, &tx).await {
        Ok(ended) => tracing::debug!(%project, ?ended, "events stream ended"),
        Err(err) => {
            tracing::warn!(%project, error = %error_chain(&err), "events stream failed");
        }
    }
}

/// Replays and then delivers live until the stream ends (module docs).
async fn deliver(
    conn: &Conn,
    positions: &mut BTreeMap<Stream, u64>,
    tx: &mpsc::Sender<Event>,
) -> Result<Ended, ApiError> {
    let Conn {
        hub,
        human,
        project,
        token,
        ..
    } = conn;
    let mut wake = hub.ingest_state().watch_stored(project);
    loop {
        // Before reading, so a commit during this pass runs another.
        wake.mark_unchanged();
        let live: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM tokens
                            WHERE hash = $1 AND kind = 'access' AND expires_at > now())",
        )
        .bind(token)
        .fetch_one(hub.pool())
        .await?;
        if !live {
            return Ok(Ended::TokenGone);
        }
        let Some(streams) = human_streams(hub.pool(), human, project).await? else {
            return Ok(Ended::NotAMember);
        };
        let heads = heads(hub, project, &streams).await?;
        for stream in streams {
            let head = heads.get(&stream).copied().unwrap_or(0);
            let at = positions.entry(stream.clone()).or_insert(0);
            if let Some(ended) = send_until(hub, project, &stream, at, head, tx).await? {
                return Ok(ended);
            }
        }
        tokio::select! {
            changed = wake.changed() => {
                // The sender lives in the hub, which this task holds, so
                // it cannot close; a poll every second would still run.
                if changed.is_err() {
                    tokio::time::sleep(POLL).await;
                }
            }
            () = tokio::time::sleep(POLL) => {}
            () = tx.closed() => return Ok(Ended::Closed),
        }
    }
}

/// Sends `stream`'s messages after `*at` up to `head`, moving `*at` with
/// each one sent.
async fn send_until(
    hub: &Hub,
    project: &ProjectId,
    stream: &Stream,
    at: &mut u64,
    head: u64,
    tx: &mpsc::Sender<Event>,
) -> Result<Option<Ended>, ApiError> {
    while *at < head {
        let after = i64::try_from(*at).map_err(ApiError::stored)?;
        let page = fetch_page(hub.pool(), project, stream, after, MAX_PAGE).await?;
        if page.is_empty() {
            break;
        }
        for message in page {
            let seq = message.seq.0;
            // fetch_page reads seq > *at; this keeps a repeat out even so.
            if seq <= *at {
                continue;
            }
            debug_assert_eq!(seq, *at + 1, "a gap in {stream}");
            let event = Event::default()
                .id(format!("{stream}:{seq}"))
                .data(serde_json::to_string(&message).map_err(ApiError::stored)?);
            match tx.send_timeout(event, SEND_WAIT).await {
                Ok(()) => *at = seq,
                Err(SendTimeoutError::Timeout(_)) => return Ok(Some(Ended::TooSlow)),
                Err(SendTimeoutError::Closed(_)) => return Ok(Some(Ended::Closed)),
            }
        }
    }
    Ok(None)
}

/// The last seq of each of `streams` that has one.
async fn heads(
    hub: &Hub,
    project: &ProjectId,
    streams: &BTreeSet<Stream>,
) -> Result<HashMap<Stream, u64>, ApiError> {
    let mut keys: HashMap<String, &Stream> = streams
        .iter()
        .map(|stream| (stream_key(project, stream), stream))
        .collect();
    let names: Vec<String> = keys.keys().cloned().collect();
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT stream, seq FROM stream_heads WHERE stream = ANY($1)")
            .bind(&names)
            .fetch_all(hub.pool())
            .await?;
    let mut heads = HashMap::with_capacity(rows.len());
    for (key, seq) in rows {
        if let Some(stream) = keys.remove(&key) {
            heads.insert(
                stream.clone(),
                u64::try_from(seq).map_err(ApiError::stored)?,
            );
        }
    }
    Ok(heads)
}

/// A watch per project that events streams sleep on until a commit in
/// their project (or the poll) wakes them.
#[derive(Debug, Default)]
pub(crate) struct Wakes {
    by_project: Mutex<HashMap<ProjectId, watch::Sender<()>>>,
}

impl Wakes {
    /// Wakes `project`'s streams. A project nobody watches any more loses
    /// its entry here, so the map holds only watched projects.
    pub(crate) fn wake(&self, project: &ProjectId) {
        let mut by_project = self
            .by_project
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(sender) = by_project.get(project)
            && sender.send(()).is_err()
        {
            by_project.remove(project);
        }
    }

    /// A receiver that sees a change after each [`Wakes::wake`] of
    /// `project`.
    pub(crate) fn watch(&self, project: &ProjectId) -> watch::Receiver<()> {
        self.by_project
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(project.clone())
            .or_default()
            .subscribe()
    }

    /// How many projects have an entry.
    #[cfg(test)]
    fn watched(&self) -> usize {
        self.by_project
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// How many events streams are open, in all and per person.
#[derive(Debug)]
pub(crate) struct Slots {
    counts: Mutex<Counts>,
}

#[derive(Debug)]
struct Counts {
    total: usize,
    per_human: HashMap<HumanId, usize>,
    max_total: usize,
    max_per_human: usize,
}

impl Default for Slots {
    fn default() -> Self {
        Self {
            counts: Mutex::new(Counts {
                total: 0,
                per_human: HashMap::new(),
                max_total: MAX_STREAMS,
                max_per_human: MAX_STREAMS_PER_HUMAN,
            }),
        }
    }
}

impl Slots {
    fn counts(&self) -> std::sync::MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the caps (test hook).
    #[cfg(feature = "test-hooks")]
    pub(crate) fn set_limits(&self, total: usize, per_human: usize) {
        let mut counts = self.counts();
        counts.max_total = total;
        counts.max_per_human = per_human;
    }
}

/// One open stream's place, freed on drop.
#[derive(Debug)]
struct Slot {
    hub: Hub,
    human: HumanId,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut counts = self.hub.event_slots().counts();
        counts.total = counts.total.saturating_sub(1);
        if let Some(mine) = counts.per_human.get_mut(&self.human) {
            *mine = mine.saturating_sub(1);
            if *mine == 0 {
                counts.per_human.remove(&self.human);
            }
        }
    }
}

/// A place for one more stream of `human`'s, or 429 past their cap and
/// 503 past the hub's.
fn take_slot(hub: &Hub, human: &HumanId) -> Result<Slot, ApiError> {
    let mut counts = hub.event_slots().counts();
    let mine = counts.per_human.get(human).copied().unwrap_or(0);
    if mine >= counts.max_per_human {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_streams",
            "you have too many event streams open; close one",
        ));
    }
    if counts.total >= counts.max_total {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "hub_busy",
            "the hub has too many event streams open; try again later",
        ));
    }
    counts.total += 1;
    counts.per_human.insert(human.clone(), mine + 1);
    drop(counts);
    Ok(Slot {
        hub: hub.clone(),
        human: human.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbox(id: &str) -> Stream {
        Stream::Inbox(id.parse().unwrap())
    }

    #[test]
    fn a_position_is_a_stream_and_a_seq() {
        assert_eq!(parse_position("feed:12"), Some((Stream::Feed, 12)));
        assert_eq!(
            parse_position("inbox:idn7k2q9:4"),
            Some((inbox("idn7k2q9"), 4))
        );
        assert_eq!(parse_position("feed:0"), Some((Stream::Feed, 0)));
        for bad in [
            "feed",
            "feed:",
            "feed:x",
            "feed:-1",
            "feed:+1",
            "dm:1",
            "inbox:1",
            "inbox:IDN7K2Q9:1",
            "feed:9223372036854775808",
        ] {
            assert_eq!(parse_position(bad), None, "{bad}");
        }
    }

    #[test]
    fn after_keeps_the_later_seq_per_stream() {
        let positions = parse_after("feed:3,inbox:idn7k2q9:4,feed:7,feed:5").unwrap();
        assert_eq!(
            positions,
            BTreeMap::from([(Stream::Feed, 7), (inbox("idn7k2q9"), 4)])
        );
        assert!(parse_after("").unwrap().is_empty());
        assert!(parse_after("feed:1,").is_err());
        assert!(parse_after("feed:1;inbox:abc").is_err());
    }

    #[test]
    fn a_wake_reaches_only_its_project() {
        let wakes = Wakes::default();
        let a: ProjectId = "projecta1".parse().unwrap();
        let b: ProjectId = "projectb1".parse().unwrap();
        let mut on_a = wakes.watch(&a);
        wakes.wake(&b);
        assert!(
            !on_a.has_changed().unwrap(),
            "a commit in b woke a stream in a"
        );
        wakes.wake(&a);
        assert!(on_a.has_changed().unwrap());
        on_a.mark_unchanged();
        // A project nobody watches keeps no entry.
        drop(on_a);
        wakes.wake(&a);
        assert_eq!(wakes.watched(), 0);
    }

    #[test]
    fn after_is_bounded() {
        let at_limit = vec!["feed:1"; MAX_AFTER].join(",");
        assert!(parse_after(&at_limit).is_ok());
        let over = vec!["feed:1"; MAX_AFTER + 1].join(",");
        assert!(parse_after(&over).is_err());
    }
}
