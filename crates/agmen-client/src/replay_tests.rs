//! Delivery against a fake replay endpoint, no broker.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agmen_proto::{IdentityId, Seq};
use axum::Json;
use axum::extract::{Query, State};
use axum::routing::get;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::outbox::OutboxState;

const ME: &str = "idmememe";
const PROJECT: &str = "project1";

/// Feed message `seq` as the hub stamps it.
fn feed_message(seq: u64) -> StampedEnvelope {
    serde_json::from_value(json!({
        "v": 1, "id": format!("01JBX6Q4T7W3C2M8Y9K5N0P{seq:03}"), "verb": "FYI", "data": {},
        "body": format!("message {seq}"), "sent_at": "2026-10-25T16:04:11Z",
        "stream": "feed", "seq": seq, "from": "idother1", "from_handle": "x/y",
        "from_kind": "agent", "hub_at": "2026-10-25T16:04:11Z",
    }))
    .unwrap()
}

/// The feed the fake hub serves, how many pages it was asked for, and a
/// gate a test can hold to keep a page from being answered.
#[derive(Debug, Clone, Default)]
struct FakeHub {
    feed: Arc<Mutex<Vec<StampedEnvelope>>>,
    pruned_through: Arc<Mutex<u64>>,
    pages: Arc<Mutex<u32>>,
    gate: Arc<tokio::sync::Mutex<()>>,
}

async fn page(
    State(hub): State<FakeHub>,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    *hub.pages.lock().unwrap() += 1;
    drop(hub.gate.lock().await);
    let after: u64 = query["after"].parse().unwrap();
    let feed = hub.feed.lock().unwrap();
    let messages: Vec<_> = feed.iter().filter(|m| m.seq.0 > after).cloned().collect();
    let head = feed.last().map_or(0, |m| m.seq.0);
    let pruned_through = *hub.pruned_through.lock().unwrap();
    Json(json!({
        "messages": messages, "next_after": null, "head": head,
        "pruned_through": pruned_through,
    }))
}

/// The link's side of delivery, a running replayer, and the events.
struct Harness {
    _dir: TempDir,
    fake: FakeHub,
    cursors: Cursors,
    outbox: Outbox,
    streams: Streams,
    events: mpsc::Receiver<ClientEvent>,
}

