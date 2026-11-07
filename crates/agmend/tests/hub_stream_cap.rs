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
