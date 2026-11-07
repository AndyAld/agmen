//! Hub delivery when one owner manages nearly the full identity quota.

#![cfg(test)]

mod support;

use std::time::{Duration, Instant};

use agmen_local::Request;
use serde_json::json;
use support::{Daemon, HUMAN, PROJECT, result};

const MANAGED_IDENTITIES: usize = agmen_local::safeguards::MAX_MANAGED - 1;
const SEEDED_IDENTITIES: usize = 2;
const POSITION_POLLS: usize = 100;
const POSITION_POLL: Duration = Duration::from_millis(50);
/// A collector that saves nothing new for this long has stopped, not slowed.
const POSITION_STALL: Duration = Duration::from_secs(15);
/// Hard bound on one wait, however steadily the collector progresses.
const POSITION_TOTAL: Duration = Duration::from_secs(120);
const CALL_WAIT: Duration = Duration::from_secs(10);

async fn wait_position(daemon: &Daemon, stream: &str, seq: i64) {
    let path = daemon.home.database();
    let stream = stream.to_owned();
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(path).unwrap();
        let start = Instant::now();
        let mut last_progress = (start, (0, 0));
        loop {
            let saved: i64 = conn
                .query_row(
                    "SELECT coalesce(max(seq),0) FROM positions WHERE project=?1 AND stream=?2",
                    rusqlite::params![PROJECT, stream],
                    |row| row.get(0),
                )
                .unwrap();
            if saved >= seq {
                return;
            }
            let (count, total): (i64, i64) = conn
                .query_row(
                    "SELECT count(*), coalesce(sum(seq),0) FROM positions WHERE project=?1",
                    [PROJECT],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            if last_progress.1 != (count, total) {
                last_progress = (Instant::now(), (count, total));
            }
            assert!(
                last_progress.0.elapsed() < POSITION_STALL && start.elapsed() < POSITION_TOTAL,
                "collector stopped at {count} saved positions; {stream}:{seq} was never committed"
            );
            std::thread::sleep(POSITION_POLL);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn owner_with_255_hub_identities_keeps_every_stream() {
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    let mut daemon = tokio::task::spawn_blocking(move || {
        for n in SEEDED_IDENTITIES + 1..=MANAGED_IDENTITIES {
            daemon
                .home
                .seed_identity(&daemon.hub.url, &format!("agent{n:03}"), &"c".repeat(64));
        }
        daemon
    })
    .await
    .unwrap();

    daemon.hub.state.inject(support::stamped(1, "feed"));
    let mut person = support::stamped(2, "person inbox");
    person["stream"] = json!(format!("inbox:{HUMAN}"));
    person["seq"] = json!(1);
    person["verb"] = json!("DM");
    person["to"] = json!([HUMAN]);
    daemon.hub.state.inject(person);
    daemon.resume();
    wait_position(&daemon, &format!("inbox:{HUMAN}"), 1).await;
    for n in 1..=MANAGED_IDENTITIES {
        let id = format!("agent{n:03}");
        let mut message = support::stamped(n as u64 + 2, "managed inbox");
        message["stream"] = json!(format!("inbox:{id}"));
        message["seq"] = json!(1);
        message["verb"] = json!("DM");
        message["to"] = json!([id]);
        daemon.hub.state.inject(message);
    }
    wait_position(&daemon, &format!("inbox:agent{MANAGED_IDENTITIES:03}"), 1).await;

    let mut client = daemon.client().await;
    for n in MANAGED_IDENTITIES - 1..=MANAGED_IDENTITIES {
        let mut request = Request::new(99, "pending", json!({"local_token": "c".repeat(64)}));
        request.as_ = Some(format!("agent{n:03}").parse().unwrap());
        let rows = tokio::time::timeout(CALL_WAIT, async {
            client.send(&request).await;
            result(client.read().await)
        })
        .await
        .unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
        assert_eq!(rows[0]["stream"], format!("inbox:agent{n:03}"));
    }
    daemon.hub.state.publish("collector is still running");
    wait_position(&daemon, "feed", 2).await;
    daemon.stop().await;
}

struct ReplayPass {
    after: std::collections::BTreeMap<String, u64>,
    sent: Vec<(String, u64)>,
}

struct ReplayHub {
    forward: std::sync::Arc<Proxy>,
    hub: std::sync::Arc<support::HubState>,
    passes: std::sync::Mutex<Vec<ReplayPass>>,
    disconnect: tokio::sync::Notify,
    /// While false, a new connection waits before it replays anything.
    gate: tokio::sync::watch::Sender<bool>,
}

async fn counted_replay(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<ReplayHub>>,
    uri: axum::http::Uri,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    if !uri.path().ends_with("/events") {
        return proxy(axum::extract::State(state.forward.clone()), uri).await;
    }
    let query = url::Url::parse(&format!("http://proxy{uri}")).unwrap();
    let after: std::collections::BTreeMap<String, u64> = query
        .query_pairs()
        .find(|(name, _)| name == "after")
        .map(|(_, value)| {
            value
                .split(',')
                .filter(|entry| !entry.is_empty())
                .map(|entry| {
                    let (stream, seq) = entry.rsplit_once(':').unwrap();
                    (stream.to_owned(), seq.parse().unwrap())
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(after.len() <= MAX_AFTER);
    // Feed comes last, so its saved position proves every discarded row before
    // it has been consumed, rather than merely queued by this fake hub.
    let replay: Vec<_> = state
        .hub
        .inboxes
        .lock()
        .unwrap()
        .values()
        .flatten()
        .chain(state.hub.feed.lock().unwrap().iter())
        .filter(|message| {
            message["seq"].as_u64().unwrap()
                > after
                    .get(message["stream"].as_str().unwrap())
                    .copied()
                    .unwrap_or(0)
        })
        .cloned()
        .collect();
    let pass = {
        let mut passes = state.passes.lock().unwrap();
        assert!(passes.len() < MAX_PROXY_REQUESTS);
        passes.push(ReplayPass {
            after,
            sent: Vec::new(),
        });
        passes.len() - 1
    };
    let (tx, rx) = tokio::sync::mpsc::channel(PROXY_QUEUE);
    let sending = state.clone();
    let task = tokio::spawn(async move {
        let disconnected = sending.disconnect.notified();
        tokio::pin!(disconnected);
        disconnected.as_mut().enable();
        sending
            .gate
            .subscribe()
            .wait_for(|open| *open)
            .await
            .unwrap();
        for message in replay {
            let stream = message["stream"].as_str().unwrap().to_owned();
            let seq = message["seq"].as_u64().unwrap();
            let event = axum::response::sse::Event::default()
                .id(format!("{stream}:{seq}"))
                .data(message.to_string());
            if tx
                .send(Ok::<_, std::convert::Infallible>(event))
                .await
                .is_err()
            {
                return;
            }
            sending.passes.lock().unwrap()[pass]
                .sent
                .push((stream, seq));
        }
        disconnected.await;
        drop(tx);
    });
    state.forward.tasks.lock().unwrap().push(task);
    axum::response::sse::Sse::new(tokio_stream::wrappers::ReceiverStream::new(rx)).into_response()
}

async fn replay_fixture(
    full: bool,
) -> (
    Daemon,
    std::sync::Arc<ReplayHub>,
    tokio::task::JoinHandle<()>,
) {
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    let state = std::sync::Arc::new(ReplayHub {
        forward: std::sync::Arc::new(Proxy {
            upstream: daemon.hub.url.clone(),
            afters: Default::default(),
            refusals: Default::default(),
            tasks: Default::default(),
        }),
        hub: daemon.hub.state.clone(),
        passes: Default::default(),
        disconnect: Default::default(),
        gate: tokio::sync::watch::Sender::new(true),
    });
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}/", socket.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(counted_replay)
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
    let origin = agmen_client::origin_key(&proxy_url.parse().unwrap());
    let seed_url = proxy_url.clone();
    let daemon = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(daemon.home.database()).unwrap();
        conn.execute("DELETE FROM identities", []).unwrap();
        for n in 1..=SEEDED_IDENTITIES {
            daemon
                .home
                .seed_identity(&seed_url, &format!("agent{n:03}"), &"c".repeat(64));
        }
        let namespace = format!("{origin}#owner={HUMAN}");
        conn.execute(
            "INSERT INTO positions VALUES (?1,?2,'feed',0)",
            rusqlite::params![namespace, PROJECT],
        )
        .unwrap();
        if full {
            conn.execute(
                "INSERT INTO positions VALUES (?1,?2,'inbox:agent001',0)",
                rusqlite::params![namespace, PROJECT],
            )
            .unwrap();
            for n in 1..=OTHER_OWNER_STREAMS {
                conn.execute(
                    "INSERT INTO positions VALUES (?1,?2,?3,0)",
                    rusqlite::params![
                        format!("{origin}#owner=other001"),
                        PROJECT,
                        format!("inbox:other{n:03}")
                    ],
                )
                .unwrap();
            }
        }
        daemon
    })
    .await
    .unwrap();
    let login = agmen_client::Login {
        hub_url: proxy_url.parse().unwrap(),
        human_id: HUMAN.parse().unwrap(),
        handle: support::HANDLE.into(),
    };
    tokio::fs::write(
        daemon.home.login_file(),
        serde_json::to_vec(&login).unwrap(),
    )
    .await
    .unwrap();
    agmen_client::store_tokens(
        &daemon.home.tokens(),
        &login,
        &agmen_client::HumanTokens::new(support::ACCESS.into(), "refresh-1".into(), 3600),
    )
    .await
    .unwrap();
    shorten_hub_backoff(&proxy_url);
    (daemon, state, server)
}

fn discarded_message(stream: &str, seq: u64) -> serde_json::Value {
    let mut message = inbox_message(2, seq);
    message["stream"] = json!(stream);
    message["to"] = json!([stream.strip_prefix("inbox:").unwrap()]);
    message
}

fn sent_for(pass: &ReplayPass, stream: &str) -> Vec<u64> {
    pass.sent
        .iter()
        .filter(|(name, _)| name == stream)
        .map(|(_, seq)| *seq)
        .collect()
}

async fn discarded_history(full: bool, repick: bool) {
    let (mut daemon, state, server) = replay_fixture(full).await;
    let stream = if full {
        "inbox:agent002"
    } else {
        "inbox:revoked000"
    };
    for seq in 1..=3 {
        daemon.hub.state.inject(discarded_message(stream, seq));
    }
    daemon
        .hub
        .state
        .inject(support::stamped(1, "first pass consumed"));
    daemon.resume();
    wait_position(&daemon, "feed", 1).await;
    assert_eq!(
        sent_for(&state.passes.lock().unwrap()[0], stream),
        [1, 2, 3]
    );
    daemon.hub.state.inject(discarded_message(stream, 4));
    daemon
        .hub
        .state
        .inject(support::stamped(2, "second pass consumed"));
    state.disconnect.notify_waiters();
    wait_position(&daemon, "feed", 2).await;
    {
        let passes = state.passes.lock().unwrap();
        assert_eq!(passes.len(), 2);
        assert_eq!(
            sent_for(&passes[1], stream),
            [4],
            "only new events may be sent after reconnect"
        );
        assert_eq!(passes[1].after.get(stream), Some(&3));
    }
    assert_eq!(count(&daemon, "SELECT count(*) FROM positions WHERE project=?1 AND stream IN ('inbox:agent002','inbox:revoked000')").await, 0, "discarded heads must never be stored");
    if repick {
        let path = daemon.home.database();
        tokio::task::spawn_blocking(move || {
            rusqlite::Connection::open(path)
                .unwrap()
                .execute("DELETE FROM identities WHERE identity='agent001'", [])
                .unwrap();
        })
        .await
        .unwrap();
        wait_position(&daemon, stream, 4).await;
        let mut client = daemon.client().await;
        let mut request = Request::new(99, "pending", json!({"local_token":"c".repeat(64)}));
        request.as_ = Some("agent002".parse().unwrap());
        client.send(&request).await;
        let rows = result(
            tokio::time::timeout(CALL_WAIT, client.read())
                .await
                .unwrap(),
        );
        let seqs: Vec<_> = rows
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["stream"] == stream)
            .map(|row| row["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, [1, 2, 3, 4]);
        client.send(&request).await;
        assert!(
            result(
                tokio::time::timeout(CALL_WAIT, client.read())
                    .await
                    .unwrap()
            )
            .as_array()
            .unwrap()
            .is_empty()
        );
        let passes = state.passes.lock().unwrap();
        assert_eq!(passes.len(), 3);
        assert_eq!(
            passes[2].after.get(stream).copied().unwrap_or(0),
            0,
            "re-pick must ignore the discarded head"
        );
    }
    stop_replay_fixture(daemon, state, server).await;
}

async fn stop_replay_fixture(
    daemon: Daemon,
    state: std::sync::Arc<ReplayHub>,
    server: tokio::task::JoinHandle<()>,
) {
    daemon.stop().await;
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    let tasks = std::mem::take(&mut *state.forward.tasks.lock().unwrap());
    for task in tasks {
        task.abort();
        assert!(task.await.map_or_else(|e| e.is_cancelled(), |()| true));
    }
}

#[tokio::test]
async fn hub_skipped_history_is_not_sent_again_on_reconnect() {
    discarded_history(true, false).await;
}

#[tokio::test]
async fn hub_undeliverable_history_is_not_sent_again_on_reconnect() {
    discarded_history(false, false).await;
}

#[tokio::test]
async fn hub_skipped_seen_head_is_discarded_when_a_position_frees() {
    discarded_history(true, true).await;
}

/// SQLite refuses a second writer for longer than its 5 s busy timeout.
const WRITE_LOCK_HELD: Duration = Duration::from_secs(7);
/// A failed ingest waits the 5 s busy timeout before it reports.
const STORE_FAILURE_WAIT: Duration = Duration::from_secs(30);
/// No watch event for this long means the collector has delivered everything.
const QUIET: Duration = Duration::from_millis(500);

#[tokio::test]
async fn hub_undeliverable_heads_are_rechecked_without_a_write_transaction() {
    let (mut daemon, state, server) = replay_fixture(false).await;
    for seq in 1..=2 {
        daemon
            .hub
            .state
            .inject(discarded_message("inbox:revoked000", seq));
    }
    daemon
        .hub
        .state
        .inject(support::stamped(1, "first pass consumed"));
    daemon.resume();
    wait_position(&daemon, "feed", 1).await;
    // A held write lock fails any write transaction after the busy timeout. The
    // recheck of an undeliverable head must neither take one nor end on one.
    let path = daemon.home.database();
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        std::thread::sleep(WRITE_LOCK_HELD);
        conn.execute_batch("COMMIT").unwrap();
    })
    .await
    .unwrap();
    daemon
        .hub
        .state
        .inject(support::stamped(2, "collector survived"));
    state.disconnect.notify_waiters();
    wait_position(&daemon, "feed", 2).await;
    assert_eq!(state.passes.lock().unwrap()[1].after["inbox:revoked000"], 2);
    stop_replay_fixture(daemon, state, server).await;
}

#[tokio::test]
async fn hub_store_errors_during_ingest_do_not_end_the_collector() {
    let (mut daemon, state, server) = replay_fixture(false).await;
    daemon
        .hub
        .state
        .inject(support::stamped(1, "first pass consumed"));
    daemon.resume();
    wait_position(&daemon, "feed", 1).await;
    // Hold the new connection until the write lock is taken, so the lock is
    // held while the message is ingested, not while the pass starts.
    state.gate.send_replace(false);
    daemon
        .hub
        .state
        .inject(support::stamped(2, "held by a store error"));
    state.disconnect.notify_waiters();
    while state.passes.lock().unwrap().len() < 2 {
        tokio::time::sleep(POSITION_POLL).await;
    }
    let path = daemon.home.database();
    let (locked, held) = std::sync::mpsc::channel();
    let holder = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        locked.send(()).unwrap();
        std::thread::sleep(WRITE_LOCK_HELD);
        conn.execute_batch("COMMIT").unwrap();
    });
    held.recv().unwrap();
    state.gate.send_replace(true);
    holder.await.unwrap();
    // The collector must still be running: it delivers the held message and a later one.
    wait_position(&daemon, "feed", 2).await;
    daemon.hub.state.inject(support::stamped(3, "later"));
    state.disconnect.notify_waiters();
    wait_position(&daemon, "feed", 3).await;
    stop_replay_fixture(daemon, state, server).await;
}

/// Reads watch events until the collector reports `connected`.
async fn next_connection(watch: &mut support::Client, connected: bool) {
    tokio::time::timeout(STORE_FAILURE_WAIT, async {
        loop {
            let event = result(watch.read().await);
            if event["kind"] == "connection" && event["connected"] == connected {
                return;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no connection event with connected={connected}"));
}

fn pending_copies(path: std::path::PathBuf, identity: &'static str, id: String) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM pending WHERE identity=?1 AND id=?2",
            rusqlite::params![identity, id],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn hub_transient_store_failure_goes_offline_and_resumes_once() {
    let (mut daemon, state, server) = replay_fixture(false).await;
    let first = inbox_message(1, 1);
    let held = [inbox_message(1, 2), inbox_message(2, 1)];
    daemon.hub.state.inject(first.clone());
    daemon
        .hub
        .state
        .inject(support::stamped(1, "first pass consumed"));
    daemon.resume();
    wait_position(&daemon, "feed", 1).await;
    wait_position(&daemon, "inbox:agent001", 1).await;
    // The second pass waits at the gate, so the lock is certainly held when
    // its first message is ingested.
    state.gate.send_replace(false);
    for message in &held {
        daemon.hub.state.inject(message.clone());
    }
    daemon
        .hub
        .state
        .inject(support::stamped(2, "held by a store error"));
    state.disconnect.notify_waiters();
    while state.passes.lock().unwrap().len() < 2 {
        tokio::time::sleep(POSITION_POLL).await;
    }
    let mut watch = daemon.client().await;
    watch
        .send(&Request::new(9, "watch", json!({"project":PROJECT})))
        .await;
    assert_eq!(result(watch.read().await)["kind"], "self");
    let path = daemon.home.database();
    let (locked, taken) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        locked.send(()).unwrap();
        // Held until the test has seen the failure, however long that takes.
        released.recv().unwrap();
        conn.execute_batch("COMMIT").unwrap();
    });
    taken.recv().unwrap();
    state.gate.send_replace(true);
    // Nothing disconnects the second pass, so an offline status can only come
    // from the failed ingest.
    next_connection(&mut watch, false).await;
    release.send(()).unwrap();
    holder.await.unwrap();
    wait_position(&daemon, "feed", 2).await;
    next_connection(&mut watch, true).await;
    {
        let passes = state.passes.lock().unwrap();
        assert!(passes.len() >= 3, "the failure must end the second pass");
        // The failed pass saved nothing, so the reconnect resumes from it.
        assert_eq!(passes[2].after["feed"], 1);
        assert_eq!(passes[2].after["inbox:agent001"], 1);
    }
    let path = daemon.home.database();
    let copies = tokio::task::spawn_blocking(move || {
        [
            (&first, "agent001"),
            (&held[0], "agent001"),
            (&held[1], "agent002"),
        ]
        .map(|(message, identity)| {
            let id = message["id"].as_str().unwrap().to_owned();
            pending_copies(path.clone(), identity, id)
        })
    })
    .await
    .unwrap();
    assert_eq!(copies, [1, 1, 1]);
    // The feed message reaches the watcher once.
    let mut delivered = 0;
    while let Ok(event) = tokio::time::timeout(QUIET, watch.read()).await {
        let event = result(event);
        if event["kind"] == "message"
            && event["message"]["seq"] == 2
            && event["message"]["stream"] == "feed"
        {
            delivered += 1;
        }
    }
    assert_eq!(delivered, 1);
    drop(watch);
    stop_replay_fixture(daemon, state, server).await;
}

#[tokio::test]
async fn hub_repick_without_room_keeps_heads_of_streams_still_skipped() {
    let (mut daemon, state, server) = replay_fixture(true).await;
    let path = daemon.home.database();
    let copy = |name: &'static str| {
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            rusqlite::Connection::open(path)
                .unwrap()
                .execute(
                    "INSERT INTO identities SELECT origin,?1,owner,project,metadata FROM identities WHERE identity='agent001'",
                    [name],
                )
                .unwrap();
        })
    };
    copy("agent003").await.unwrap();
    for stream in ["inbox:agent002", "inbox:agent003", "inbox:agent004"] {
        for seq in 1..=3 {
            daemon.hub.state.inject(discarded_message(stream, seq));
        }
    }
    daemon
        .hub
        .state
        .inject(support::stamped(1, "first pass consumed"));
    daemon.resume();
    wait_position(&daemon, "feed", 1).await;
    daemon
        .hub
        .state
        .inject(support::stamped(2, "second pass consumed"));
    // agent004 becomes deliverable while the project is still full.
    copy("agent004").await.unwrap();
    wait_position(&daemon, "feed", 2).await;
    {
        let passes = state.passes.lock().unwrap();
        for stream in ["inbox:agent002", "inbox:agent003"] {
            assert!(
                sent_for(&passes[1], stream).is_empty(),
                "{stream} is still skipped and must not replay"
            );
            assert_eq!(passes[1].after[stream], 3);
        }
    }
    stop_replay_fixture(daemon, state, server).await;
}

#[tokio::test]
async fn hub_discarded_head_overflow_keeps_deliverable_cursors_first() {
    const REMOTE_STREAMS: usize = 600;
    // Only the feed has a saved position; deliverable streams without one are unnamed.
    const NAMED_CURSORS: usize = 1;
    let (mut daemon, state, server) = replay_fixture(false).await;
    for n in 0..REMOTE_STREAMS {
        daemon
            .hub
            .state
            .inject(discarded_message(&format!("inbox:remote{n:03}"), 1));
    }
    daemon
        .hub
        .state
        .inject(support::stamped(1, "first pass consumed"));
    daemon.resume();
    wait_position(&daemon, "feed", 1).await;
    assert_eq!(
        state.passes.lock().unwrap()[0].sent.len(),
        REMOTE_STREAMS + 1
    );
    // An unnamed deliverable stream still gets every message from the hub.
    daemon.hub.state.inject(inbox_message(1, 1));
    daemon
        .hub
        .state
        .inject(support::stamped(2, "second pass consumed"));
    state.disconnect.notify_waiters();
    wait_position(&daemon, "feed", 2).await;
    {
        let passes = state.passes.lock().unwrap();
        assert_eq!(
            passes.len(),
            2,
            "discarded heads must not add SSE connections"
        );
        assert_eq!(passes[1].after.len(), MAX_AFTER);
        assert!(!passes[1].after.contains_key("inbox:agent001"));
        assert_eq!(passes[1].after["feed"], 1);
        assert_eq!(
            passes[1].sent.len(),
            REMOTE_STREAMS - (MAX_AFTER - NAMED_CURSORS) + 2
        );
    }
    assert_eq!(
        count(
            &daemon,
            "SELECT count(*) FROM positions WHERE project=?1 AND stream LIKE 'inbox:remote%'"
        )
        .await,
        0
    );
    wait_position(&daemon, "inbox:agent001", 1).await;
    stop_replay_fixture(daemon, state, server).await;
}

const MAX_AFTER: usize = 256;
const REFUSED_ATTEMPTS: usize = 2;
const PROXY_QUEUE: usize = 16;
const MAX_PROXY_REQUESTS: usize = 64;
const CONNECTION_POLLS: usize = 300;
/// Text a refusing hub chooses; it must never reach agmend's log.
const HUB_TEXT: &str = "hub_text_must_not_reach_the_log";

#[derive(Clone, Default)]
struct Logs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

struct Proxy {
    upstream: String,
    afters: std::sync::Mutex<Vec<std::collections::BTreeMap<String, u64>>>,
    refusals: std::sync::atomic::AtomicUsize,
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        for task in self
            .tasks
            .get_mut()
            .unwrap()
            .drain(..)
            .take(MAX_PROXY_REQUESTS)
        {
            task.abort();
        }
    }
}

