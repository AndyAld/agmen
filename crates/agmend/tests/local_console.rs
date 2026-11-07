//! Console reads, posts and watches on this machine's coordination record.

#![cfg(test)]

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agmen_local::console::{ConsoleEvent, ConsoleSnapshot, Status};
use agmen_local::{ErrorCode, Request, Response, frame};
use agmen_proto::IdentityKind;
use agmend::Agmend;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse as _;
use axum::routing::{get, post};
use serde_json::{Value, json};
use support::{Client, Daemon, Home, envelope, error, result};
use tokio::net::UnixStream;

const ANSWER_WAIT: Duration = Duration::from_secs(5);
const QUIET_WAIT: Duration = Duration::from_millis(250);
const POLL_EVERY: Duration = Duration::from_millis(20);
const PRESENCE_POLLS: usize = 100;
#[cfg(feature = "test-hooks")]
const LONG_FEED: u32 = 1_200;
#[cfg(feature = "test-hooks")]
const POSTS_PER_SENDER: u32 = 30;

struct LocalDaemon {
    home: Home,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
    #[cfg(feature = "test-hooks")]
    local: agmend::LocalOriginTest,
}

impl LocalDaemon {
    async fn start(home: Home) -> Self {
        let daemon = Agmend::bind(home.config()).unwrap();
        #[cfg(feature = "test-hooks")]
        let local = daemon.test_local_origin();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(daemon.serve_until(async {
            let _stopped = stopped.await;
        }));
        Self {
            home,
            stop,
            task,
            #[cfg(feature = "test-hooks")]
            local,
        }
    }

    async fn unsigned() -> Self {
        Self::start(Home {
            dir: tempfile::Builder::new().prefix("lc").tempdir().unwrap(),
        })
        .await
    }

    async fn client(&self) -> Client {
        let mut client = Client::connect(&self.home.socket()).await;
        result(client.call("hello", json!({"api": 1})).await);
        client
    }

    async fn stop(self) -> Home {
        self.stop.send(()).unwrap();
        tokio::time::timeout(ANSWER_WAIT, self.task)
            .await
            .unwrap()
            .unwrap();
        self.home
    }
}

async fn create(client: &mut Client) -> Value {
    result(
        client
            .call("local_project_create", json!({"name": "solo"}))
            .await,
    )
}

async fn snapshot(client: &mut Client, project: &Value) -> ConsoleSnapshot {
    let answer = tokio::time::timeout(
        ANSWER_WAIT,
        client.call("console_snapshot", json!({"project": project})),
    )
    .await
    .unwrap();
    serde_json::from_value(result(answer)).unwrap()
}

async fn post_message(client: &mut Client, project: &Value, id: u32) -> Value {
    result(
        client
            .call(
                "post",
                json!({"project": project, "envelope": envelope(id, "local message")}),
            )
            .await,
    )
}

// Own the socket so dropping a watch really hangs it up, including when idle.
async fn watch(daemon: &LocalDaemon, project: &Value) -> UnixStream {
    let mut stream = UnixStream::connect(daemon.home.socket()).await.unwrap();
    frame::write_async(&mut stream, &Request::new(1, "hello", json!({"api": 1})))
        .await
        .unwrap();
    result(frame::read_async(&mut stream).await.unwrap());
    frame::write_async(
        &mut stream,
        &Request::new(2, "console_watch", json!({"project": project})),
    )
    .await
    .unwrap();
    stream
}

async fn event(stream: &mut UnixStream) -> ConsoleEvent {
    let response: Response = tokio::time::timeout(ANSWER_WAIT, frame::read_async(stream))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.id, 2);
    serde_json::from_value(result(response)).unwrap()
}

fn connected() -> ConsoleEvent {
    ConsoleEvent::Hub {
        connected: true,
        error: None,
    }
}

#[tokio::test]
async fn whoami_without_login_is_the_local_person() {
    let daemon = LocalDaemon::unsigned().await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    assert_eq!(
        result(client.call("whoami", json!({})).await),
        created["person"]
    );
    daemon.stop().await;
}

