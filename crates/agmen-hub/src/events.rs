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
//! when ingest commits (`IngestState::stored`, an in-process watch, which
//! coalesces bursts) and at least every second, which covers commits that
//! do not signal it (the lane and lease sweeps) and a second hub replica,
//! should one ever run (today the sweeper requires one; see `sweeper.rs`).
//! Postgres LISTEN/NOTIFY would only replace the wake-up and needs a
//! dedicated connection, so it is left until the poll costs too much. Each
//! pass also reloads the person's streams: an identity minted while
//! connected is covered from the next pass, and a person who stops being a
//! member is disconnected.
//!
//! # Bounds
//!
//! Events go through a channel of [`CHANNEL`] events per connection. A
//! client that leaves an event unread for [`SEND_WAIT`] is disconnected;
//! it reconnects and replays, so nothing builds up in the hub.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use agmen_proto::{ProjectId, Stream};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::SendTimeoutError;

use crate::auth::Caller;
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
    let (tx, rx) = mpsc::channel(CHANNEL);
    // Not joined: the task ends by itself once the response is dropped,
    // because every send and every wait also watches for the channel
    // closing.
    tokio::spawn(pump(hub, human, project, positions, tx));
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
}

/// Runs one connection until it ends, and logs why.
async fn pump(
    hub: Hub,
    human: HumanId,
    project: ProjectId,
    mut positions: BTreeMap<Stream, u64>,
    tx: mpsc::Sender<Event>,
) {
    match deliver(&hub, &human, &project, &mut positions, &tx).await {
        Ok(ended) => tracing::debug!(%project, ?ended, "events stream ended"),
        Err(err) => {
            tracing::warn!(%project, error = %error_chain(&err), "events stream failed");
        }
    }
}

/// Replays and then delivers live until the stream ends (module docs).
async fn deliver(
    hub: &Hub,
    human: &HumanId,
    project: &ProjectId,
    positions: &mut BTreeMap<Stream, u64>,
    tx: &mpsc::Sender<Event>,
) -> Result<Ended, ApiError> {
    let mut wake = hub.ingest_state().watch_stored();
    loop {
        // Before reading, so a commit during this pass runs another.
        wake.mark_unchanged();
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
    fn after_is_bounded() {
        let at_limit = vec!["feed:1"; MAX_AFTER].join(",");
        assert!(parse_after(&at_limit).is_ok());
        let over = vec!["feed:1"; MAX_AFTER + 1].join(",");
        assert!(parse_after(&over).is_err());
    }
}