async fn proxy(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<Proxy>>,
    uri: axum::http::Uri,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    if uri.path().ends_with("/events") {
        let query = url::Url::parse(&format!("http://proxy{uri}")).unwrap();
        let after: std::collections::BTreeMap<String, u64> = query
            .query_pairs()
            .find(|(name, _)| name == "after")
            .map(|(_, value)| {
                value
                    .split(',')
                    .map(|entry| {
                        let (stream, seq) = entry.rsplit_once(':').unwrap();
                        (stream.to_owned(), seq.parse().unwrap())
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            after.len() <= MAX_AFTER,
            "after names {} streams",
            after.len()
        );
        state.afters.lock().unwrap().push(after.clone());
        // Only the second request names the person's inbox, which sorts last.
        if after.contains_key(&format!("inbox:{HUMAN}"))
            && state
                .refusals
                .try_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |remaining| remaining.checked_sub(1),
                )
                .is_ok()
        {
            return (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                axum::Json(json!({"error": HUB_TEXT})),
            )
                .into_response();
        }
    }
    let upstream = format!("{}{}", state.upstream.trim_end_matches('/'), uri);
    let mut response = reqwest::Client::builder()
        .read_timeout(CALL_WAIT)
        .build()
        .unwrap()
        .get(upstream)
        .bearer_auth(support::ACCESS)
        .send()
        .await
        .unwrap();
    let mut builder = axum::http::Response::builder().status(response.status());
    if let Some(content_type) = response.headers().get("content-type") {
        builder = builder.header("content-type", content_type);
    }
    let (tx, rx) = tokio::sync::mpsc::channel(PROXY_QUEUE);
    let task = tokio::spawn(async move {
        while let Ok(Some(bytes)) = response.chunk().await {
            if !matches!(
                tokio::time::timeout(CALL_WAIT, tx.send(Ok::<_, std::io::Error>(bytes))).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
    });
    let mut tasks = state.tasks.lock().unwrap();
    assert!(tasks.len() < MAX_PROXY_REQUESTS);
    tasks.push(task);
    builder
        .body(axum::body::Body::from_stream(
            tokio_stream::wrappers::ReceiverStream::new(rx),
        ))
        .unwrap()
}

async fn wait_afters(state: &Proxy, expected: usize) {
    for _ in 0..CONNECTION_POLLS {
        if state.afters.lock().unwrap().len() >= expected {
            return;
        }
        tokio::time::sleep(POSITION_POLL).await;
    }
    panic!("collector did not open {expected} connections");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "exercise two reconnects, refusal and every chunk in one daemon lifetime"
)]
async fn hub_chunks_resume_after_refusal_without_duplicate_delivery() {
    let logs = Logs::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish(),
    );
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    let state = std::sync::Arc::new(Proxy {
        upstream: daemon.hub.url.clone(),
        afters: Default::default(),
        refusals: std::sync::atomic::AtomicUsize::new(REFUSED_ATTEMPTS),
        tasks: Default::default(),
    });
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}/", socket.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(proxy)
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
    let mut daemon = tokio::task::spawn_blocking(move || {
        // Use the existing seeding helper with the proxy as this owner's origin.
        let connection = rusqlite::Connection::open(daemon.home.database()).unwrap();
        connection.execute("DELETE FROM identities", []).unwrap();
        for n in 1..=MANAGED_IDENTITIES {
            daemon
                .home
                .seed_identity(&proxy_url, &format!("agent{n:03}"), &"c".repeat(64));
        }
        let login = agmen_client::Login {
            hub_url: proxy_url.parse().unwrap(),
            human_id: HUMAN.parse().unwrap(),
            handle: support::HANDLE.into(),
        };
        std::fs::write(
            daemon.home.login_file(),
            serde_json::to_vec(&login).unwrap(),
        )
        .unwrap();
        (daemon, login)
    })
    .await
    .unwrap();
    agmen_client::store_tokens(
        &daemon.0.home.tokens(),
        &daemon.1,
        &agmen_client::HumanTokens::new(support::ACCESS.into(), "refresh-1".into(), 3600),
    )
    .await
    .unwrap();
    daemon.0.hub.state.inject(support::stamped(1, "first feed"));
    for n in 0..=MANAGED_IDENTITIES {
        let id = if n == 0 {
            HUMAN.to_owned()
        } else {
            format!("agent{n:03}")
        };
        let mut message = support::stamped(n as u64 + 2, "first inbox");
        message["stream"] = json!(format!("inbox:{id}"));
        message["seq"] = json!(1);
        message["verb"] = json!("DM");
        message["to"] = json!([id]);
        daemon.0.hub.state.inject(message);
    }
    daemon.0.resume();
    wait_position(&daemon.0, "inbox:agent255", 1).await;
    wait_position(&daemon.0, &format!("inbox:{HUMAN}"), 1).await;
    let before = state.afters.lock().unwrap().len();
    daemon.0.hub.state.drop_streams();
    wait_afters(&state, before + 2 * REFUSED_ATTEMPTS + 2).await;
    daemon.0.hub.state.publish("second feed");
    for (n, id) in [(1, "agent001"), (2, "agent255"), (3, HUMAN)] {
        let mut message = support::stamped(MANAGED_IDENTITIES as u64 + n + 3, "second inbox");
        message["stream"] = json!(format!("inbox:{id}"));
        message["seq"] = json!(2);
        message["verb"] = json!("DM");
        message["to"] = json!([id]);
        daemon.0.hub.state.inject(message);
    }
    wait_position(&daemon.0, "feed", 2).await;
    wait_position(&daemon.0, "inbox:agent001", 2).await;
    wait_position(&daemon.0, "inbox:agent255", 2).await;
    wait_position(&daemon.0, &format!("inbox:{HUMAN}"), 2).await;
    let before = state.afters.lock().unwrap().len();
    daemon.0.hub.state.drop_streams();
    wait_afters(&state, before + 2).await;
    {
        let afters = state.afters.lock().unwrap();
        let resumed = &afters[before..];
        assert!(resumed.iter().any(|after| after.get("feed") == Some(&2)));
        assert!(
            resumed
                .iter()
                .any(|after| after.get("inbox:agent255") == Some(&2))
        );
        // The hub replays a stream `after` omits from seq 0. The short chunk
        // also names the feed, so reconnecting does not replay its whole history.
        assert!(resumed.iter().any(|after| {
            after.get(&format!("inbox:{HUMAN}")) == Some(&2) && after.get("feed") == Some(&2)
        }));
        assert!(afters.iter().all(|after| after.len() <= MAX_AFTER));
    }
    let mut client = daemon.0.client().await;
    for id in ["agent001", "agent255"] {
        let mut request = Request::new(99, "pending", json!({"local_token":"c".repeat(64)}));
        request.as_ = Some(id.parse().unwrap());
        client.send(&request).await;
        let rows = result(
            tokio::time::timeout(CALL_WAIT, client.read())
                .await
                .unwrap(),
        );
        let inbox: Vec<_> = rows
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["stream"] == format!("inbox:{id}"))
            .map(|row| row["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(inbox, [1, 2], "{rows}");
        client.send(&request).await;
        assert!(
            result(
                tokio::time::timeout(CALL_WAIT, client.read())
                    .await
                    .unwrap()
            )
            .as_array()
            .unwrap()
            .is_empty()
        );
    }
    let log = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    let warnings: Vec<_> = log
        .lines()
        .filter(|line| line.contains("hub events connection refused"))
        .collect();
    assert_eq!(warnings.len(), 1, "{log}");
    assert!(warnings[0].contains(PROJECT), "{log}");
    assert!(warnings[0].contains("8 streams per person"), "{log}");
    assert!(!log.contains(HUB_TEXT), "{log}");
    daemon.0.stop().await;
    server.abort();
}

const LIVE_IDENTITIES: usize = agmen_local::safeguards::MAX_MANAGED;
const STALE_IDENTITIES: usize = 3;
const OLD_RETRY_BUDGET: usize = 10;
const LONG_REFUSAL_WAIT: Duration = Duration::from_secs(280);

async fn seed_positions(daemon: Daemon, origin: String, stale: bool) -> Daemon {
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(daemon.home.database()).unwrap();
        conn.execute("DELETE FROM identities", []).unwrap();
        for n in 1..=LIVE_IDENTITIES {
            daemon
                .home
                .seed_identity(&origin, &format!("agent{n:03}"), &"c".repeat(64));
        }
        let namespace = format!("{}#owner={HUMAN}", origin.trim_end_matches('/'));
        for stream in std::iter::once("feed".to_owned())
            .chain(std::iter::once(format!("inbox:{HUMAN}")))
            .chain((1..=LIVE_IDENTITIES).map(|n| format!("inbox:agent{n:03}")))
            .chain(
                (0..if stale { STALE_IDENTITIES } else { 0 })
                    .map(|n| format!("inbox:revoked{n:03}")),
            )
        {
            conn.execute(
                "INSERT INTO positions VALUES (?1,?2,?3,0)",
                rusqlite::params![namespace, PROJECT, stream],
            )
            .unwrap();
        }
        daemon
    })
    .await
    .unwrap()
}

