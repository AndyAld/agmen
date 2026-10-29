//! Delivery in seq order (protocol 5.3). [`Streams`] is the link task's
//! side: it applies delivery replies to the outbox and, for a `Watch`
//! session, delivers each live feed or inbox message that is next by the
//! delivery cursor. A message after a gap, a reconnect, or a `state/head`
//! ahead of the feed cursor queues the stream for the [`Replayer`] task,
//! which pages the stream from the hub over HTTP and delivers what the
//! cursor has not seen.
//!
//! While a stream is queued or replaying, the link delivers none of its
//! live messages: it queues the stream again, and the next pass delivers
//! them from the hub, which records every message before it publishes it.
//! So only one task delivers a stream at a time, and its events come out in
//! cursor order.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use agmen_proto::{DeliveryReply, StampedEnvelope, Stream};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::cursor::{Advance, Cursors};
use crate::error::ClientError;
use crate::event::ClientEvent;
use crate::hub::HubClient;
use crate::outbox::Outbox;
use crate::session::{blocking, emit};

/// The most pages one replay pass reads (10,000 messages); a longer stream
/// is queued again for the next pass.
const PAGES_PER_PASS: u32 = 20;
/// Failed replays in a row before the session gives up.
const MAX_REPLAY_FAILURES: u32 = 60;
/// The first delay after a failed replay; it doubles up to
/// [`MAX_REPLAY_DELAY`].
const FIRST_REPLAY_DELAY: Duration = Duration::from_secs(1);
const MAX_REPLAY_DELAY: Duration = Duration::from_secs(60);

/// The part of the retained `state/head` the session uses (ruling R51).
#[derive(Debug, Deserialize)]
struct Head {
    feed_seq: u64,
}

/// Which streams wait for a replay pass and which one is replaying.
#[derive(Debug, Default)]
struct Queue {
    waiting: BTreeSet<Stream>,
    replaying: BTreeSet<Stream>,
}

type SharedQueue = Arc<Mutex<Queue>>;

fn lock(queue: &SharedQueue) -> std::sync::MutexGuard<'_, Queue> {
    queue.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The link task's side of delivery.
#[derive(Debug)]
pub(crate) struct Streams {
    /// The streams this session delivers: none, or the feed and its inbox.
    watched: Vec<Stream>,
    cursors: Cursors,
    outbox: Outbox,
    events: mpsc::Sender<ClientEvent>,
    queue: SharedQueue,
    /// Wakes the replayer; it ends when this is dropped.
    wake: mpsc::Sender<()>,
}

/// The replayer task.
#[derive(Debug)]
pub(crate) struct Replayer {
    cursors: Cursors,
    hub: HubClient,
    events: mpsc::Sender<ClientEvent>,
    queue: SharedQueue,
    wake: mpsc::Receiver<()>,
    /// Failed replays in a row.
    failures: u32,
}

/// The two sides of delivery for one session, sharing its cursors and
/// events.
pub(crate) fn delivery(
    watched: Vec<Stream>,
    cursors: Cursors,
    outbox: Outbox,
    hub: HubClient,
    events: mpsc::Sender<ClientEvent>,
) -> (Streams, Replayer) {
    let queue = SharedQueue::default();
    // One pending wake is enough: the replayer takes every queued stream.
    let (wake_tx, wake_rx) = mpsc::channel(1);
    let replayer = Replayer {
        cursors: cursors.clone(),
        hub,
        events: events.clone(),
        queue: queue.clone(),
        wake: wake_rx,
        failures: 0,
    };
    let streams = Streams {
        watched,
        cursors,
        outbox,
        events,
        queue,
        wake: wake_tx,
    };
    (streams, replayer)
}

impl Streams {
    /// Queues `stream` for a replay pass.
    fn replay(&self, stream: &Stream) {
        lock(&self.queue).waiting.insert(stream.clone());
        // A full channel already holds a wake the replayer has not taken.
        if self.wake.try_send(()).is_err() {
            tracing::trace!("the replayer is already woken");
        }
    }

    /// Whether `stream` waits for or is in a replay pass.
    fn replaying(&self, stream: &Stream) -> bool {
        let queue = lock(&self.queue);
        queue.waiting.contains(stream) || queue.replaying.contains(stream)
    }

    /// Whether no stream waits for or is in a replay pass.
    #[cfg(test)]
    pub(crate) fn idle(&self) -> bool {
        let queue = lock(&self.queue);
        queue.waiting.is_empty() && queue.replaying.is_empty()
    }

    /// The link (re)connected: catch every watched stream up.
    pub(crate) fn on_connected(&self) {
        for stream in &self.watched {
            self.replay(stream);
        }
    }

    /// An ACK or NACK on `ack/{id}`: apply it to the outbox.
    pub(crate) async fn on_reply(&self, payload: &[u8]) -> Result<(), ClientError> {
        let reply: DeliveryReply = match serde_json::from_slice(payload) {
            Ok(reply) => reply,
            Err(err) => {
                tracing::warn!(error = %err, "dropped a delivery reply that does not parse");
                return Ok(());
            }
        };
        let id = match &reply {
            DeliveryReply::Ack { id, .. } | DeliveryReply::Nack { id, .. } => id.clone(),
        };
        let outbox = self.outbox.clone();
        match blocking(move || outbox.apply(&reply)).await {
            Ok(state) => emit(&self.events, ClientEvent::Delivery { id, state }).await,
            // This identity sent it with another state folder.
            Err(ClientError::UnknownMessage(_)) => {
                tracing::debug!(%id, "a delivery reply for a message not in this outbox");
            }
            Err(err) => return Err(err),
        }
        Ok(())
    }