#[tokio::test]
async fn snapshot_of_a_local_project_has_feed_roster_and_me() {
    let daemon = LocalDaemon::unsigned().await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    let project = &created["project"];
    let empty = snapshot(&mut client, project).await;
    assert!(empty.messages.is_empty());
    assert!(!empty.older);
    post_message(&mut client, project, 1).await;
    let snap = snapshot(&mut client, project).await;
    assert_eq!(snap.project, project.as_str().unwrap());
    assert_eq!(serde_json::to_value(&snap.me).unwrap(), created["person"]);
    assert_eq!(snap.messages.len(), 1);
    assert_eq!(snap.messages[0].client.body.as_str(), "local message");
    assert!(!snap.older);
    assert!(snap.roster.iter().any(|row| row.identity_id == snap.me.id));
    daemon.stop().await;
}

#[tokio::test]
async fn person_post_is_stamped_as_the_person() {
    let daemon = LocalDaemon::unsigned().await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    let project = &created["project"];
    for field in ["from", "from_kind", "from_handle"] {
        let mut forged = envelope(1, "forged");
        forged[field] = json!("impostor");
        assert_eq!(
            error(
                client
                    .call("post", json!({"project": project, "envelope": forged}))
                    .await
            )
            .code,
            ErrorCode::BadRequest
        );
    }
    assert_eq!(
        post_message(&mut client, project, 1).await,
        json!({"stream": "feed", "seq": 1})
    );
    // Repeating the id is an ACK of the original row, not another message.
    assert_eq!(post_message(&mut client, project, 1).await["seq"], 1);
    let snap = snapshot(&mut client, project).await;
    assert_eq!(snap.messages.len(), 1);
    assert_eq!(
        snap.messages[0].from.as_str(),
        created["person"]["id"].as_str().unwrap()
    );
    assert_eq!(snap.messages[0].from_kind, IdentityKind::Human);
    assert_eq!(
        snap.messages[0].from_handle,
        created["person"]["handle"].as_str().unwrap()
    );
    daemon.stop().await;
}

#[tokio::test]
async fn console_watch_streams_local_posts() {
    let daemon = LocalDaemon::unsigned().await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    let project = &created["project"];
    post_message(&mut client, project, 1).await;
    let mut stream = watch(&daemon, project).await;
    assert_eq!(event(&mut stream).await, connected());
    assert_eq!(post_message(&mut client, project, 2).await["seq"], 2);
    assert!(
        matches!(event(&mut stream).await, ConsoleEvent::Message(m) if m.seq.0 == 2 && m.client.body.as_str() == "local message")
    );
    assert!(
        tokio::time::timeout(QUIET_WAIT, frame::read_async::<Response>(&mut stream))
            .await
            .is_err()
    );
    drop(stream);
    daemon.stop().await;
}

#[tokio::test]
async fn person_is_online_while_a_console_watches() {
    let daemon = LocalDaemon::unsigned().await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    let project = &created["project"];
    let mut stream = watch(&daemon, project).await;
    assert_eq!(event(&mut stream).await, connected());
    let snap = snapshot(&mut client, project).await;
    assert_eq!(
        snap.roster
            .iter()
            .find(|row| row.identity_id == snap.me.id)
            .unwrap()
            .status,
        Status::Online
    );
    drop(stream);
    tokio::time::timeout(ANSWER_WAIT, async {
        for _ in 0..PRESENCE_POLLS {
            let snap = snapshot(&mut client, project).await;
            if snap
                .roster
                .iter()
                .find(|row| row.identity_id == snap.me.id)
                .unwrap()
                .status
                == Status::Offline
            {
                return;
            }
            tokio::time::sleep(POLL_EVERY).await;
        }
        panic!("the local person remained online after the watch closed");
    })
    .await
    .unwrap();
    daemon.stop().await;
}

