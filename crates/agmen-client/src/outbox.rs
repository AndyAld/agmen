//! The outbox (protocol 5.1): every envelope is written here with
//! `synchronous=FULL` before it is published, and sent again with the same
//! id until the hub answers with an ACK or a final NACK, across restarts.

use std::path::Path;
use std::time::Duration;

use agmen_proto::{ClientEnvelope, DeliveryReply, MessageId, NackReason, Seq, Stream, Timestamp};
use rusqlite::{ErrorCode, OptionalExtension as _, TransactionBehavior, params};
use serde::de::IntoDeserializer as _;
use serde::{Deserialize, Serialize};

use crate::db::{Db, from_sql_int, to_sql_seq};
use crate::error::ClientError;

/// The first retry delay; it doubles with each attempt up to [`MAX_RETRY`].
const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(60);
/// Each delay is spread by up to this many percent either way, so clients
/// that lost the hub together do not all retry together.
const JITTER_PERCENT: u32 = 20;
/// The most envelopes one [`Outbox::due`] call returns.
const DUE_LIMIT: i64 = 256;

/// Where one outbox message stands. Serializes as
/// `{"state":"pending","attempts":n}`, `{"state":"recorded","stream","seq"}`
/// or `{"state":"rejected","reason"}` (ruling R31).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum OutboxState {
    /// Not answered yet, or answered with a retryable NACK; sent `attempts`
    /// times so far.
    Pending {
        /// How many times it was published.
        attempts: u32,
    },
    /// The hub recorded it at `seq` in `stream`.
    Recorded {
        /// The stream it went to; a DM is recorded in the sender's inbox.
        stream: Stream,
        /// Its number there.
        seq: Seq,
    },
    /// The hub refused it for good; it is never sent again.
    Rejected {
        /// Why.
        reason: NackReason,
    },
}

/// The outbox in the identity's SQLite file. Cheap to clone; clones share
/// one connection.
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use agmen_client::{Outbox, OutboxState};
/// use agmen_proto::{ClientEnvelope, DeliveryReply, Seq, Stream, Timestamp};
///
/// let dir = tempfile::tempdir()?;
/// let outbox = Outbox::open(dir.path().join("identity.db"))?;
/// let env = ClientEnvelope::from_json(br#"{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS",
///     "verb":"FYI","data":{},"body":"hi","sent_at":"2026-10-25T16:04:11Z"}"#)?;
/// outbox.enqueue(&env)?;
/// assert_eq!(outbox.due(Timestamp::now())?, [env.clone()]);
/// let ack = DeliveryReply::Ack { id: env.id.clone(), stream: Stream::Feed, seq: Seq(1) };
/// assert_eq!(outbox.apply(&ack)?, OutboxState::Recorded { stream: Stream::Feed, seq: Seq(1) });
/// assert!(outbox.due(Timestamp::now())?.is_empty());
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
pub struct Outbox {
    db: Db,
}

/// A row's state columns: state, attempts, stream, seq, reason.
type StateRow = (String, i64, Option<String>, Option<i64>, Option<String>);