fn inbox_message(n: usize, seq: u64) -> serde_json::Value {
    let id = format!("agent{n:03}");
    let mut message = support::stamped(n as u64 * 10 + seq, "deliverable inbox");
    message["stream"] = json!(format!("inbox:{id}"));
    message["seq"] = json!(seq);
    message["verb"] = json!("DM");
    message["to"] = json!([id]);
    message
}

async fn drain_inbox(daemon: &Daemon, n: usize) -> Vec<u64> {
    let mut client = daemon.client().await;
    let mut request = Request::new(99, "pending", json!({"local_token":"c".repeat(64)}));
    request.as_ = Some(format!("agent{n:03}").parse().unwrap());
    client.send(&request).await;
    result(
        tokio::time::timeout(CALL_WAIT, client.read())
            .await
            .unwrap(),
    )
    .as_array()
    .unwrap()
    .iter()
    .map(|row| row["seq"].as_u64().unwrap())
    .collect()
}

#[tokio::test]
async fn hub_drops_revoked_positions_and_delivers_all_live_inboxes() {
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    let origin = daemon.hub.url.clone();
    let mut daemon = seed_positions(daemon, origin, true).await;
    for n in 1..=LIVE_IDENTITIES {
        daemon.hub.state.inject(inbox_message(n, 1));
    }
    let mut stale = inbox_message(1, 1);
    stale["stream"] = json!("inbox:revoked000");
    stale["to"] = json!(["revoked000"]);
    daemon.hub.state.inject(stale);
    daemon.resume();
    wait_position(&daemon, "inbox:agent256", 1).await;
    let path = daemon.home.database();
    let stale_count = tokio::task::spawn_blocking(move || {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM positions WHERE stream LIKE 'inbox:revoked%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        stale_count, 0,
        "stale positions must be deleted, including new stale events"
    );
    {
        let afters = daemon.hub.state.event_afters.lock().unwrap();
        assert!(
            afters.len() >= 2,
            "all deliverable saved positions need two chunks"
        );
        assert!(
            afters
                .iter()
                .flatten()
                .all(|after| !after.contains("revoked"))
        );
    }
    for n in 1..=LIVE_IDENTITIES {
        wait_position(&daemon, &format!("inbox:agent{n:03}"), 1).await;
        assert_eq!(drain_inbox(&daemon, n).await, [1]);
    }
    daemon.stop().await;
}

async fn refusing_second_chunk(
    refusals: usize,
) -> (Daemon, std::sync::Arc<Proxy>, tokio::task::JoinHandle<()>) {
    chunk_connections(refusals, None, false).await
}

/// The same refusals with the collector's reconnect delays shortened, when the
/// build has the hook; the retries are the same, only the waits between them.
async fn quickly_retrying_second_chunk(
    refusals: usize,
) -> (Daemon, std::sync::Arc<Proxy>, tokio::task::JoinHandle<()>) {
    chunk_connections(refusals, None, true).await
}

#[cfg(feature = "test-hooks")]
fn shorten_hub_backoff(hub_url: &str) {
    const RETRY_INITIAL: Duration = Duration::from_millis(10);
    const RETRY_MAX: Duration = Duration::from_millis(80);
    const RETRY_HEALTHY: Duration = Duration::from_secs(60);
    agmend::test_hub_timing(
        &hub_url.parse().unwrap(),
        RETRY_INITIAL,
        RETRY_MAX,
        RETRY_HEALTHY,
    );
}

#[cfg(not(feature = "test-hooks"))]
fn shorten_hub_backoff(_hub_url: &str) {}

type HeldHeaders = std::sync::Arc<(tokio::sync::Notify, tokio::sync::Notify)>;

async fn chunk_connections(
    refusals: usize,
    held: Option<HeldHeaders>,
    quick_retries: bool,
) -> (Daemon, std::sync::Arc<Proxy>, tokio::task::JoinHandle<()>) {
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    let state = std::sync::Arc::new(Proxy {
        upstream: daemon.hub.url.clone(),
        afters: Default::default(),
        refusals: std::sync::atomic::AtomicUsize::new(refusals),
        tasks: Default::default(),
    });
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}/", socket.local_addr().unwrap());
    let app = axum::Router::new()
        .fallback(
            move |state: axum::extract::State<std::sync::Arc<Proxy>>, uri: axum::http::Uri| {
                let held = held.clone();
                async move {
                    let second = uri.path().ends_with("/events")
                        && url::Url::parse(&format!("http://proxy{uri}"))
                            .unwrap()
                            .query_pairs()
                            .any(|(name, value)| {
                                name == "after" && value.contains(&format!("inbox:{HUMAN}:"))
                            });
                    if second && let Some(held) = held {
                        held.0.notify_one();
                        held.1.notified().await;
                    }
                    proxy(state, uri).await
                }
            },
        )
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
    let mut daemon = seed_positions(daemon, proxy_url.clone(), false).await;
    let login = agmen_client::Login {
        hub_url: proxy_url.parse().unwrap(),
        human_id: HUMAN.parse().unwrap(),
        handle: support::HANDLE.into(),
    };
    tokio::fs::write(
        daemon.home.login_file(),
        serde_json::to_vec(&login).unwrap(),
    )
    .await
    .unwrap();
    agmen_client::store_tokens(
        &daemon.home.tokens(),
        &login,
        &agmen_client::HumanTokens::new(support::ACCESS.into(), "refresh-1".into(), 3600),
    )
    .await
    .unwrap();
    daemon.hub.state.inject(inbox_message(1, 1));
    daemon.hub.state.inject(inbox_message(LIVE_IDENTITIES, 1));
    if quick_retries {
        shorten_hub_backoff(&proxy_url);
    }
    daemon.resume();
    (daemon, state, server)
}

