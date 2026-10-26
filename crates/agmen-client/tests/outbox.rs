//! The outbox and the cursors over a real SQLite file, no broker.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

use std::path::{Path, PathBuf};

use agmen_client::{Advance, Cursors, Outbox, OutboxState};
use agmen_proto::{ClientEnvelope, DeliveryReply, MessageId, NackReason, Seq, Stream, Timestamp};
use jiff::SignedDuration;
use tempfile::TempDir;

/// A ULID whose last character is `n`'s (base 32).
fn message_id(n: usize) -> MessageId {
    const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut id = String::from("01JBX6Q4T7W3C2M8Y9K5N0P1");
    id.push(char::from(CROCKFORD[(n / 32) % 32]));
    id.push(char::from(CROCKFORD[n % 32]));
    id.parse().unwrap()
}

fn fyi(n: usize) -> ClientEnvelope {
    let raw = format!(
        r#"{{"v":1,"id":"{}","verb":"FYI","data":{{}},"body":"message {n}","sent_at":"2026-10-25T16:04:11Z"}}"#,
        message_id(n)
    );
    ClientEnvelope::from_json(raw.as_bytes()).unwrap()
}

fn db_file() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("identity.db");
    (dir, path)
}

fn ids(envs: &[ClientEnvelope]) -> Vec<MessageId> {
    envs.iter().map(|env| env.id.clone()).collect()
}

fn after(now: Timestamp, secs: i64) -> Timestamp {
    now.checked_add(SignedDuration::from_secs(secs)).unwrap()
}

#[test]
fn outbox_survives_reopen() {
    let (_dir, path) = db_file();
    {
        let outbox = Outbox::open(&path).unwrap();
        for n in [1, 2, 3] {
            outbox.enqueue(&fyi(n)).unwrap();
        }
    }
    let outbox = Outbox::open(&path).unwrap();
    let due = outbox.due(Timestamp::now()).unwrap();
    assert_eq!(ids(&due), [message_id(1), message_id(2), message_id(3)]);
    assert_eq!(due[1], fyi(2), "the envelope comes back as it went in");
}

#[test]
fn outbox_stops_on_final_nack() {
    let (_dir, path) = db_file();
    let outbox = Outbox::open(&path).unwrap();
    outbox.enqueue(&fyi(1)).unwrap();
    outbox.enqueue(&fyi(2)).unwrap();
    let state = outbox
        .apply(&DeliveryReply::Nack {
            id: message_id(1),
            retryable: false,
            retry_after_ms: None,
            reason: NackReason::IdentityRevoked,
        })
        .unwrap();
    assert_eq!(
        state,
        OutboxState::Rejected {
            reason: NackReason::IdentityRevoked
        }
    );
    let far = after(Timestamp::now(), 365 * 24 * 3600);
    assert_eq!(ids(&outbox.due(far).unwrap()), [message_id(2)]);
    // A later answer does not bring it back.
    let again = outbox
        .apply(&DeliveryReply::Nack {
            id: message_id(1),
            retryable: true,
            retry_after_ms: None,
            reason: NackReason::HubBusy,
        })
        .unwrap();
    assert_eq!(again, state);
    assert_eq!(ids(&outbox.due(far).unwrap()), [message_id(2)]);
    assert_eq!(outbox.state(&message_id(1)).unwrap(), Some(state));
}

#[test]
fn outbox_honors_retry_after() {
    let (_dir, path) = db_file();
    let outbox = Outbox::open(&path).unwrap();
    outbox.enqueue(&fyi(1)).unwrap();
    let state = outbox
        .apply(&DeliveryReply::Nack {
            id: message_id(1),
            retryable: true,
            retry_after_ms: Some(5000),
            reason: NackReason::RateLimited,
        })
        .unwrap();
    assert_eq!(state, OutboxState::Pending { attempts: 0 });
    let now = Timestamp::now();
    assert!(outbox.due(after(now, 4)).unwrap().is_empty());
    assert_eq!(ids(&outbox.due(after(now, 5)).unwrap()), [message_id(1)]);
}