    /// The retained `state/head`: replay the feed if the hub is ahead.
    pub(crate) async fn on_head(&self, payload: &[u8]) -> Result<(), ClientError> {
        if !self.watched.contains(&Stream::Feed) {
            return Ok(());
        }
        let head: Head = match serde_json::from_slice(payload) {
            Ok(head) => head,
            Err(err) => {
                tracing::warn!(error = %err, "dropped a state/head that does not parse");
                return Ok(());
            }
        };
        let cursors = self.cursors.clone();
        if head.feed_seq > blocking(move || cursors.last(&Stream::Feed)).await? {
            self.replay(&Stream::Feed);
        }
        Ok(())
    }

    /// A live message on `stream`'s topic: deliver it if it is next,
    /// else leave it to replay.
    pub(crate) async fn on_message(
        &self,
        stream: Stream,
        payload: &[u8],
    ) -> Result<(), ClientError> {
        let message: StampedEnvelope = match serde_json::from_slice(payload) {
            Ok(message) => message,
            Err(err) => {
                tracing::warn!(%stream, error = %err, "dropped a message that does not parse");
                return Ok(());
            }
        };
        if message.stream != stream {
            tracing::warn!(%stream, claimed = %message.stream, "dropped a message whose stream does not match its topic");
            return Ok(());
        }
        if self.replaying(&stream) {
            // The replayer owns this stream now; its next pass delivers
            // this message from the hub.
            self.replay(&stream);
            return Ok(());
        }
        if deliver(&self.cursors, &self.events, message).await? == Delivered::AfterGap {
            self.replay(&stream);
        }
        Ok(())
    }
}

/// What [`deliver`] did with a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivered {
    /// Emitted, or dropped as a duplicate.
    Done,
    /// Held back: seqs before it are missing.
    AfterGap,
}

/// Emits `message` if it is next in its stream by the delivery cursor.
async fn deliver(
    cursors: &Cursors,
    events: &mpsc::Sender<ClientEvent>,
    message: StampedEnvelope,
) -> Result<Delivered, ClientError> {
    let (cursors, stream, seq) = (cursors.clone(), message.stream.clone(), message.seq.0);
    match blocking(move || cursors.advance(&stream, seq)).await? {
        Advance::Next => {
            emit(events, ClientEvent::message(message)).await;
            Ok(Delivered::Done)
        }
        Advance::Duplicate => {
            tracing::debug!(stream = %message.stream, seq, "dropped a duplicate");
            Ok(Delivered::Done)
        }
        Advance::Gap { from, to } => {
            tracing::debug!(stream = %message.stream, from, to, "gap before a message");
            Ok(Delivered::AfterGap)
        }
    }
}

/// How a replay pass over one stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// Caught up to the hub's head.
    Done,
    /// More pages wait.
    More,
}

impl Replayer {
    /// Runs until the link task stops, or replay fails for good.
    pub(crate) async fn run(mut self) -> Result<(), ClientError> {
        while self.wake.recv().await.is_some() {
            loop {
                let streams = {
                    let mut queue = lock(&self.queue);
                    let streams = std::mem::take(&mut queue.waiting);
                    queue.replaying.extend(streams.iter().cloned());
                    streams
                };
                if streams.is_empty() {
                    break;
                }
                for stream in streams {
                    self.pass(stream).await?;
                }
            }
        }
        Ok(())
    }

    /// One replay pass over `stream`, then hands it back to the link, or
    /// queues it again when pages remain or the hub could not be read.
    async fn pass(&mut self, stream: Stream) -> Result<(), ClientError> {
        let outcome = self.replay(&stream).await;
        let mut again = false;
        let mut delay = None;
        match outcome {
            Ok(Pass::Done) => self.failures = 0,
            Ok(Pass::More) => {
                self.failures = 0;
                again = true;
            }
            Err(err) if err.is_refusal() => return Err(err),
            Err(err) => {
                self.failures += 1;
                if self.failures >= MAX_REPLAY_FAILURES {
                    return Err(ClientError::ReplayGaveUp {
                        attempts: self.failures,
                        source: Box::new(err),
                    });
                }
                tracing::warn!(%stream, error = %err, failures = self.failures, "replay failed; trying again");
                again = true;
                delay = Some(replay_delay(self.failures));
            }
        }
        {
            let mut queue = lock(&self.queue);
            queue.replaying.remove(&stream);
            if again {
                queue.waiting.insert(stream);
            }
        }
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        Ok(())
    }

    /// Delivers up to [`PAGES_PER_PASS`] pages of `stream` after its
    /// delivery cursor.
    async fn replay(&self, stream: &Stream) -> Result<Pass, ClientError> {
        for _ in 0..PAGES_PER_PASS {
            let (cursors, key) = (self.cursors.clone(), stream.clone());
            let after = blocking(move || cursors.last(&key)).await?;
            let page = self.hub.page(stream, after).await?;
            if page.pruned_through > after {
                let (cursors, key) = (self.cursors.clone(), stream.clone());
                blocking(move || cursors.skip_to(&key, page.pruned_through)).await?;
            }
            for message in page.messages {
                if message.stream != *stream
                    || deliver(&self.cursors, &self.events, message).await? == Delivered::AfterGap
                {
                    // The hub numbers each stream without gaps, so its own
                    // page cannot skip a seq. Stop rather than loop.
                    tracing::warn!(%stream, after, "a replay page does not follow the cursor");
                    return Ok(Pass::Done);
                }
            }
            if page.next_after.is_none() {
                return Ok(Pass::Done);
            }
        }
        Ok(Pass::More)
    }
}

/// The delay after `failures` failed replays in a row.
fn replay_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    FIRST_REPLAY_DELAY
        .saturating_mul(1 << doublings)
        .min(MAX_REPLAY_DELAY)
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
