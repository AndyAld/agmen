//! The console methods against a fake hub: snapshot, history, watch, post.

#![cfg(test)]

mod support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use agmen_local::ErrorCode;
use agmen_local::console::{ConsoleEvent, ConsoleSnapshot};
use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{ACCESS, Daemon, PROJECT, envelope, error, next, quiet, result, roster};

fn seqs(messages: &Value) -> Vec<u64> {
    messages
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["seq"].as_u64().unwrap())
        .collect()
}

fn event(frame: Value) -> ConsoleEvent {
    serde_json::from_value(frame).unwrap()
}

/// Reads the watch's first frames: `Hub { connected: false }` while the
/// shared events stream opens, then `Hub { connected: true }`.
async fn until_connected(frames: &mut tokio::sync::mpsc::Receiver<agmen_local::Response>) {
    let mut event = event(next(frames).await);
    if event == (ConsoleEvent::Hub { connected: false }) {
        event = self::event(next(frames).await);
    }
    assert_eq!(event, ConsoleEvent::Hub { connected: true });
}

#[tokio::test]
async fn console_snapshot_has_the_newest_page_and_roster() {
    let daemon = Daemon::start().await;
    daemon.hub.state.fill(1200);
    let mut client = daemon.client().await;
    let snap = result(
        client
            .call("console_snapshot", json!({"project": PROJECT}))
            .await,
    );
    let snap: ConsoleSnapshot = serde_json::from_value(snap).unwrap();
    assert_eq!(snap.project, PROJECT);
    assert_eq!(snap.me.handle, "andy");
    let got: Vec<u64> = snap.messages.iter().map(|m| m.seq.0).collect();
    assert_eq!(got, (701..=1200).collect::<Vec<_>>());
    assert!(snap.older);
    assert_eq!(
        serde_json::to_value(&snap.roster).unwrap(),
        roster("online")
    );
}

#[tokio::test]
async fn a_short_feed_has_nothing_older() {
    let daemon = Daemon::start().await;
    daemon.hub.state.fill(3);
    let mut client = daemon.client().await;
    let snap = result(
        client
            .call("console_snapshot", json!({"project": PROJECT}))
            .await,
    );
    assert_eq!(seqs(&snap["messages"]), vec![1, 2, 3]);
    assert_eq!(snap["older"], false);
}