#[tokio::test]
#[cfg(feature = "test-hooks")]
async fn snapshot_of_a_long_local_feed_is_bounded() {
    let daemon = LocalDaemon::unsigned().await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    let project = &created["project"];
    // Each sender stays within the core's 30-message burst. Seed through
    // the existing test hook while identity API routing belongs to Task 9.
    let project_id = project.as_str().unwrap().parse().unwrap();
    let mut sender = None;
    for id in 1..=LONG_FEED {
        if (id - 1).is_multiple_of(POSTS_PER_SENDER) {
            sender = Some(
                daemon
                    .local
                    .mint_agent(&project_id, &format!("writer{id}"))
                    .await
                    .unwrap(),
            );
        }
        let message = serde_json::from_value(envelope(id, "local message")).unwrap();
        assert!(
            matches!(daemon.local.ingest(&project_id, sender.as_ref().unwrap(), &message).await.unwrap(), agmen_proto::DeliveryReply::Ack { seq, .. } if seq.0 == u64::from(id))
        );
    }
    let snap = snapshot(&mut client, project).await;
    assert_eq!(
        snap.messages.iter().map(|m| m.seq.0).collect::<Vec<_>>(),
        (701..=1200).collect::<Vec<_>>()
    );
    assert!(snap.older);
    let page = result(
        tokio::time::timeout(
            ANSWER_WAIT,
            client.call(
                "chat_history",
                json!({"project": project, "before": snap.messages[0].seq, "limit": 500}),
            ),
        )
        .await
        .unwrap(),
    );
    assert_eq!(
        page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        (201..=700).collect::<Vec<_>>()
    );
    assert_eq!(page["older"], true);
    let newest = result(
        client
            .call("chat_history", json!({"project": project, "limit": 1}))
            .await,
    );
    assert_eq!(newest["messages"][0]["seq"], LONG_FEED);
    for limit in [0, 501] {
        assert_eq!(
            error(
                client
                    .call("chat_history", json!({"project": project, "limit": limit}))
                    .await
            )
            .code,
            ErrorCode::BadRequest
        );
    }
    let first = result(
        client
            .call("chat_history", json!({"project": project, "before": 1}))
            .await,
    );
    assert_eq!(first, json!({"messages": [], "older": false}));
    daemon.stop().await;
}

#[tokio::test]
async fn unknown_console_project_keeps_the_login_hint() {
    let daemon = LocalDaemon::unsigned().await;
    let mut client = daemon.client().await;
    for (method, params) in [
        ("console_snapshot", json!({"project": "unknown1"})),
        ("chat_history", json!({"project": "unknown1"})),
        ("chat_history", json!({"project": "unknown1", "before": 1})),
        (
            "post",
            json!({"project": "unknown1", "envelope": envelope(1, "body")}),
        ),
    ] {
        let err = error(client.call(method, params).await);
        assert!(err.message.contains("this machine"), "{method}: {err:?}");
        assert!(
            err.message.contains("not signed in to a hub"),
            "{method}: {err:?}"
        );
        assert!(err.message.contains("agmen login"), "{method}: {err:?}");
    }
    let mut stream = watch(&daemon, &json!("unknown1")).await;
    let response = tokio::time::timeout(ANSWER_WAIT, frame::read_async(&mut stream))
        .await
        .unwrap()
        .unwrap();
    assert!(error(response).message.contains("agmen login"));
    daemon.stop().await;
}

#[tokio::test]
async fn hub_login_stays_whoami_while_local_snapshots_use_the_local_person() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    assert_eq!(
        result(client.call("whoami", json!({})).await)["id"],
        support::HUMAN
    );
    assert_eq!(
        snapshot(&mut client, &created["project"])
            .await
            .me
            .id
            .as_str(),
        created["person"]["id"].as_str().unwrap()
    );
    daemon.stop().await;
}

struct LookupHub {
    lookups: Mutex<Vec<(String, Value)>>,
    posts: Mutex<Vec<Value>>,
    status: StatusCode,
    delay: Duration,
    roster: Option<Value>,
}