#[test]
fn outbox_records_ack_and_backs_off_after_a_send() {
    let (_dir, path) = db_file();
    let outbox = Outbox::open(&path).unwrap();
    outbox.enqueue(&fyi(1)).unwrap();
    outbox.enqueue(&fyi(2)).unwrap();
    let now = Timestamp::now();
    assert_eq!(
        outbox.sent(&message_id(1), now).unwrap(),
        OutboxState::Pending { attempts: 1 }
    );
    // 1 s with 20% jitter: not due before 0.8 s, due by 1.2 s.
    let at = |ms: i64| now.checked_add(SignedDuration::from_millis(ms)).unwrap();
    assert_eq!(ids(&outbox.due(at(790)).unwrap()), [message_id(2)]);
    assert_eq!(
        ids(&outbox.due(at(1210)).unwrap()),
        [message_id(1), message_id(2)]
    );
    let recorded = outbox
        .apply(&DeliveryReply::Ack {
            id: message_id(1),
            stream: Stream::Feed,
            seq: Seq(7),
        })
        .unwrap();
    assert_eq!(
        recorded,
        OutboxState::Recorded {
            stream: Stream::Feed,
            seq: Seq(7)
        }
    );
    assert_eq!(ids(&outbox.due(at(3_600_000)).unwrap()), [message_id(2)]);
    assert_eq!(outbox.state(&message_id(3)).unwrap(), None);
    assert!(
        outbox
            .apply(&DeliveryReply::Ack {
                id: message_id(3),
                stream: Stream::Feed,
                seq: Seq(8),
            })
            .is_err(),
        "a reply for a message this outbox never held is an error"
    );
}

#[test]
fn reader_drops_duplicate_seq() {
    let (_dir, path) = db_file();
    let cursors = Cursors::open(&path).unwrap();
    assert_eq!(cursors.last(&Stream::Feed).unwrap(), 0);
    assert_eq!(cursors.advance(&Stream::Feed, 1).unwrap(), Advance::Next);
    assert_eq!(
        cursors.advance(&Stream::Feed, 1).unwrap(),
        Advance::Duplicate
    );
    assert_eq!(cursors.last(&Stream::Feed).unwrap(), 1);
}

#[test]
fn reader_detects_gap() {
    let (_dir, path) = db_file();
    let cursors = Cursors::open(&path).unwrap();
    assert_eq!(cursors.advance(&Stream::Feed, 1).unwrap(), Advance::Next);
    assert_eq!(
        cursors.advance(&Stream::Feed, 4).unwrap(),
        Advance::Gap { from: 2, to: 3 }
    );
    assert_eq!(
        cursors.last(&Stream::Feed).unwrap(),
        1,
        "a gap does not move the cursor"
    );
    // Each stream has its own cursor, and it survives a reopen.
    let inbox = Stream::Inbox("idn7k2q9".parse().unwrap());
    assert_eq!(cursors.advance(&inbox, 1).unwrap(), Advance::Next);
    drop(cursors);
    let cursors = Cursors::open(&path).unwrap();
    assert_eq!(cursors.last(&Stream::Feed).unwrap(), 1);
    assert_eq!(cursors.last(&inbox).unwrap(), 1);
}

#[test]
fn read_cursor_is_separate_from_delivery() {
    let (_dir, path) = db_file();
    let cursors = Cursors::open(&path).unwrap();
    for seq in 1..=5 {
        assert_eq!(cursors.advance(&Stream::Feed, seq).unwrap(), Advance::Next);
    }
    assert_eq!(cursors.read_cursor(&Stream::Feed).unwrap(), 0);
    cursors.mark_read(&Stream::Feed, 3).unwrap();
    assert_eq!(cursors.read_cursor(&Stream::Feed).unwrap(), 3);
    cursors.mark_read(&Stream::Feed, 2).unwrap();
    assert_eq!(
        cursors.read_cursor(&Stream::Feed).unwrap(),
        3,
        "the read cursor never moves back"
    );
    assert_eq!(cursors.last(&Stream::Feed).unwrap(), 5);
}

/// Opens the outbox and the cursors from two threads on separate
/// connections, as the MCP server and `agmen watch` do from two processes.
#[test]
fn two_processes_share_file() {
    const EACH: usize = 100;
    let (_dir, path) = db_file();
    let writer = |path: &Path| {
        let path = path.to_owned();
        std::thread::spawn(move || {
            let outbox = Outbox::open(&path).unwrap();
            for n in 0..EACH {
                outbox.enqueue(&fyi(n)).unwrap();
            }
        })
    };
    let reader = |path: &Path| {
        let path = path.to_owned();
        std::thread::spawn(move || {
            let cursors = Cursors::open(&path).unwrap();
            for seq in 1..=EACH {
                let seq = u64::try_from(seq).unwrap();
                assert_eq!(cursors.advance(&Stream::Feed, seq).unwrap(), Advance::Next);
            }
        })
    };
    let (outbox_thread, cursor_thread) = (writer(&path), reader(&path));
    outbox_thread.join().unwrap();
    cursor_thread.join().unwrap();
    let outbox = Outbox::open(&path).unwrap();
    assert_eq!(outbox.due(Timestamp::now()).unwrap().len(), EACH);
    let cursors = Cursors::open(&path).unwrap();
    assert_eq!(
        cursors.last(&Stream::Feed).unwrap(),
        u64::try_from(EACH).unwrap()
    );
}