#[tokio::test]
async fn history_pages_at_500() {
    let daemon = Daemon::start().await;
    daemon.hub.state.fill(10_000);
    let mut client = daemon.client().await;
    let page = result(
        client
            .call(
                "chat_history",
                json!({"project": PROJECT, "before": 9501, "limit": 500}),
            )
            .await,
    );
    assert_eq!(seqs(&page["messages"]), (9001..=9500).collect::<Vec<_>>());
    assert_eq!(page["older"], true);
    let page = result(
        client
            .call(
                "chat_history",
                json!({"project": PROJECT, "before": 201, "limit": 500}),
            )
            .await,
    );
    assert_eq!(seqs(&page["messages"]), (1..=200).collect::<Vec<_>>());
    assert_eq!(page["older"], false);
    let err = error(
        client
            .call(
                "chat_history",
                json!({"project": PROJECT, "before": 9501, "limit": 501}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::BadRequest);
    // Without before, the newest page.
    let page = result(
        client
            .call("chat_history", json!({"project": PROJECT, "limit": 3}))
            .await,
    );
    assert_eq!(seqs(&page["messages"]), vec![9998, 9999, 10_000]);
}

#[tokio::test]
async fn watch_streams_messages_in_seq_order() {
    let daemon = Daemon::start().await;
    daemon.hub.state.fill(5);
    let mut first = daemon.client().await.watch(PROJECT).await;
    let mut second = daemon.client().await.watch(PROJECT).await;
    for frames in [&mut first, &mut second] {
        until_connected(frames).await;
    }
    for body in ["six", "seven", "eight"] {
        daemon.hub.state.publish(body);
    }
    for frames in [&mut first, &mut second] {
        let mut got = Vec::new();
        for _ in 0..3 {
            match event(next(frames).await) {
                ConsoleEvent::Message(m) => got.push((m.seq.0, m.client.body.to_string())),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(
            got,
            vec![
                (6, "six".to_owned()),
                (7, "seven".to_owned()),
                (8, "eight".to_owned())
            ]
        );
    }
    // Both consoles share one events stream, which starts at the head, so
    // the five older messages are not replayed.
    assert_eq!(daemon.hub.state.event_streams.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_dropped_stream_resumes_without_duplicates() {
    let daemon = Daemon::start().await;
    daemon.hub.state.fill(2);
    let mut frames = daemon.client().await.watch(PROJECT).await;
    until_connected(&mut frames).await;
    daemon.hub.state.publish("three");
    assert!(matches!(event(next(&mut frames).await), ConsoleEvent::Message(m) if m.seq.0 == 3));
    daemon.hub.state.drop_streams();
    assert_eq!(
        event(next(&mut frames).await),
        ConsoleEvent::Hub { connected: false }
    );
    // Posted while agmend is between connections: the resume replays it.
    daemon.hub.state.publish("four");
    assert_eq!(
        event(next(&mut frames).await),
        ConsoleEvent::Hub { connected: true }
    );
    daemon.hub.state.publish("five");
    let mut got = Vec::new();
    for _ in 0..2 {
        match event(next(&mut frames).await) {
            ConsoleEvent::Message(m) => got.push(m.seq.0),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(got, vec![4, 5]);
    quiet(&mut frames, Duration::from_millis(300)).await;
    assert_eq!(
        *daemon.hub.state.event_afters.lock().unwrap(),
        vec![Some("feed:2".to_owned()), Some("feed:3".to_owned())]
    );
}

#[tokio::test]
async fn the_last_console_leaving_closes_the_stream() {
    let daemon = Daemon::start().await;
    let mut frames = daemon.client().await.watch(PROJECT).await;
    until_connected(&mut frames).await;
    drop(frames);
    // The client's reader notices on its next frame and hangs up; the
    // feed sees no console at its next roster tick (100 ms here).
    daemon.hub.state.publish("one");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let mut frames = daemon.client().await.watch(PROJECT).await;
    assert_eq!(
        event(next(&mut frames).await),
        ConsoleEvent::Hub { connected: false }
    );
    until_connected(&mut frames).await;
    assert_eq!(daemon.hub.state.event_streams.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_signed_out_snapshot_is_unauthorized() {
    let daemon = Daemon::start().await;
    std::fs::remove_dir_all(daemon.home.dir.path().join("tokens")).unwrap();
    let mut client = daemon.client().await;
    let err = error(
        client
            .call("console_snapshot", json!({"project": PROJECT}))
            .await,
    );
    assert_eq!(err.code, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn watching_a_project_the_hub_refuses_is_an_error() {
    let daemon = Daemon::start().await;
    std::fs::remove_dir_all(daemon.home.dir.path().join("tokens")).unwrap();
    let mut frames = daemon.client().await.watch(PROJECT).await;
    let response = tokio::time::timeout(Duration::from_secs(5), frames.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(error(response).code, ErrorCode::Unauthorized);
    assert_eq!(daemon.hub.state.event_streams.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn watch_sends_roster_only_on_change() {
    let daemon = Daemon::start().await;
    let mut frames = daemon.client().await.watch(PROJECT).await;
    until_connected(&mut frames).await;
    // The roster is read every 100 ms here; unchanged, it sends nothing.
    quiet(&mut frames, Duration::from_millis(400)).await;
    *daemon.hub.state.roster.lock().unwrap() = roster("away");
    match event(next(&mut frames).await) {
        ConsoleEvent::Roster(rows) => {
            assert_eq!(serde_json::to_value(&rows).unwrap(), roster("away"));
        }
        other => panic!("{other:?}"),
    }
    quiet(&mut frames, Duration::from_millis(400)).await;
}

#[tokio::test]
async fn post_goes_out_as_the_human() {
    let daemon = Daemon::start().await;
    daemon.hub.state.fill(4);
    let mut client = daemon.client().await;
    let posted = result(
        client
            .call(
                "post",
                json!({"project": PROJECT, "envelope": envelope(1, "hello room")}),
            )
            .await,
    );
    assert_eq!(posted, json!({"stream": "feed", "seq": 5}));
    let posts = daemon.hub.state.posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].0, format!("Bearer {ACCESS}"));
    assert_eq!(posts[0].1["body"], "hello room");
    assert_eq!(posts[0].1["id"], envelope(1, "")["id"]);
}

#[tokio::test]
async fn post_reports_the_hub_nack() {
    let daemon = Daemon::start().await;
    *daemon.hub.state.post_answer.lock().unwrap() = Some((
        StatusCode::TOO_MANY_REQUESTS,
        json!({"verb": "NACK", "id": envelope(2, "")["id"], "retryable": true,
               "retry_after_ms": 2000, "reason": "rate_limited"}),
    ));
    let mut client = daemon.client().await;
    let err = error(
        client
            .call(
                "post",
                json!({"project": PROJECT, "envelope": envelope(2, "too fast")}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::Hub);
    assert!(err.message.contains("rate_limited"), "{}", err.message);
}

#[tokio::test]
async fn post_with_a_bad_envelope_is_a_bad_request() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    let err = error(
        client
            .call(
                "post",
                json!({"project": PROJECT, "envelope": {"v": 1, "body": "no id"}}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}