#[tokio::test]
async fn hub_first_chunk_delivers_during_second_chunk_refusal() {
    let (daemon, state, server) = refusing_second_chunk(MAX_PROXY_REQUESTS).await;
    wait_afters(&state, 2).await;
    wait_position(&daemon, "inbox:agent001", 1).await;
    assert!(state.refusals.load(std::sync::atomic::Ordering::SeqCst) > 0);
    assert_eq!(drain_inbox(&daemon, 1).await, [1]);
    state.refusals.store(0, std::sync::atomic::Ordering::SeqCst);
    wait_position(&daemon, "inbox:agent256", 1).await;
    assert_eq!(drain_inbox(&daemon, LIVE_IDENTITIES).await, [1]);
    assert!(
        drain_inbox(&daemon, 1).await.is_empty(),
        "replay must not redeliver the first chunk"
    );
    daemon.hub.state.inject(inbox_message(1, 2));
    wait_position(&daemon, "inbox:agent001", 2).await;
    assert_eq!(drain_inbox(&daemon, 1).await, [2]);
    daemon.stop().await;
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn hub_first_chunk_delivers_before_second_response_headers() {
    let held = HeldHeaders::default();
    let (daemon, _, server) = chunk_connections(0, Some(held.clone()), false).await;
    tokio::time::timeout(CALL_WAIT, held.0.notified())
        .await
        .unwrap();
    wait_position(&daemon, "inbox:agent001", 1).await;
    assert_eq!(drain_inbox(&daemon, 1).await, [1]);
    held.1.notify_one();
    wait_position(&daemon, "inbox:agent256", 1).await;
    assert_eq!(drain_inbox(&daemon, LIVE_IDENTITIES).await, [1]);
    assert!(drain_inbox(&daemon, 1).await.is_empty());
    daemon.stop().await;
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn person_only_collector_posts_a_project_stream_alert() {
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    let mut daemon = tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(daemon.home.database()).unwrap();
        conn.execute("DELETE FROM identities", []).unwrap();
        let origin = agmen_client::origin_key(&daemon.hub.url.parse().unwrap());
        conn.execute(
            "INSERT INTO positions VALUES (?1,?2,'feed',0)",
            rusqlite::params![format!("{origin}#owner={HUMAN}"), PROJECT],
        )
        .unwrap();
        for n in 1..LIVE_IDENTITIES + 2 {
            conn.execute(
                "INSERT INTO positions VALUES (?1,?2,?3,0)",
                rusqlite::params![
                    format!("{origin}#owner=other001"),
                    PROJECT,
                    format!("inbox:other{n:03}")
                ],
            )
            .unwrap();
        }
        daemon
    })
    .await
    .unwrap();
    let mut blocked = support::stamped(1, "owner inbox at the project position cap");
    blocked["stream"] = json!(format!("inbox:{HUMAN}"));
    blocked["verb"] = json!("DM");
    blocked["to"] = json!([HUMAN]);
    daemon.hub.state.inject(blocked);
    daemon.resume();
    let mut client = daemon.client().await;
    client
        .send(&Request::new(9, "watch", json!({"project":PROJECT})))
        .await;
    tokio::time::timeout(CALL_WAIT, async {
        for _ in 0..POSITION_POLLS {
            if !daemon.hub.state.posts.lock().unwrap().is_empty() {
                return;
            }
            tokio::time::sleep(POSITION_POLL).await;
        }
        panic!("person-only collector lost its project ALERT");
    })
    .await
    .unwrap();
    {
        let posts = daemon.hub.state.posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].0, format!("Bearer {}", support::ACCESS));
        assert_eq!(posts[0].1["verb"], "ALERT");
        assert_eq!(posts[0].1["data"]["needs"], "owner");
        assert!(posts[0].1["body"].as_str().unwrap().contains("258 streams"));
    }
    wait_position(&daemon, "feed", 1).await;
    drop(client);
    daemon.stop().await;
}