impl Default for LookupHub {
    fn default() -> Self {
        Self {
            lookups: Mutex::new(Vec::new()),
            posts: Mutex::new(Vec::new()),
            status: StatusCode::NOT_FOUND,
            delay: Duration::ZERO,
            roster: None,
        }
    }
}

async fn lookup(
    State(hub): State<Arc<LookupHub>>,
    Path(project): Path<String>,
    Query(query): Query<Value>,
    headers: HeaderMap,
) -> axum::response::Response {
    assert_eq!(
        headers["authorization"],
        format!("Bearer {}", support::ACCESS)
    );
    hub.lookups.lock().unwrap().push((project, query));
    tokio::time::sleep(hub.delay).await;
    match &hub.roster {
        Some(value) => axum::Json(json!({"value": value})).into_response(),
        None => hub.status.into_response(),
    }
}

async fn capture_post(
    State(hub): State<Arc<LookupHub>>,
    axum::Json(body): axum::Json<Value>,
) -> StatusCode {
    hub.posts.lock().unwrap().push(body);
    StatusCode::NOT_FOUND
}

async fn start_lookup_hub(hub: LookupHub) -> (Arc<LookupHub>, Home, tokio::task::JoinHandle<()>) {
    let state = Arc::new(hub);
    let app = axum::Router::new()
        .route("/v1/projects/{p}/state", get(lookup))
        .route("/v1/projects/{p}/messages", post(capture_post))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let home = Home::signed_in(&format!("http://{}/", listener.local_addr().unwrap())).await;
    let hub = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (state, home, hub)
}

