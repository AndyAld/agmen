//! Hub delivery when one owner manages nearly the full identity quota.

#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_local::Request;
use serde_json::json;
use support::{Daemon, HUMAN, PROJECT, result};

const MANAGED_IDENTITIES: usize = agmen_local::safeguards::MAX_MANAGED - 1;
const SEEDED_IDENTITIES: usize = 2;
const POSITION_POLLS: usize = 100;
const POSITION_POLL: Duration = Duration::from_millis(50);
const CALL_WAIT: Duration = Duration::from_secs(10);

async fn wait_position(daemon: &Daemon, stream: &str, seq: i64) {
    let path = daemon.home.database();
    let stream = stream.to_owned();
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(path).unwrap();
        for _ in 0..POSITION_POLLS {
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
            std::thread::sleep(POSITION_POLL);
        }
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM positions WHERE project=?1",
                [PROJECT],
                |row| row.get(0),
            )
            .unwrap();
        panic!("collector stopped at {count} saved positions; {stream}:{seq} was never committed");
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