impl Outbox {
    /// Opens the outbox in the SQLite file at `path`, creating the file and
    /// its tables if needed.
    ///
    /// # Errors
    ///
    /// [`ClientError`] when the file cannot be opened or switched to WAL.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        Ok(Self {
            db: Db::open(path.as_ref())?,
        })
    }

    /// Writes `env` as pending and due now.
    ///
    /// # Errors
    ///
    /// [`ClientError::DuplicateMessage`] when the outbox already holds its
    /// id, and [`ClientError::Store`] when the write fails.
    pub fn enqueue(&self, env: &ClientEnvelope) -> Result<(), ClientError> {
        let json = serde_json::to_string(env).map_err(ClientError::Encode)?;
        let inserted = self.db.lock().execute(
            "INSERT INTO outbox (id, envelope, next_at_ms) VALUES (?1, ?2, ?3)",
            params![env.id.as_str(), json, Timestamp::now().as_millisecond()],
        );
        match inserted {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == ErrorCode::ConstraintViolation =>
            {
                Err(ClientError::DuplicateMessage(env.id.clone()))
            }
            Err(err) => Err(err.into()),
        }
    }

    /// The pending envelopes due at `now`, oldest first, at most 256.
    ///
    /// # Errors
    ///
    /// [`ClientError`] when the read fails or a stored envelope no longer
    /// parses.
    pub fn due(&self, now: Timestamp) -> Result<Vec<ClientEnvelope>, ClientError> {
        let conn = self.db.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT envelope FROM outbox WHERE state = 'pending' AND next_at_ms <= ?1
             ORDER BY n LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![now.as_millisecond(), DUE_LIMIT], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|json| {
            ClientEnvelope::from_json(json?.as_bytes())
                .map_err(|err| ClientError::Stored(format!("an outbox envelope: {err}")))
        })
        .collect()
    }

    /// Records that message `id` was just published: one more attempt, and
    /// due again after the backoff (1 s doubling to 60 s, with 20% jitter)
    /// unless the hub answers first. A message the hub already answered
    /// keeps its state, and a retryable NACK that arrived before this call
    /// keeps its delay when that delay ends later than the backoff.
    ///
    /// # Errors
    ///
    /// [`ClientError::UnknownMessage`] when the outbox does not hold `id`,
    /// and [`ClientError::Store`] when the write fails.
    pub fn sent(&self, id: &MessageId, now: Timestamp) -> Result<OutboxState, ClientError> {
        let mut conn = self.db.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = load_state(&tx, id)?;
        let OutboxState::Pending { attempts } = state else {
            return Ok(state);
        };
        let attempts = attempts.saturating_add(1);
        let next_at = later(now, jittered(backoff(attempts)));
        // The hub's reply can beat this call: keep a later due time it set.
        tx.execute(
            "UPDATE outbox SET attempts = ?2, next_at_ms = max(next_at_ms, ?3) WHERE id = ?1",
            params![id.as_str(), attempts, next_at],
        )?;
        tx.commit()?;
        Ok(OutboxState::Pending { attempts })
    }

    /// Applies the hub's answer to one of this outbox's messages: an ACK
    /// records it, a final NACK rejects it for good, and a retryable NACK
    /// makes it due again after `retry_after_ms`, or after the backoff when
    /// the hub names no delay. A message that is already recorded or
    /// rejected keeps that state.
    ///
    /// # Errors
    ///
    /// [`ClientError::UnknownMessage`] when the outbox does not hold the
    /// reply's id, and [`ClientError::Store`] when the write fails.
    pub fn apply(&self, reply: &DeliveryReply) -> Result<OutboxState, ClientError> {
        let id = match reply {
            DeliveryReply::Ack { id, .. } | DeliveryReply::Nack { id, .. } => id,
        };
        let mut conn = self.db.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let OutboxState::Pending { attempts } = load_state(&tx, id)? else {
            return load_state(&tx, id);
        };
        let state = match reply {
            DeliveryReply::Ack { stream, seq, .. } => {
                tx.execute(
                    "UPDATE outbox SET state = 'recorded', stream = ?2, seq = ?3 WHERE id = ?1",
                    params![id.as_str(), stream.to_string(), to_sql_seq(seq.0)?],
                )?;
                OutboxState::Recorded {
                    stream: stream.clone(),
                    seq: *seq,
                }
            }
            DeliveryReply::Nack {
                retryable: false,
                reason,
                ..
            } => {
                tx.execute(
                    "UPDATE outbox SET state = 'rejected', reason = ?2 WHERE id = ?1",
                    params![id.as_str(), reason.as_str()],
                )?;
                OutboxState::Rejected { reason: *reason }
            }
            DeliveryReply::Nack {
                retryable: true,
                retry_after_ms,
                ..
            } => {
                // The hub's delay is exact, and a reconnect waits it out
                // (the hold); only the client's own guess is spread out.
                let now = Timestamp::now();
                let (next_at, hold) = match retry_after_ms {
                    Some(ms) => {
                        let until = later(now, Duration::from_millis(*ms));
                        (until, Some(until))
                    }
                    None => (later(now, jittered(backoff(attempts.max(1)))), None),
                };
                tx.execute(
                    "UPDATE outbox SET next_at_ms = ?2, hold_until_ms = ?3 WHERE id = ?1",
                    params![id.as_str(), next_at, hold],
                )?;
                OutboxState::Pending { attempts }
            }
        };
        tx.commit()?;
        Ok(state)
    }

    /// Where message `id` stands, or `None` when the outbox never held it.
    ///
    /// # Errors
    ///
    /// [`ClientError`] when the read fails or the row holds a value this
    /// version cannot read.
    pub fn state(&self, id: &MessageId) -> Result<Option<OutboxState>, ClientError> {
        let conn = self.db.lock();
        match load_state(&conn, id) {
            Ok(state) => Ok(Some(state)),
            Err(ClientError::UnknownMessage(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Makes every pending message due at `now`, so a new connection sends
    /// what the last one may have lost without waiting out the backoff. A
    /// message the hub told to wait (`retry_after_ms`) falls due when that
    /// wait ends instead, never earlier.
    pub(crate) fn retry_now(&self, now: Timestamp) -> Result<(), ClientError> {
        self.db.lock().execute(
            "UPDATE outbox SET next_at_ms = max(?1, coalesce(hold_until_ms, ?1))
             WHERE state = 'pending' AND next_at_ms > ?1",
            params![now.as_millisecond()],
        )?;
        Ok(())
    }

    /// The earliest time a pending message falls due, if any is pending.
    pub(crate) fn next_due(&self) -> Result<Option<Timestamp>, ClientError> {
        let next: Option<i64> = self.db.lock().query_row(
            "SELECT min(next_at_ms) FROM outbox WHERE state = 'pending'",
            [],
            |row| row.get(0),
        )?;
        next.map(|ms| {
            Timestamp::from_millisecond(ms)
                .map_err(|err| ClientError::Stored(format!("an outbox due time: {err}")))
        })
        .transpose()
    }
}

/// The state of message `id`, read inside `conn`'s current transaction.
fn load_state(conn: &rusqlite::Connection, id: &MessageId) -> Result<OutboxState, ClientError> {
    let row: Option<StateRow> = conn
        .query_row(
            "SELECT state, attempts, stream, seq, reason FROM outbox WHERE id = ?1",
            params![id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((state, attempts, stream, seq, reason)) = row else {
        return Err(ClientError::UnknownMessage(id.clone()));
    };
    let bad = |what: &str| ClientError::Stored(format!("outbox row {id}: {what}"));
    match (state.as_str(), stream, seq, reason) {
        ("pending", ..) => Ok(OutboxState::Pending {
            attempts: u32::try_from(attempts).map_err(|_| bad("attempts"))?,
        }),
        ("recorded", Some(stream), Some(seq), _) => Ok(OutboxState::Recorded {
            stream: stream.parse().map_err(|_| bad("stream"))?,
            seq: Seq(from_sql_int(seq)?),
        }),
        ("rejected", _, _, Some(reason)) => Ok(OutboxState::Rejected {
            reason: NackReason::deserialize(reason.as_str().into_deserializer())
                .map_err(|_: serde::de::value::Error| bad("reason"))?,
        }),
        _ => Err(bad("state")),
    }
}

/// The retry delay after `attempts` sends (1 for the first): 1 s doubling
/// to 60 s.
fn backoff(attempts: u32) -> Duration {
    let doublings = attempts.saturating_sub(1).min(16);
    FIRST_RETRY.saturating_mul(1 << doublings).min(MAX_RETRY)
}

/// `delay` spread by up to [`JITTER_PERCENT`] either way.
fn jittered(delay: Duration) -> Duration {
    // Without randomness the delay is used as is: clients then retry in
    // step, which is slower to recover from but still correct.
    let roll = getrandom::u32().unwrap_or(u32::MAX / 2);
    spread(delay, roll)
}

/// `delay` times a factor between 80% and 120% that `roll` picks.
fn spread(delay: Duration, roll: u32) -> Duration {
    let span = 2 * JITTER_PERCENT + 1;
    let percent = 100 - JITTER_PERCENT + roll % span;
    delay * percent / 100
}

/// `now` plus `delay`, as Unix milliseconds.
fn later(now: Timestamp, delay: Duration) -> i64 {
    let delay = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
    now.as_millisecond().saturating_add(delay)
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::*;

    fn fyi(id: &str) -> ClientEnvelope {
        let raw = format!(
            r#"{{"v":1,"id":"{id}","verb":"FYI","data":{{}},"body":"hi","sent_at":"2026-10-25T16:04:11Z"}}"#
        );
        ClientEnvelope::from_json(raw.as_bytes()).unwrap()
    }

    fn retry_later(id: &MessageId, retry_after_ms: Option<u64>) -> DeliveryReply {
        DeliveryReply::Nack {
            id: id.clone(),
            retryable: true,
            retry_after_ms,
            reason: NackReason::RateLimited,
        }
    }

    #[test]
    fn retry_now_waits_out_the_hubs_delay() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(dir.path().join("identity.db")).unwrap();
        let (held, backed_off, released) = (
            fyi("01JBX6Q4T7W3C2M8Y9K5N0P1R1"),
            fyi("01JBX6Q4T7W3C2M8Y9K5N0P1R2"),
            fyi("01JBX6Q4T7W3C2M8Y9K5N0P1R3"),
        );
        for env in [&held, &backed_off, &released] {
            outbox.enqueue(env).unwrap();
        }
        outbox.apply(&retry_later(&held.id, Some(5000))).unwrap();
        // A later NACK without a delay lifts the hub's earlier hold.
        outbox
            .apply(&retry_later(&released.id, Some(5000)))
            .unwrap();
        outbox.apply(&retry_later(&released.id, None)).unwrap();
        let now = Timestamp::now();
        outbox.sent(&backed_off.id, now).unwrap();
        let at = |secs| now.checked_add(SignedDuration::from_secs(secs)).unwrap();
        assert!(outbox.due(now).unwrap().is_empty());

        outbox.retry_now(now).unwrap();
        let ids =
            |envs: Vec<ClientEnvelope>| envs.into_iter().map(|env| env.id).collect::<Vec<_>>();
        assert_eq!(
            ids(outbox.due(now).unwrap()),
            [backed_off.id.clone(), released.id.clone()],
            "only the client's own backoff is skipped"
        );
        assert_eq!(
            ids(outbox.due(at(4)).unwrap()),
            [backed_off.id.clone(), released.id.clone()]
        );
        assert_eq!(
            ids(outbox.due(at(5)).unwrap()),
            [held.id, backed_off.id, released.id]
        );
    }

    #[test]
    fn backoff_doubles_to_the_cap() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(6), Duration::from_secs(32));
        assert_eq!(backoff(7), MAX_RETRY);
        assert_eq!(backoff(u32::MAX), MAX_RETRY);
    }

    #[test]
    fn jitter_stays_within_twenty_percent() {
        let second = Duration::from_secs(1);
        assert_eq!(spread(second, 0), Duration::from_millis(800));
        assert_eq!(spread(second, 40), Duration::from_millis(1200));
        assert_eq!(spread(second, 41), Duration::from_millis(800));
        for _ in 0..100 {
            let delay = jittered(second);
            assert!((800..=1200).contains(&delay.as_millis()), "{delay:?}");
        }
    }

    #[test]
    fn state_serializes_as_r31_says() {
        let json = |state: &OutboxState| serde_json::to_value(state).unwrap();
        assert_eq!(
            json(&OutboxState::Pending { attempts: 2 }),
            serde_json::json!({"state": "pending", "attempts": 2})
        );
        let id: agmen_proto::IdentityId = "idn7k2q9".parse().unwrap();
        assert_eq!(
            json(&OutboxState::Recorded {
                stream: Stream::Inbox(id),
                seq: Seq(4)
            }),
            serde_json::json!({"state": "recorded", "stream": "inbox:idn7k2q9", "seq": 4})
        );
        assert_eq!(
            json(&OutboxState::Rejected {
                reason: NackReason::BodyTooLarge
            }),
            serde_json::json!({"state": "rejected", "reason": "body_too_large"})
        );
    }
}