#[tokio::test]
async fn absent_marked_record_never_posts_an_unconfirmed_person_only_body() {
    let (state, home, hub) = start_lookup_hub(LookupHub::default()).await;
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    drop(client);
    let home = daemon.stop().await;
    // The marker local_project_create left must survive removing only the database.
    std::fs::remove_file(home.dir.path().join("local.db")).unwrap();
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    let answer = client
        .call(
            "post",
            json!({"project": created["project"], "envelope": envelope(1, "private local body")}),
        )
        .await;
    assert_eq!(state.posts.lock().unwrap().len(), 0);
    let err = error(answer);
    assert_eq!(err.code, ErrorCode::Version, "{err:?}");
    assert!(err.message.contains("local.db"), "{err:?}");
    assert_eq!(
        *state.lookups.lock().unwrap(),
        vec![(
            created["project"].as_str().unwrap().into(),
            json!({"key": "roster"})
        )]
    );
    daemon.stop().await;
    hub.abort();
    assert!(hub.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn absent_unmarked_record_keeps_hub_posts_without_roster_confirmation() {
    let (state, home, hub) = start_lookup_hub(LookupHub::default()).await;
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    let body: agmen_proto::ClientEnvelope =
        serde_json::from_value(envelope(1, "hub project body")).unwrap();
    let body = serde_json::to_value(body).unwrap();
    let answer = client
        .call(
            "post",
            json!({"project": support::PROJECT, "envelope": body}),
        )
        .await;
    assert_eq!(*state.posts.lock().unwrap(), vec![body]);
    assert!(state.lookups.lock().unwrap().is_empty());
    assert_eq!(error(answer).code, ErrorCode::NotFound);
    assert!(!daemon.home.dir.path().join("local.db").exists());
    assert!(!daemon.home.dir.path().join("local-origin-created").exists());
    daemon.stop().await;
    hub.abort();
    assert!(hub.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn absent_record_with_an_unreadable_marker_requires_roster_confirmation() {
    let (state, home, hub) = start_lookup_hub(LookupHub::default()).await;
    let marker = home.dir.path().join("local-origin-created");
    // A directory makes reading fail even when the test runs with elevated privileges.
    std::fs::create_dir(&marker).unwrap();
    assert!(std::fs::read(&marker).is_err());
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    let answer = client
        .call(
            "post",
            json!({"project": support::PROJECT, "envelope": envelope(1, "private local body")}),
        )
        .await;
    assert_eq!(state.posts.lock().unwrap().len(), 0);
    assert_eq!(error(answer).code, ErrorCode::Version);
    assert_eq!(state.lookups.lock().unwrap().len(), 1);
    daemon.stop().await;
    hub.abort();
    assert!(hub.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn absent_marked_record_with_a_local_identity_never_asks_the_hub() {
    // This hub lists the person for every project id, so a roster read would
    // place the project on it. agmend.db already says the project is local.
    let (state, home, hub) = start_lookup_hub(LookupHub {
        roster: Some(support::roster("online")),
        ..LookupHub::default()
    })
    .await;
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    result(
        client
            .call(
                "local_mint",
                json!({"project": created["project"], "name": "worker"}),
            )
            .await,
    );
    drop(client);
    let home = daemon.stop().await;
    std::fs::remove_file(home.dir.path().join("local.db")).unwrap();
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    for (method, params) in [
        (
            "post",
            json!({"project": created["project"], "envelope": envelope(1, "private local body")}),
        ),
        ("console_snapshot", json!({"project": created["project"]})),
    ] {
        let err = error(client.call(method, params).await);
        assert_eq!(err.code, ErrorCode::Version, "{method}: {err:?}");
        assert!(err.message.contains("local.db"), "{method}: {err:?}");
    }
    assert!(state.posts.lock().unwrap().is_empty());
    assert!(state.lookups.lock().unwrap().is_empty());
    daemon.stop().await;
    hub.abort();
    assert!(hub.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn newer_local_record_never_sends_a_local_body_to_the_hub() {
    unreadable_record_never_posts("PRAGMA user_version=2", None).await;
}

#[tokio::test]
async fn incomplete_local_record_never_sends_a_local_body_to_the_hub() {
    unreadable_record_never_posts("DELETE FROM local_origin", None).await;
}

#[tokio::test]
async fn malformed_hub_roster_does_not_confirm_a_local_project() {
    const INVALID_ROSTERS: [&str; 4] = [
        "null",
        "{}",
        "[]",
        r#"[{"identity_id":"another1","handle":"someone","kind":"human","status":"online"}]"#,
    ];
    for encoded in INVALID_ROSTERS {
        unreadable_record_never_posts(
            "PRAGMA user_version=2",
            Some(serde_json::from_str(encoded).unwrap()),
        )
        .await;
    }
}

async fn unreadable_record_never_posts(damage: &'static str, roster: Option<Value>) {
    let state = Arc::new(LookupHub {
        roster,
        ..LookupHub::default()
    });
    let app = axum::Router::new()
        .route("/v1/projects/{p}/state", get(lookup))
        .route("/v1/projects/{p}/messages", post(capture_post))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let home = Home::signed_in(&format!("http://{}/", listener.local_addr().unwrap())).await;
    let hub = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    drop(client);
    let home = daemon.stop().await;
    let path = home.dir.path().join("local.db");
    let version_path = path.clone();
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(version_path).unwrap();
        conn.execute_batch(damage).unwrap();
    })
    .await
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    for (method, params) in [
        (
            "post",
            json!({"project": created["project"], "envelope": envelope(1, "private local body")}),
        ),
        ("console_snapshot", json!({"project": created["project"]})),
    ] {
        let err = error(client.call(method, params).await);
        assert_eq!(err.code, ErrorCode::Version, "{method}: {err:?}");
        assert!(err.message.contains("local.db"), "{err:?}");
    }
    assert!(state.posts.lock().unwrap().is_empty());
    {
        let lookups = state.lookups.lock().unwrap();
        assert_eq!(lookups.len(), 2);
        assert!(lookups.iter().all(|(project, query)| project
            == created["project"].as_str().unwrap()
            && query["key"] == "roster"));
    }
    assert_eq!(std::fs::read(path).unwrap(), before);
    daemon.stop().await;
    hub.abort();
    assert!(hub.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn unreadable_record_fails_closed_on_hub_errors_and_timeout() {
    const LOOKUP_STALL: Duration = Duration::from_secs(10);
    const FAILURES: [(StatusCode, Duration); 7] = [
        (StatusCode::UNAUTHORIZED, Duration::ZERO),
        (StatusCode::FORBIDDEN, Duration::ZERO),
        (StatusCode::NOT_FOUND, Duration::ZERO),
        (StatusCode::TOO_MANY_REQUESTS, Duration::ZERO),
        (StatusCode::INTERNAL_SERVER_ERROR, Duration::ZERO),
        // A success without a decodable roster answer must also fail closed.
        (StatusCode::OK, Duration::ZERO),
        (StatusCode::NOT_FOUND, LOOKUP_STALL),
    ];
    for (status, delay) in FAILURES {
        let state = Arc::new(LookupHub {
            status,
            delay,
            ..LookupHub::default()
        });
        let app = axum::Router::new()
            .route("/v1/projects/{p}/state", get(lookup))
            .route("/v1/projects/{p}/messages", post(capture_post))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let home = Home::signed_in(&format!("http://{}/", listener.local_addr().unwrap())).await;
        let path = home.dir.path().join("local.db");
        // A failed open, rather than a newer schema, exercises the other
        // unreadable-record path without relying on filesystem permissions.
        std::fs::write(path, "not a SQLite database").unwrap();
        let hub = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let daemon = LocalDaemon::start(home).await;
        let mut client = daemon.client().await;
        let err = error(
            tokio::time::timeout(
                ANSWER_WAIT,
                client.call(
                    "post",
                    json!({"project": "unknown1", "envelope": envelope(1, "private local body")}),
                ),
            )
            .await
            .unwrap(),
        );
        assert_eq!(err.code, ErrorCode::Version, "{status}: {err:?}");
        assert!(err.message.contains("local.db"), "{err:?}");
        assert_eq!(state.lookups.lock().unwrap().len(), 1);
        assert!(state.posts.lock().unwrap().is_empty());
        daemon.stop().await;
        hub.abort();
        assert!(hub.await.unwrap_err().is_cancelled());
    }
}

#[tokio::test]
async fn newer_local_record_without_login_returns_the_version_error() {
    let daemon = LocalDaemon::unsigned().await;
    let home = daemon.stop().await;
    let path = home.dir.path().join("local.db");
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
    })
    .await
    .unwrap();
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    let err = error(
        client
            .call(
                "post",
                json!({"project": "unknown1", "envelope": envelope(1, "private local body")}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::Version);
    assert!(err.message.contains("local.db"), "{err:?}");
    daemon.stop().await;
}

const LOCAL_AGENT: &str = "agent001";

/// A local project whose local.db is unreadable, on a daemon signed in to a hub
/// that lists the person for every project id, after `worker`, if named, was
/// minted through `local_mint` and `prepare` changed agmend.db. Asking that hub
/// for a roster would place the project on it and a post would hand it a local
/// body, so neither call may reach it.
async fn unreadable_record_stays_off_the_hub(
    worker: Option<&'static str>,
    prepare: impl FnOnce(&rusqlite::Connection, &Value) + Send + 'static,
) {
    let state = Arc::new(LookupHub {
        roster: Some(support::roster("online")),
        ..LookupHub::default()
    });
    let app = axum::Router::new()
        .route("/v1/projects/{p}/state", get(lookup))
        .route("/v1/projects/{p}/messages", post(capture_post))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let home = Home::signed_in(&format!("http://{}/", listener.local_addr().unwrap())).await;
    let hub = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    let created = create(&mut client).await;
    if let Some(name) = worker {
        result(
            client
                .call(
                    "local_mint",
                    json!({"project": created["project"], "name": name}),
                )
                .await,
        );
    }
    drop(client);
    let home = daemon.stop().await;
    let database = home.database();
    let row = created.clone();
    tokio::task::spawn_blocking(move || {
        prepare(&rusqlite::Connection::open(database).unwrap(), &row);
    })
    .await
    .unwrap();
    let path = home.dir.path().join("local.db");
    let damaged = path.clone();
    tokio::task::spawn_blocking(move || {
        rusqlite::Connection::open(damaged)
            .unwrap()
            .pragma_update(None, "user_version", 2)
            .unwrap();
    })
    .await
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    let daemon = LocalDaemon::start(home).await;
    let mut client = daemon.client().await;
    let mut answers = Vec::new();
    for (method, params) in [
        (
            "post",
            json!({"project": created["project"], "envelope": envelope(1, "private local body")}),
        ),
        ("console_snapshot", json!({"project": created["project"]})),
    ] {
        let answer = tokio::time::timeout(ANSWER_WAIT, client.call(method, params))
            .await
            .unwrap();
        answers.push((method, answer));
    }
    let lookups = state.lookups.lock().unwrap().clone();
    let posts = state.posts.lock().unwrap().clone();
    assert!(posts.is_empty(), "a local body reached the hub: {posts:?}");
    assert!(
        lookups.is_empty(),
        "the hub was asked for a roster: {lookups:?}"
    );
    for (method, answer) in answers {
        let err = error(answer);
        assert_eq!(err.code, ErrorCode::Version, "{method}: {err:?}");
        assert!(err.message.contains("local.db"), "{method}: {err:?}");
    }
    assert_eq!(std::fs::read(path).unwrap(), before);
    daemon.stop().await;
    hub.abort();
    assert!(hub.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn local_identity_keeps_an_unreadable_record_off_the_hub() {
    unreadable_record_stays_off_the_hub(None, |agmend_db, created| {
        // Seed the row the way Home::seed_identity seeds a hub identity; the
        // next test gets its row from local_mint instead.
        let origin = agmen_client::origin_key(&agmen_client::local_hub_url());
        let owner = created["person"]["id"].as_str().unwrap();
        let metadata = json!({
            "hub_url": agmen_client::LOCAL_HUB_URL, "broker_url": agmen_client::LOCAL_BROKER_URL,
            "tenant": created["tenant"], "project": created["project"],
            "identity_id": LOCAL_AGENT, "owner_id": owner,
            "handle": "andy/worker", "kind": "agent",
        });
        agmend_db
            .execute(
                "INSERT INTO identities VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![
                    origin,
                    LOCAL_AGENT,
                    owner,
                    created["project"].as_str().unwrap(),
                    metadata.to_string()
                ],
            )
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn minted_local_identity_keeps_an_unreadable_record_off_the_hub() {
    // No seeded row: local_mint registers the identity under the origin key that
    // routing looks up, so a mismatch between the two sends the project to the hub.
    unreadable_record_stays_off_the_hub(Some("worker"), |_, _| {}).await;
}

#[tokio::test]
async fn unanswerable_agmend_db_keeps_an_unreadable_record_off_the_hub() {
    // No answer from agmend.db cannot rule out a local identity, so it is no
    // license to ask the hub either.
    unreadable_record_stays_off_the_hub(None, |agmend_db, _| {
        agmend_db.execute_batch("DROP TABLE identities").unwrap();
    })
    .await;
}

#[tokio::test]
async fn hub_identity_rows_do_not_keep_a_hub_project_off_the_hub() {
    // The fake hub's origin holds identities for this very project id, which
    // says nothing about a local project.
    let daemon = Daemon::seeded().await;
    daemon.hub.state.fill(2);
    let path = daemon.home.dir.path().join("local.db");
    tokio::task::spawn_blocking(move || {
        rusqlite::Connection::open(path)
            .unwrap()
            .pragma_update(None, "user_version", 2)
            .unwrap();
    })
    .await
    .unwrap();
    let mut client = daemon.client().await;
    let snap = snapshot(&mut client, &json!(support::PROJECT)).await;
    assert_eq!(snap.messages.len(), 2);
    assert_eq!(snap.me.id.as_str(), support::HUMAN);
    daemon.stop().await;
}
