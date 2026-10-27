//! Bounded listener input and post policy, without a runtime or clock.

// shortcut: L21 wires the source, monotonic time and resolved thread roots.
#![cfg_attr(not(test), expect(dead_code, reason = "wired by L21"))]

use std::collections::HashMap;
use std::time::Duration;

use agmen_proto::{IdentityId, IdentityKind, MessageId, Payload, StampedEnvelope, Verb};
use tokio::sync::mpsc;

const CAPACITY: usize = 256;
const BATCH_INTERVAL: Duration = Duration::from_secs(30);
const MAX_THREADS: usize = 256;

#[derive(Debug)]
pub(super) struct Incoming {
    /// Validated, size-limited hub message. The source deduplicates feed/inbox copies.
    pub(super) message: StampedEnvelope,
    /// Resolved root, not just reply_to: all descendants share one post cap.
    pub(super) thread: MessageId,
}

/// Unique within this queue's lifetime. L21 scopes it to the listener session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BatchId(u64);

#[derive(Debug)]
pub(super) struct Batch {
    pub(super) batch_id: BatchId,
    pub(super) messages: Vec<StampedEnvelope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PostDecision {
    Allow,
    ThreadCap,
    Capacity,
}

#[derive(Debug)]
pub(super) struct Queue {
    listener: IdentityId,
    person: IdentityId,
    source: mpsc::Receiver<Incoming>,
    pending: Vec<StampedEnvelope>,
    last_batch: Option<Duration>,
    next_batch: u64,
    posts: HashMap<MessageId, u8>,
}

impl Queue {
    pub(super) fn new(listener: IdentityId, person: IdentityId) -> (mpsc::Sender<Incoming>, Self) {
        let (tx, source) = mpsc::channel(CAPACITY);
        (
            tx,
            Self {
                listener,
                person,
                source,
                pending: Vec::with_capacity(CAPACITY),
                last_batch: None,
                next_batch: 0,
                posts: HashMap::new(),
            },
        )
    }

    /// Drains at most CAPACITY source events per call. A full pending buffer
    /// leaves events in the bounded source, applying backpressure without loss.
    /// `now` is monotonic elapsed time supplied by the caller; a backwards value
    /// cannot bypass the interval. Empty polls neither consume ids nor set time.
    /// `workers` is the current trusted list, including workers delegated since
    /// the previous poll. Membership scans are bounded by the slice length.
    pub(super) fn poll(&mut self, now: Duration, workers: &[IdentityId]) -> Option<Batch> {
        for _ in 0..CAPACITY {
            if self.pending.len() == CAPACITY {
                break;
            }
            let Ok(incoming) = self.source.try_recv() else {
                break;
            };
            let message = incoming.message;
            if matches!(message.from_kind, IdentityKind::Human | IdentityKind::Agent) {
                self.posts.remove(&incoming.thread);
            }
            if self.includes(&message, workers) {
                self.pending.push(message);
            }
        }
        if self.pending.is_empty()
            || self.last_batch.is_some_and(|last| {
                now.checked_sub(last)
                    .is_none_or(|elapsed| elapsed < BATCH_INTERVAL)
            })
        {
            return None;
        }
        // Exhaustion stops delivery rather than reusing an authorization id.
        let next = self.next_batch.checked_add(1)?;
        let batch_id = BatchId(self.next_batch);
        self.next_batch = next;
        self.last_batch = Some(now);
        Some(Batch {
            batch_id,
            messages: std::mem::replace(&mut self.pending, Vec::with_capacity(CAPACITY)),
        })
    }

    fn includes(&self, message: &StampedEnvelope, workers: &[IdentityId]) -> bool {
        if message.from == self.listener {
            return false;
        }
        let addressed = |id: &IdentityId| {
            message.mentioned.contains(id)
                || (matches!(message.client.payload, Payload::Dm {} | Payload::Ask { .. })
                    && message.client.to.contains(id))
                || matches!(&message.client.payload, Payload::Assign { to, .. } if to == id)
        };
        addressed(&self.listener)
            || addressed(&self.person)
            || (message.from == self.person
                && matches!(
                    message.client.payload,
                    Payload::Fyi {} | Payload::Ask { .. } | Payload::Answer {} | Payload::Ack {}
                )
                && message.client.mentions.is_empty()
                && message.mentioned.is_empty()
                && message.client.to.is_empty()
                && !matches!(message.client.payload, Payload::Ask { of: Some(_) }))
            || (workers.contains(&message.from)
                && matches!(
                    message.client.payload.verb(),
                    Some(Verb::Done | Verb::Status | Verb::Review | Verb::Alert)
                ))
    }

    /// Reserves one outgoing listener post before publishing. Rejected posts
    /// are dropped and logged without message bodies. Failed publishes keep
    /// the reservation conservatively. No eviction can reopen a capped thread.
    pub(super) fn try_post(&mut self, thread: &MessageId) -> PostDecision {
        if let Some(count) = self.posts.get_mut(thread) {
            if *count >= 3 {
                tracing::warn!("listener post dropped: thread cap");
                return PostDecision::ThreadCap;
            }
            *count += 1;
        } else {
            if self.posts.len() == MAX_THREADS {
                tracing::warn!("listener post dropped: thread tracking capacity");
                return PostDecision::Capacity;
            }
            self.posts.insert(thread.clone(), 1);
        }
        PostDecision::Allow
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