#[tokio::test]
async fn hub_refusal_outlives_previous_retry_budget() {
    let (daemon, state, server) = quickly_retrying_second_chunk(OLD_RETRY_BUDGET + 1).await;
    tokio::time::timeout(LONG_REFUSAL_WAIT, async {
        for _ in 0..LONG_REFUSAL_WAIT.as_secs() {
            if state
                .afters
                .lock()
                .unwrap()
                .iter()
                .filter(|after| after.contains_key(&format!("inbox:{HUMAN}")))
                .count()
                > OLD_RETRY_BUDGET + 1
            {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        panic!("collector stopped retrying the refused second chunk");
    })
    .await
    .expect("collector must retry beyond the old reconnect budget");
    wait_position(&daemon, "inbox:agent256", 1).await;
    assert_eq!(drain_inbox(&daemon, LIVE_IDENTITIES).await, [1]);
    daemon.stop().await;
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[cfg(feature = "test-hooks")]
const BACKOFF_STEPS: u32 = 5;
#[cfg(feature = "test-hooks")]
const STREAM_WAIT: Duration = Duration::from_secs(10);

#[cfg(feature = "test-hooks")]
async fn wait_event_streams(daemon: &Daemon, expected: usize) {
    tokio::time::timeout(STREAM_WAIT, async {
        while daemon
            .hub
            .state
            .event_streams
            .load(std::sync::atomic::Ordering::SeqCst)
            < expected
        {
            tokio::time::sleep(POSITION_POLL / 10).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the collector never opened events connection {expected}"));
}

#[cfg(feature = "test-hooks")]
#[tokio::test]
async fn hub_backoff_starts_over_after_a_healthy_connection() {
    const INITIAL: Duration = Duration::from_millis(50);
    const MAX: Duration = Duration::from_millis(1600);
    const HEALTHY: Duration = Duration::from_millis(400);
    // Longer than HEALTHY, shorter than MAX, and well past INITIAL.
    const UP_FOR: Duration = Duration::from_millis(600);
    const RECONNECT_BY: Duration = Duration::from_millis(800);
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    agmend::test_hub_timing(&daemon.hub.url.parse().unwrap(), INITIAL, MAX, HEALTHY);
    daemon.resume();
    // Each drop comes before HEALTHY, so the delay doubles up to MAX.
    for n in 1..=BACKOFF_STEPS as usize {
        wait_event_streams(&daemon, n).await;
        daemon.hub.state.drop_streams();
    }
    let n = BACKOFF_STEPS as usize + 1;
    wait_event_streams(&daemon, n).await;
    tokio::time::sleep(UP_FOR).await;
    daemon.hub.state.drop_streams();
    let dropped = std::time::Instant::now();
    wait_event_streams(&daemon, n + 1).await;
    assert!(
        dropped.elapsed() < RECONNECT_BY,
        "reconnected after {:?}, not at the initial {INITIAL:?}",
        dropped.elapsed()
    );
    daemon.stop().await;
}

const OTHER_OWNER_STREAMS: usize = agmen_local::safeguards::MAX_MANAGED;

async fn count(daemon: &Daemon, sql: &'static str) -> i64 {
    let path = daemon.home.database();
    tokio::task::spawn_blocking(move || {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(sql, [PROJECT], |row| row.get(0))
            .unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn hub_stream_skipped_at_the_position_cap_resumes_when_an_identity_is_removed() {
    const ALERTS: &str = "SELECT coalesce(max(alerts),0) FROM stream_alerts WHERE project=?1";
    const REARMED: &str = "SELECT count(*) FROM stream_alerts WHERE project=?1 AND full=0";
    const POSITION: &str =
        "SELECT count(*) FROM positions WHERE project=?1 AND stream='inbox:agent002'";
    const PENDING: &str = "SELECT count(*) FROM pending WHERE project=?1 AND identity='agent002'";
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    // The project holds 258 positions: this owner's feed and agent001, and
    // another owner's 256, which this collector never deletes. agent002 is live
    // but has none, and cannot get one.
    let origin = agmen_client::origin_key(&daemon.hub.url.parse().unwrap());
    let path = daemon.home.database();
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(path).unwrap();
        for stream in ["feed", "inbox:agent001"] {
            conn.execute(
                "INSERT INTO positions VALUES (?1,?2,?3,0)",
                rusqlite::params![format!("{origin}#owner={HUMAN}"), PROJECT, stream],
            )
            .unwrap();
        }
        for n in 1..=OTHER_OWNER_STREAMS {
            conn.execute(
                "INSERT INTO positions VALUES (?1,?2,?3,0)",
                rusqlite::params![
                    format!("{origin}#owner=other001"),
                    PROJECT,
                    format!("inbox:other{n:03}")
                ],
            )
            .unwrap();
        }
    })
    .await
    .unwrap();
    daemon.hub.state.inject(inbox_message(2, 1));
    daemon.resume();
    for _ in 0..POSITION_POLLS {
        if count(&daemon, ALERTS).await > 0 {
            break;
        }
        tokio::time::sleep(POSITION_POLL).await;
    }
    assert_eq!(count(&daemon, ALERTS).await, 1, "one ALERT for the fill");
    assert_eq!(count(&daemon, POSITION).await, 0, "no position at the cap");
    assert_eq!(count(&daemon, PENDING).await, 0, "skipped, not delivered");

    // Removing agent001 frees its position, so agent002's inbox fits again.
    let path = daemon.home.database();
    tokio::task::spawn_blocking(move || {
        rusqlite::Connection::open(path)
            .unwrap()
            .execute("DELETE FROM identities WHERE identity='agent001'", [])
            .unwrap();
    })
    .await
    .unwrap();
    wait_position(&daemon, "inbox:agent002", 1).await;
    assert_eq!(
        count(&daemon, PENDING).await,
        1,
        "the skipped message arrives"
    );
    assert_eq!(count(&daemon, REARMED).await, 1, "the ALERT re-arms");
    daemon.stop().await;
}

#[tokio::test]
async fn hub_permanent_store_failure_keeps_a_watch_open_and_offline_until_repaired() {
    const FAILING_FOR: Duration = Duration::from_secs(1);
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    shorten_hub_backoff(&daemon.hub.url);
    let conn = rusqlite::Connection::open(daemon.home.database()).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_pending BEFORE INSERT ON pending BEGIN SELECT RAISE(FAIL,'injected schema failure'); END;").unwrap();
    daemon.resume();
    let mut watch = daemon.client().await;
    watch
        .send(&Request::new(9, "watch", json!({"project":PROJECT})))
        .await;
    assert_eq!(result(watch.read().await)["kind"], "self");
    daemon.hub.state.inject(inbox_message(1, 1));
    // Only a failed ingest can report offline here.
    next_connection(&mut watch, false).await;
    // Many retries pass; the watch stays open and never hears the collector
    // come back, because nothing has been ingested.
    let _quiet = tokio::time::timeout(FAILING_FOR, async {
        loop {
            let event = result(watch.read().await);
            assert!(
                !(event["kind"] == "connection" && event["connected"] == true),
                "connected flapped during the failure: {event}"
            );
        }
    })
    .await;
    conn.execute_batch("DROP TRIGGER fail_pending").unwrap();
    wait_position(&daemon, "inbox:agent001", 1).await;
    next_connection(&mut watch, true).await;
    drop(watch);
    daemon.stop().await;
}

#[tokio::test]
async fn hub_failure_after_the_commit_goes_back_online_on_a_quiet_hub() {
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    shorten_hub_backoff(&daemon.hub.url);
    let mut daemon = tokio::task::spawn_blocking(move || {
        for n in SEEDED_IDENTITIES + 1..=agmen_local::safeguards::MAX_MANAGED {
            daemon
                .home
                .seed_identity(&daemon.hub.url, &format!("agent{n:03}"), &"c".repeat(64));
        }
        daemon
    })
    .await
    .unwrap();
    daemon.resume();
    // Each managed sender takes one own-message tracker, so all are used.
    for n in 1..=agmen_local::safeguards::MAX_MANAGED {
        let mut message = support::stamped(n as u64, "own message");
        message["from"] = json!(format!("agent{n:03}"));
        daemon.hub.state.inject(message);
    }
    wait_position(&daemon, "feed", agmen_local::safeguards::MAX_MANAGED as i64).await;
    // A new managed sender finds no free tracker, which fails after its commit.
    let path = daemon.home.database();
    tokio::task::spawn_blocking(move || {
        rusqlite::Connection::open(path)
            .unwrap()
            .execute(
                "UPDATE identities SET identity='agent900' WHERE identity='agent256'",
                [],
            )
            .unwrap();
    })
    .await
    .unwrap();
    let mut watch = daemon.client().await;
    watch
        .send(&Request::new(9, "watch", json!({"project":PROJECT})))
        .await;
    assert_eq!(result(watch.read().await)["kind"], "self");
    let last = agmen_local::safeguards::MAX_MANAGED as u64 + 1;
    let mut message = support::stamped(last, "from the new identity");
    message["from"] = json!("agent900");
    daemon.hub.state.inject(message);
    next_connection(&mut watch, false).await;
    wait_position(&daemon, "feed", last as i64).await;
    // The hub stays quiet: only the next good state read can end the outage.
    next_connection(&mut watch, true).await;
    drop(watch);
    daemon.stop().await;
}
