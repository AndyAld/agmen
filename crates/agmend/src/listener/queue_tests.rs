use super::*;
use agmen_proto::{IdentityKind, Mention, Payload, Seq, Verb};

fn identity(name: &str) -> IdentityId {
    name.parse().unwrap()
}

fn message_id(n: u64) -> MessageId {
    format!("{n:026}").parse().unwrap()
}

fn message(n: u64, from: &str, kind: IdentityKind, verb: Verb) -> StampedEnvelope {
    let data = match verb {
        Verb::Status => serde_json::json!({"summary": "working"}),
        Verb::Done => serde_json::json!({"item": "B-1", "branch": "fix/test",
            "commit": "abcdef0", "base_sha": "abcdef1", "checks": [], "changes": []}),
        Verb::Review => serde_json::json!({"verdict": "pass", "findings": []}),
        Verb::Alert => serde_json::json!({"need": "help", "actions": []}),
        _ => serde_json::json!({}),
    };
    serde_json::from_value(serde_json::json!({
        "v": 1, "id": message_id(n), "reply_to": null, "to": [], "mentions": [],
        "items": [], "lane": null, "body": "test", "valid_until": null,
        "sent_at": "2026-10-27T12:00:00Z", "verb": verb, "data": data,
        "stream": "feed", "seq": n, "from": from, "from_handle": "display-only",
        "from_kind": kind, "mentioned": [], "refs": [],
        "hub_at": "2026-10-27T12:00:00Z"
    }))
    .unwrap()
}

fn queue() -> (mpsc::Sender<Incoming>, Queue) {
    Queue::new(identity("listener1"), identity("person01"))
}

fn send(tx: &mpsc::Sender<Incoming>, message: StampedEnvelope) {
    let thread = message_id(1);
    tx.try_send(Incoming { message, thread }).unwrap();
}