async fn start() -> Harness {
    let fake = FakeHub::default();
    let app = axum::Router::new()
        .route("/v1/projects/{p}/streams/{s}", get(page))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let me: IdentityId = ME.parse().unwrap();
    let hub = HubClient::new(
        base.parse().unwrap(),
        PROJECT.parse().unwrap(),
        me.clone(),
        "t",
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("agmen.db");
    let (cursors, outbox) = (Cursors::open(&db).unwrap(), Outbox::open(&db).unwrap());
    let (events_tx, events_rx) = mpsc::channel(64);
    let (streams, replayer) = delivery(
        vec![Stream::Feed, Stream::Inbox(me)],
        cursors.clone(),
        outbox.clone(),
        hub,
        events_tx,
    );
    tokio::spawn(replayer.run());
    Harness {
        _dir: dir,
        fake,
        cursors,
        outbox,
        streams,
        events: events_rx,
    }
}

impl Harness {
    /// The hub records feed messages 1 to `last`.
    fn hub_has(&self, last: u64) {
        *self.fake.feed.lock().unwrap() = (1..=last).map(feed_message).collect();
    }

    async fn live(&self, seq: u64) {
        let payload = serde_json::to_vec(&feed_message(seq)).unwrap();
        self.streams
            .on_message(Stream::Feed, &payload)
            .await
            .unwrap();
    }

    /// Waits until no replay is queued or running.
    async fn settle(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !self.streams.idle() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "replay did not finish"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The seqs of the message events so far.
    async fn delivered(&mut self) -> Vec<u64> {
        self.settle().await;
        let mut seqs = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            if let ClientEvent::Message { message } = event {
                seqs.push(message.seq.0);
            }
        }
        seqs
    }
}

#[tokio::test]
async fn replay_skips_pruned_seqs() {
    let mut h = start().await;
    *h.fake.feed.lock().unwrap() = (7..=8).map(feed_message).collect();
    *h.fake.pruned_through.lock().unwrap() = 6;
    assert_eq!(h.cursors.last(&Stream::Feed).unwrap(), 0);
    h.streams.on_head(br#"{"feed_seq":8}"#).await.unwrap();
    assert_eq!(h.delivered().await, [7, 8]);
    assert_eq!(h.cursors.last(&Stream::Feed).unwrap(), 8);
}

#[tokio::test]
async fn a_pruned_through_past_the_page_never_skips_its_messages() {
    // A buggy hub that claims more was pruned than it still serves must not
    // push the cursor past messages on the page.
    let mut h = start().await;
    *h.fake.feed.lock().unwrap() = (7..=8).map(feed_message).collect();
    *h.fake.pruned_through.lock().unwrap() = 20;
    h.streams.on_head(br#"{"feed_seq":8}"#).await.unwrap();
    assert_eq!(h.delivered().await, [7, 8]);
    assert_eq!(h.cursors.last(&Stream::Feed).unwrap(), 8);
}

#[test]
fn skip_to_never_lowers() {
    let dir = tempfile::tempdir().unwrap();
    let cursors = Cursors::open(dir.path().join("identity.db")).unwrap();
    for seq in 1..=9 {
        assert_eq!(cursors.advance(&Stream::Feed, seq).unwrap(), Advance::Next);
    }
    cursors.mark_read(&Stream::Feed, 4).unwrap();
    cursors.skip_to(&Stream::Feed, 5).unwrap();
    assert_eq!(cursors.last(&Stream::Feed).unwrap(), 9);
    cursors.skip_to(&Stream::Feed, 12).unwrap();
    assert_eq!(cursors.last(&Stream::Feed).unwrap(), 12);
    assert_eq!(cursors.read_cursor(&Stream::Feed).unwrap(), 4);
    assert!(cursors.skip_to(&Stream::Feed, u64::MAX).is_err());
    assert_eq!(cursors.last(&Stream::Feed).unwrap(), 12);
}

#[test]
fn page_without_pruned_through_parses() {
    let page: crate::hub::Page =
        serde_json::from_str(r#"{"messages":[],"next_after":null,"head":0}"#).unwrap();
    assert_eq!(page.pruned_through, 0);
}

#[tokio::test]
async fn a_message_after_a_gap_waits_for_replay() {
    let mut h = start().await;
    h.hub_has(1);
    h.live(1).await;
    assert_eq!(h.delivered().await, [1]);
    // 2 and 3 never arrive live; 4 does.
    h.hub_has(4);
    h.live(4).await;
    assert_eq!(
        h.delivered().await,
        [2, 3, 4],
        "the gap is filled first, in order"
    );
    // A late or repeated copy is dropped.
    h.live(3).await;
    h.live(4).await;
    assert_eq!(h.delivered().await, Vec::<u64>::new());
    assert_eq!(h.cursors.last(&Stream::Feed).unwrap(), 4);
}

#[tokio::test]
async fn live_messages_wait_while_a_stream_replays() {
    let mut h = start().await;
    h.hub_has(3);
    h.streams.on_connected();
    // While the catch-up is queued, live messages are left to it.
    h.live(1).await;
    h.live(2).await;
    h.hub_has(4);
    h.live(4).await;
    assert_eq!(h.delivered().await, [1, 2, 3, 4], "each once, in order");
}

#[tokio::test]
async fn the_replayer_owns_a_stream_until_its_pass_ends() {
    let mut h = start().await;
    h.hub_has(3);
    let held = h.fake.gate.clone().lock_owned().await;
    h.streams.on_connected();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while *h.fake.pages.lock().unwrap() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "no replay started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Next by the cursor, but the replayer's page is in flight: leave it.
    h.live(1).await;
    assert!(h.events.try_recv().is_err(), "delivered while replaying");
    drop(held);
    assert_eq!(h.delivered().await, [1, 2, 3]);
}

#[tokio::test]
async fn catches_up_on_a_newer_head() {
    let mut h = start().await;
    h.hub_has(3);
    h.streams.on_head(br#"{"feed_seq":3}"#).await.unwrap();
    assert_eq!(h.delivered().await, [1, 2, 3]);
    // A head the cursor already reached asks the hub nothing.
    let pages = *h.fake.pages.lock().unwrap();
    h.streams
        .on_head(br#"{"feed_seq":3,"rules_v":0}"#)
        .await
        .unwrap();
    assert_eq!(h.delivered().await, Vec::<u64>::new());
    assert_eq!(*h.fake.pages.lock().unwrap(), pages);
    h.hub_has(5);
    h.streams.on_head(br#"{"feed_seq":5}"#).await.unwrap();
    assert_eq!(h.delivered().await, [4, 5]);
}

#[tokio::test]
async fn applies_delivery_replies() {
    let mut h = start().await;
    let env = agmen_proto::ClientEnvelope::from_json(
        br#"{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"FYI","data":{},
            "body":"hi","sent_at":"2026-10-25T16:04:11Z"}"#,
    )
    .unwrap();
    h.outbox.enqueue(&env).unwrap();
    let reply = json!({"verb": "ACK", "id": env.id, "stream": "feed", "seq": 9});
    h.streams
        .on_reply(&serde_json::to_vec(&reply).unwrap())
        .await
        .unwrap();
    let recorded = OutboxState::Recorded {
        stream: Stream::Feed,
        seq: Seq(9),
    };
    assert_eq!(
        h.events.try_recv().unwrap(),
        ClientEvent::Delivery {
            id: env.id.clone(),
            state: recorded.clone()
        }
    );
    assert_eq!(h.outbox.state(&env.id).unwrap(), Some(recorded));
    // Garbage and replies for other messages are skipped.
    h.streams.on_reply(b"not json").await.unwrap();
    let other =
        json!({"verb": "ACK", "id": "01JBX6Q4T7W3C2M8Y9K5N0P1ZZ", "stream": "feed", "seq": 10});
    h.streams
        .on_reply(&serde_json::to_vec(&other).unwrap())
        .await
        .unwrap();
    assert!(h.events.try_recv().is_err());
}

#[test]
fn replay_delays_double_to_the_cap() {
    assert_eq!(replay_delay(1), Duration::from_secs(1));
    assert_eq!(replay_delay(4), Duration::from_secs(8));
    assert_eq!(replay_delay(7), MAX_REPLAY_DELAY);
    assert_eq!(replay_delay(u32::MAX), MAX_REPLAY_DELAY);
}
