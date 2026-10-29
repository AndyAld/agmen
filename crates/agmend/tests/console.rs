//! The console methods against a fake hub: snapshot, history, watch, post.

#![cfg(test)]

mod support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use agmen_local::ErrorCode;
use agmen_local::console::{ConsoleEvent, ConsoleSnapshot};
use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{
    ACCESS, Daemon, HANDLE, HUMAN, PROJECT, envelope, error, next, quiet, result, roster,
};

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
    if event
        == (ConsoleEvent::Hub {
            connected: false,
            error: None,
        })
    {
        event = self::event(next(frames).await);
    }
    assert_eq!(
        event,
        ConsoleEvent::Hub {
            connected: true,
            error: None
        }
    );
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
        ConsoleEvent::Hub {
            connected: false,
            error: None
        }
    );
    // Posted while agmend is between connections: the resume replays it.
    daemon.hub.state.publish("four");
    assert_eq!(
        event(next(&mut frames).await),
        ConsoleEvent::Hub {
            connected: true,
            error: None
        }
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
        ConsoleEvent::Hub {
            connected: false,
            error: None
        }
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

#[tokio::test]
async fn an_unreachable_hub_during_renewal_is_a_hub_error() {
    let daemon = Daemon::start_unreachable().await;
    let mut client = daemon.client().await;
    let err = error(
        client
            .call("console_snapshot", json!({"project": PROJECT}))
            .await,
    );
    assert_eq!(err.code, ErrorCode::Hub, "{}", err.message);
    assert!(
        err.message.contains("could not be reached"),
        "{}",
        err.message
    );
}

#[tokio::test]
async fn a_watch_while_renewal_cannot_reach_the_hub_starts_disconnected() {
    let daemon = Daemon::start_unreachable().await;
    let mut frames = daemon.client().await.watch(PROJECT).await;
    assert_eq!(
        event(next(&mut frames).await),
        ConsoleEvent::Hub {
            connected: false,
            error: None
        }
    );
    // Still retrying, and nothing says the person signed out.
    quiet(&mut frames, Duration::from_millis(500)).await;
}

/// A body at the 32 KiB cap made of quotes, which JSON doubles: 500 of
/// these are about 33 MB, over the 16 MiB frame cap.
fn heavy_body() -> String {
    "\"".repeat(32 * 1024)
}

/// Asserts `seqs` are contiguous and end at `last`, and fit one frame.
fn assert_newest_that_fit(seqs: &[u64], last: u64) {
    assert!(!seqs.is_empty());
    assert!(seqs.len() < 500, "{} messages cannot fit", seqs.len());
    assert_eq!(*seqs.last().unwrap(), last);
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
}

#[tokio::test]
async fn a_snapshot_over_the_frame_cap_keeps_the_newest_that_fit() {
    let daemon = Daemon::start().await;
    daemon.hub.state.fill_with(600, &heavy_body());
    let mut client = daemon.client().await;
    let snap = result(
        client
            .call("console_snapshot", json!({"project": PROJECT}))
            .await,
    );
    let got = seqs(&snap["messages"]);
    assert_newest_that_fit(&got, 600);
    assert_eq!(snap["older"], true);
    // Paging back from the oldest one kept reaches the dropped ones.
    let page = result(
        client
            .call(
                "chat_history",
                json!({"project": PROJECT, "before": got[0]}),
            )
            .await,
    );
    assert_newest_that_fit(&seqs(&page["messages"]), got[0] - 1);
    assert_eq!(page["older"], true);
}

/// Reads frames until a `Hub` event carries an error, and returns it.
async fn until_hub_error(
    frames: &mut tokio::sync::mpsc::Receiver<agmen_local::Response>,
) -> agmen_local::ApiError {
    for _ in 0..20 {
        if let ConsoleEvent::Hub {
            connected,
            error: Some(error),
        } = event(next(frames).await)
        {
            assert!(!connected);
            return error;
        }
    }
    panic!("no Hub event with an error");
}

#[tokio::test]
async fn signing_out_mid_watch_tells_the_console() {
    let daemon = Daemon::start().await;
    let mut frames = daemon.client().await.watch(PROJECT).await;
    until_connected(&mut frames).await;
    // A logout: the hub drops the access token, the store loses it, and the
    // stream ends.
    daemon.hub.state.revoked.store(true, Ordering::SeqCst);
    std::fs::remove_dir_all(daemon.home.dir.path().join("tokens")).unwrap();
    daemon.hub.state.drop_streams();
    let error = until_hub_error(&mut frames).await;
    assert_eq!(error.code, ErrorCode::Unauthorized, "{}", error.message);
}

#[tokio::test]
async fn too_many_streams_tells_the_console_and_retries() {
    let daemon = Daemon::start().await;
    *daemon.hub.state.events_refusal.lock().unwrap() =
        Some((StatusCode::TOO_MANY_REQUESTS, "too_many_streams"));
    let mut frames = daemon.client().await.watch(PROJECT).await;
    let error = until_hub_error(&mut frames).await;
    assert_eq!(error.code, ErrorCode::Hub);
    assert!(
        error.message.contains("too_many_streams"),
        "{}",
        error.message
    );
    // The same refusal again sends nothing new; once the hub has room the
    // retry connects.
    *daemon.hub.state.events_refusal.lock().unwrap() = None;
    until_connected(&mut frames).await;
}

#[tokio::test]
async fn snapshot_fills_the_listener_from_roster_and_settings() {
    // No listener process is running, so status is empty. The roster row
    // plus the project's settings table are enough for the entry.
    let daemon = Daemon::start().await;
    let settings = daemon.home.socket().with_file_name("settings.toml");
    std::fs::write(
        &settings,
        format!("[listener.{PROJECT}]\nper_delegation_usd = \"8.00\"\n"),
    )
    .unwrap();
    *daemon.hub.state.roster.lock().unwrap() = json!([
        {"identity_id": HUMAN, "handle": HANDLE, "kind": "human", "status": "online"},
        {"identity_id": "idlisten", "handle": "andy/listen", "kind": "listener", "status": "away"},
        {"identity_id": "idn7k2q9", "handle": "andy/sol", "kind": "agent", "status": "online",
         "agent": "claude", "model": "opus"},
    ]);
    let mut client = daemon.client().await;
    let snap = result(
        client
            .call("console_snapshot", json!({"project": PROJECT}))
            .await,
    );
    let snap: ConsoleSnapshot = serde_json::from_value(snap).unwrap();
    let listener = snap.listener.expect("roster and settings name a listener");
    assert_eq!(listener.identity_id.as_str(), "idlisten");
    assert_eq!(listener.handle, "andy/listen");
    assert_eq!(listener.state, "away");
    assert_eq!(listener.model, "claude-haiku-5-5");
    assert_eq!(listener.today_usd, "0.00");
    assert_eq!(listener.daily_usd, "2.00");
    assert_eq!(listener.per_delegation_usd.as_deref(), Some("8.00"));
    assert_eq!(listener.workers_daily_usd.as_deref(), Some("20.00"));
    assert_eq!(listener.max_workers, Some(3));
    assert_eq!(
        listener.takes_work_from.as_deref(),
        Some("andy, coordinators")
    );
    assert_eq!(listener.role.as_deref(), Some("listener"));
    assert!(listener.can_use.is_empty());
    assert!(listener.never.is_empty());
    assert!(listener.workers_running.is_none());
    assert!(snap.changes.is_empty());
}

#[tokio::test]
async fn snapshot_invents_no_listener_caps_without_a_table_or_a_running_listener() {
    // The roster lists the listener, nothing runs, and there is no settings
    // file: the defaults are not something this person set, so no entry.
    let daemon = Daemon::start().await;
    *daemon.hub.state.roster.lock().unwrap() = json!([
        {"identity_id": HUMAN, "handle": HANDLE, "kind": "human", "status": "online"},
        {"identity_id": "idlisten", "handle": "andy/listen", "kind": "listener", "status": "away"},
    ]);
    let mut client = daemon.client().await;
    let snap = result(
        client
            .call("console_snapshot", json!({"project": PROJECT}))
            .await,
    );
    let snap: ConsoleSnapshot = serde_json::from_value(snap).unwrap();
    assert!(snap.listener.is_none());
    assert!(snap.changes.is_empty());
}