fn seconds(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn filtering_owner_plain_chat_and_other_mentions() {
    let (tx, mut queue) = queue();
    for (n, verb) in [(1, Verb::Fyi), (2, Verb::Ask), (3, Verb::Answer)] {
        send(&tx, message(n, "person01", IdentityKind::Human, verb));
    }
    let mut elsewhere = message(4, "person01", IdentityKind::Human, Verb::Fyi);
    elsewhere.client.mentions = vec![Mention::Identity(identity("another1"))];
    send(&tx, elsewhere);
    send(&tx, message(5, "person01", IdentityKind::Human, Verb::Bye));
    send(&tx, message(6, "worker01", IdentityKind::Agent, Verb::Fyi));
    send(&tx, message(7, "stranger", IdentityKind::Human, Verb::Fyi));
    // Stamped identity, rather than the display handle, selects the owner.
    let batch = queue.poll(seconds(0), &[]).unwrap();
    assert_eq!(
        batch.messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
        [Seq(1), Seq(2), Seq(3)]
    );
}

#[test]
fn filtering_listener_and_person_routes() {
    let (tx, mut queue) = queue();
    for (index, target) in ["listener1", "person01"].into_iter().enumerate() {
        let n = index as u64 * 3;
        let mut mentioned = message(n + 1, "stranger", IdentityKind::Human, Verb::Fyi);
        mentioned.mentioned = vec![identity(target)];
        mentioned.client.mentions = vec![Mention::All];
        send(&tx, mentioned);
        let mut dm = message(n + 2, "stranger", IdentityKind::Human, Verb::Dm);
        dm.client.to = vec![identity(target)];
        send(&tx, dm);
        let mut assign = message(n + 3, "coord001", IdentityKind::Coordinator, Verb::Fyi);
        assign.client.payload = Payload::Assign {
            item: "B-1".parse().unwrap(),
            to: identity(target),
        };
        send(&tx, assign);
    }
    let mut self_echo = message(7, "listener1", IdentityKind::Listener, Verb::Fyi);
    self_echo.mentioned = vec![identity("person01")];
    send(&tx, self_echo);
    let mut elsewhere = message(8, "person01", IdentityKind::Human, Verb::Dm);
    elsewhere.client.to = vec![identity("another1")];
    send(&tx, elsewhere);
    let batch = queue.poll(seconds(0), &[]).unwrap();
    assert_eq!(batch.messages.len(), 6);
    assert_eq!(batch.messages[0].from_handle, "display-only");
    assert_eq!(batch.messages[0].from_kind, IdentityKind::Human);
}

#[test]
fn filtering_only_started_workers_reports() {
    let workers = [identity("worker01")];
    let (tx, mut queue) = queue();
    for (n, verb) in [
        (1, Verb::Done),
        (2, Verb::Status),
        (3, Verb::Review),
        (4, Verb::Alert),
    ] {
        send(&tx, message(n, "worker01", IdentityKind::Agent, verb));
        send(&tx, message(n + 10, "worker02", IdentityKind::Agent, verb));
    }
    send(&tx, message(20, "worker01", IdentityKind::Agent, Verb::Fyi));
    assert_eq!(queue.poll(seconds(0), &workers).unwrap().messages.len(), 4);
}

#[test]
fn batches_at_29_30_and_31_seconds() {
    for (elapsed, ready) in [(29, false), (30, true), (31, true)] {
        let (tx, mut queue) = queue();
        send(&tx, message(1, "person01", IdentityKind::Human, Verb::Fyi));
        assert!(queue.poll(seconds(0), &[]).is_some());
        send(&tx, message(2, "person01", IdentityKind::Human, Verb::Fyi));
        send(&tx, message(3, "person01", IdentityKind::Human, Verb::Fyi));
        let batch = queue.poll(seconds(elapsed), &[]);
        assert_eq!(batch.is_some(), ready, "elapsed={elapsed}");
        let batch = batch.or_else(|| queue.poll(seconds(30), &[])).unwrap();
        assert_eq!(
            batch.messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
            [Seq(2), Seq(3)]
        );
        assert!(queue.poll(seconds(elapsed.max(30)), &[]).is_none());
    }
}

#[test]
fn batch_id_stays_with_batch_and_empty_polls_do_not_consume_ids() {
    let (tx, mut queue) = queue();
    assert!(queue.poll(seconds(0), &[]).is_none());
    send(&tx, message(1, "person01", IdentityKind::Human, Verb::Fyi));
    let first = queue.poll(seconds(10), &[]).unwrap();
    let original = first.batch_id;
    send(&tx, message(2, "person01", IdentityKind::Human, Verb::Fyi));
    assert!(queue.poll(seconds(39), &[]).is_none());
    assert!(queue.poll(seconds(9), &[]).is_none());
    let second = queue.poll(seconds(40), &[]).unwrap();
    assert_eq!(first.batch_id, original);
    assert_eq!(first.batch_id, BatchId(0));
    assert_eq!(second.batch_id, BatchId(1));
    assert_eq!(first.messages[0].client.id, message_id(1));
    assert_eq!(second.messages[0].client.id, message_id(2));
}

#[test]
fn thread_cap_at_two_three_and_four_posts() {
    let (_tx, mut queue) = queue();
    let thread = message_id(1);
    assert_eq!(queue.try_post(&thread), PostDecision::Allow);
    assert_eq!(queue.try_post(&thread), PostDecision::Allow);
    assert_eq!(queue.try_post(&thread), PostDecision::Allow);
    assert_eq!(queue.try_post(&thread), PostDecision::ThreadCap);
    assert_eq!(queue.try_post(&thread), PostDecision::ThreadCap);
    assert_eq!(queue.try_post(&message_id(2)), PostDecision::Allow);
}

#[test]
fn only_human_or_worker_posts_reset_same_resolved_thread() {
    for (from, kind, reset) in [
        ("stranger", IdentityKind::Human, true),
        ("worker02", IdentityKind::Agent, true),
        ("listen02", IdentityKind::Listener, false),
        ("coord001", IdentityKind::Coordinator, false),
        ("hub00001", IdentityKind::Hub, false),
    ] {
        let (tx, mut queue) = queue();
        for _ in 0..3 {
            assert_eq!(queue.try_post(&message_id(1)), PostDecision::Allow);
        }
        let mut reply = message(9, from, kind, Verb::Fyi);
        reply.client.reply_to = Some(message_id(8));
        send(&tx, reply);
        queue.poll(seconds(0), &[]);
        assert_eq!(
            queue.try_post(&message_id(1)),
            if reset {
                PostDecision::Allow
            } else {
                PostDecision::ThreadCap
            }
        );
    }
}

#[test]
fn bounded_source_and_pending_preserve_order_with_backpressure() {
    let (tx, mut queue) = queue();
    send(&tx, message(1, "person01", IdentityKind::Human, Verb::Fyi));
    queue.poll(seconds(0), &[]).unwrap();
    for n in 0..CAPACITY {
        send(
            &tx,
            message(n as u64 + 2, "person01", IdentityKind::Human, Verb::Fyi),
        );
    }
    assert!(
        tx.try_send(Incoming {
            message: message(900, "person01", IdentityKind::Human, Verb::Fyi),
            thread: message_id(1),
        })
        .is_err()
    );
    assert!(queue.poll(seconds(29), &[]).is_none());
    assert_eq!(queue.pending.len(), CAPACITY);
    send(
        &tx,
        message(901, "person01", IdentityKind::Human, Verb::Fyi),
    );
    let batch = queue.poll(seconds(30), &[]).unwrap();
    assert_eq!(batch.messages.len(), CAPACITY);
    assert_eq!(batch.messages[0].seq, Seq(2));
    assert_eq!(batch.messages[CAPACITY - 1].seq, Seq(CAPACITY as u64 + 1));
    assert!(queue.poll(seconds(59), &[]).is_none());
    assert_eq!(
        queue.poll(seconds(60), &[]).unwrap().messages[0].seq,
        Seq(901)
    );
}

#[test]
fn thread_tracking_fails_closed_at_capacity_without_eviction() {
    let (tx, mut queue) = queue();
    for n in 1..=MAX_THREADS as u64 {
        for _ in 0..3 {
            assert_eq!(queue.try_post(&message_id(n)), PostDecision::Allow);
        }
    }
    assert_eq!(queue.try_post(&message_id(999)), PostDecision::Capacity);
    assert_eq!(queue.try_post(&message_id(1)), PostDecision::ThreadCap);
    send(
        &tx,
        message(900, "person01", IdentityKind::Human, Verb::Fyi),
    );
    queue.poll(seconds(0), &[]);
    assert_eq!(queue.try_post(&message_id(999)), PostDecision::Allow);
}

#[test]
fn unrelated_routes_do_not_start_a_batch_or_its_interval() {
    let (tx, mut queue) = queue();
    let mut raw_mention = message(1, "stranger", IdentityKind::Human, Verb::Fyi);
    raw_mention.client.mentions = vec![Mention::Identity(identity("listener1"))];
    // Only hub-expanded mentions are authoritative.
    send(&tx, raw_mention);
    let mut ask = message(2, "person01", IdentityKind::Human, Verb::Ask);
    ask.client.payload = Payload::Ask {
        of: Some(identity("another1")),
    };
    send(&tx, ask);
    let mut assign = message(3, "coord001", IdentityKind::Coordinator, Verb::Fyi);
    assign.client.payload = Payload::Assign {
        item: "B-1".parse().unwrap(),
        to: identity("another1"),
    };
    send(&tx, assign);
    send(
        &tx,
        message(4, "listener1", IdentityKind::Listener, Verb::Fyi),
    );
    assert!(queue.poll(seconds(100), &[]).is_none());
    send(&tx, message(5, "person01", IdentityKind::Human, Verb::Fyi));
    assert!(queue.poll(seconds(101), &[]).is_some());
    drop(tx);
    assert!(queue.poll(seconds(131), &[]).is_none());
}

#[test]
fn a_reply_in_another_thread_does_not_reset_the_cap() {
    let (tx, mut queue) = queue();
    let thread = message_id(2);
    for _ in 0..3 {
        assert_eq!(queue.try_post(&thread), PostDecision::Allow);
    }
    send(&tx, message(3, "person01", IdentityKind::Human, Verb::Fyi));
    queue.poll(seconds(0), &[]);
    assert_eq!(queue.try_post(&thread), PostDecision::ThreadCap);
}

#[test]
fn newly_delegated_workers_reach_the_existing_queue() {
    let (tx, mut queue) = queue();
    send(&tx, message(1, "person01", IdentityKind::Human, Verb::Fyi));
    let first = queue.poll(seconds(0), &[]).unwrap();
    let workers = [identity("worker01")];
    // The running listener delegates after its first batch.
    send(&tx, message(2, "worker01", IdentityKind::Agent, Verb::Done));
    let next = queue.poll(seconds(30), &workers);
    assert!(
        next.is_some(),
        "newly delegated worker must reach the queue"
    );
    assert_ne!(next.unwrap().batch_id, first.batch_id);
}
