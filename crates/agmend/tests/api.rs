//! The socket, the lock, `hello` and `whoami`.

#![cfg(test)]

mod support;

use std::os::unix::fs::PermissionsExt as _;

use agmen_local::{ErrorCode, Request};
use agmend::{Agmend, AgmendError};
use serde_json::json;
use support::{Client, Daemon, HANDLE, HUMAN, error, result};

#[tokio::test]
async fn hello_reports_version_and_exe() {
    let daemon = Daemon::start().await;
    let mut client = Client::connect(&daemon.home.socket()).await;
    let answer = result(client.call("hello", json!({"api": 1})).await);
    assert_eq!(answer["api"], 1);
    assert_eq!(answer["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(answer["build"], option_env!("AGMEN_BUILD").unwrap_or("dev"));
    assert_eq!(answer["exe"], json!(std::env::current_exe().unwrap()));
}

#[tokio::test]
async fn ready_reports_store_schemas() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    let answer = result(client.call("ready", json!({})).await);
    assert_eq!(answer, json!({"ready": true, "schemas": {"agmend.db": 2}}));
}

#[tokio::test]
async fn newer_schema_refused_without_writes_at_startup() {
    let hub = support::fake_hub().await;
    let home = support::Home::signed_in(&hub.url).await;
    for identity in [false, true] {
        let path = if identity {
            home.database()
                .parent()
                .unwrap()
                .join("identities/agent001/agmen.db")
        } else {
            home.database()
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        let error = Agmend::bind(home.config()).unwrap_err();
        let error = anyhow::Error::new(error);
        assert!(format!("{error:#}").contains("99"), "{error:#}");
        if identity {
            assert!(
                format!("{error:#}").contains("identities/agent001/agmen.db"),
                "{error:#}"
            );
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!std::path::PathBuf::from(format!("{}{suffix}", path.display())).exists());
        }
        assert!(!home.socket().exists());
        if identity {
            assert!(
                !home.database().exists(),
                "identity preflight precedes daemon migration"
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test]
async fn ready_reports_identity_store_schemas_after_startup() {
    let hub = support::fake_hub().await;
    let home = support::Home::signed_in(&hub.url).await;
    let identity = home.dir.path().join("identities/agent001/agmen.db");
    std::fs::create_dir_all(identity.parent().unwrap()).unwrap();
    drop(rusqlite::Connection::open(&identity).unwrap());
    let daemon = Agmend::bind(home.config()).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(daemon.serve_until(async {
        stopped.await.unwrap();
    }));
    let mut client = Client::connect(&home.socket()).await;
    result(client.call("hello", json!({"api":1})).await);
    let ready = result(client.call("ready", json!({})).await);
    assert_eq!(
        ready,
        json!({"ready":true,"schemas":{"agmend.db":2,"identities/agent001/agmen.db":2}})
    );
    assert_eq!(
        agmen_client::store_schema(&identity, std::time::Duration::from_secs(1)).unwrap(),
        Some(2)
    );
    let invalid = error(client.call("ready", json!({"extra":true})).await);
    assert_eq!(invalid.code, ErrorCode::BadRequest);
    stop.send(()).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn newer_wal_schema_refused_without_writes_at_startup() {
    let hub = support::fake_hub().await;
    let source = tempfile::tempdir().unwrap();
    let original = source.path().join("future.db");
    let conn = rusqlite::Connection::open(&original).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA user_version=99;")
        .unwrap();
    for identity in [false, true] {
        for with_shm in [false, true] {
            let home = support::Home::signed_in(&hub.url).await;
            let path = if identity {
                home.dir.path().join("identities/agent001/agmen.db")
            } else {
                home.database()
            };
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let suffixes = if with_shm {
                vec!["", "-wal", "-shm"]
            } else {
                vec!["", "-wal"]
            };
            let before: Vec<_> = suffixes
                .iter()
                .map(|suffix| {
                    let bytes = std::fs::read(format!("{}{suffix}", original.display())).unwrap();
                    let target = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
                    std::fs::write(&target, &bytes).unwrap();
                    (target, bytes)
                })
                .collect();
            let error = anyhow::Error::new(Agmend::bind(home.config()).unwrap_err());
            let message = format!("{error:#}");
            assert!(message.contains("99"), "{message}");
            assert!(!home.socket().exists());
            if identity {
                assert!(!home.database().exists());
            }
            assert!(!std::path::PathBuf::from(format!("{}-journal", path.display())).exists());
            assert_eq!(
                std::fs::read_dir(path.parent().unwrap())
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(path.file_name().unwrap().to_str().unwrap()))
                    .count(),
                before.len()
            );
            for (target, bytes) in before {
                assert_eq!(std::fs::read(target).unwrap(), bytes);
            }
        }
    }
}

#[tokio::test]
async fn startup_refuses_symlink_identity_directories() {
    let hub = support::fake_hub().await;
    let home = support::Home::signed_in(&hub.url).await;
    let external = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.dir.path().join("identities")).unwrap();
    std::os::unix::fs::symlink(external.path(), home.dir.path().join("identities/agent001"))
        .unwrap();
    let error = anyhow::Error::new(Agmend::bind(home.config()).unwrap_err());
    assert!(format!("{error:#}").contains("symlink"));
    assert!(!home.database().exists());
    assert_eq!(std::fs::read_dir(external.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn socket_is_0600() {
    let daemon = Daemon::start().await;
    let mode = std::fs::metadata(daemon.home.socket())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
}

#[tokio::test]
async fn second_agmend_exits() {
    let daemon = Daemon::start().await;
    let err = Agmend::bind(daemon.home.config()).unwrap_err();
    assert!(matches!(err, AgmendError::AlreadyRunning { .. }), "{err:?}");
    assert!(err.to_string().contains("already running"), "{err}");
    // The first one still answers.
    let mut client = daemon.client().await;
    result(client.call("whoami", json!({})).await);
}

#[tokio::test]
async fn database_opens_after_lock_and_future_schema_is_contextual() {
    use std::error::Error as _;

    let hub = support::fake_hub().await;
    let home = support::Home::signed_in(&hub.url).await;
    let daemon = Agmend::bind(home.config()).unwrap();
    let conn = rusqlite::Connection::open(home.database()).unwrap();
    conn.pragma_update(None, "user_version", 3).unwrap();
    let error = Agmend::bind(home.config()).unwrap_err();
    assert!(matches!(error, AgmendError::AlreadyRunning { .. }));
    drop(daemon);
    let error = Agmend::bind(home.config()).unwrap_err();
    assert!(matches!(error, AgmendError::Store { .. }));
    assert!(error.to_string().contains("agmend.db"));
    assert!(error.source().unwrap().to_string().contains("schema 3"));
    assert!(!home.socket().exists());
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    let reopened = Agmend::bind(home.config()).unwrap();
    assert!(reopened.socket().exists());
}

#[tokio::test]
async fn hello_with_api_2_is_version_error() {
    let daemon = Daemon::start().await;
    let mut client = Client::connect(&daemon.home.socket()).await;
    let err = error(client.call("hello", json!({"api": 2})).await);
    assert_eq!(err.code, ErrorCode::Version);
    assert!(err.message.contains('1'), "{}", err.message);
}

#[tokio::test]
async fn hello_version_validation_precedes_identity_authentication() {
    let daemon = Daemon::start().await;
    let mut client = Client::connect(&daemon.home.socket()).await;
    client
        .send(&Request::new(
            1,
            "hello",
            json!({"api":2,"local_token":"invalid"}),
        ))
        .await;
    assert_eq!(error(client.read().await).code, ErrorCode::Version);
}

#[tokio::test]
async fn the_first_request_must_be_hello() {
    let daemon = Daemon::start().await;
    let mut client = Client::connect(&daemon.home.socket()).await;
    let err = error(client.call("whoami", json!({})).await);
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("hello"), "{}", err.message);
}

#[tokio::test]
async fn whoami_reads_the_cli_login() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    let me = result(client.call("whoami", json!({})).await);
    assert_eq!(me, json!({"id": HUMAN, "handle": HANDLE}));
}

#[tokio::test]
async fn whoami_signed_out_is_unauthorized() {
    let daemon = Daemon::start().await;
    std::fs::remove_file(daemon.home.login_file()).unwrap();
    let mut client = daemon.client().await;
    let err = error(client.call("whoami", json!({})).await);
    assert_eq!(err.code, ErrorCode::Unauthorized);
    assert!(err.message.contains("agmen login"), "{}", err.message);
}

#[tokio::test]
async fn unknown_methods_are_not_found() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    let err = error(client.call("launch", json!({})).await);
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn post_requires_identity_token() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    let mut request = Request::new(
        9,
        "post",
        json!({"project":support::PROJECT,"envelope":support::envelope(1,"hi")}),
    );
    request.as_ = Some("agent001".parse().unwrap());
    client.send(&request).await;
    let err = error(client.read().await);
    assert_eq!(err.code, ErrorCode::Unauthorized);
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}

async fn identity_call(
    client: &mut Client,
    id: Option<&str>,
    method: &str,
    params: serde_json::Value,
) -> agmen_local::Response {
    let mut request = Request::new(99, method, params);
    request.as_ = id.map(|id| id.parse().unwrap());
    client.send(&request).await;
    client.read().await
}

#[tokio::test]
async fn sibling_token_is_unauthorized() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    for (id, token) in [
        ("agent001", "b".repeat(64)),
        ("agent003", "a".repeat(64)),
        ("agent001", "x".repeat(64)),
    ] {
        assert_eq!(
            error(identity_call(&mut client, Some(id), "post", json!({"local_token":token})).await)
                .code,
            ErrorCode::Unauthorized
        );
    }
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn identity_token_is_not_a_human_credential() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    for token in [json!("a".repeat(64)), json!(null), json!(7)] {
        assert_eq!(
            error(client.call("post", json!({"local_token":token})).await).code,
            ErrorCode::Unauthorized
        );
    }
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn identity_origin_and_owner_must_match_login() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    let original = std::fs::read(daemon.home.login_file()).unwrap();
    for field in ["hub_url", "human_id"] {
        let mut login: serde_json::Value = serde_json::from_slice(&original).unwrap();
        login[field] = if field == "hub_url" {
            json!("https://foreign.example")
        } else {
            json!("other001")
        };
        std::fs::write(
            daemon.home.login_file(),
            serde_json::to_vec(&login).unwrap(),
        )
        .unwrap();
        assert_eq!(
            error(
                identity_call(
                    &mut client,
                    Some("agent001"),
                    "post",
                    json!({"local_token":"a".repeat(64)})
                )
                .await
            )
            .code,
            ErrorCode::Unauthorized
        );
    }
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn identity_authorization_is_per_request() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    assert_eq!(
        error(
            identity_call(
                &mut client,
                Some("agent001"),
                "whoami",
                json!({"local_token":"a".repeat(64)})
            )
            .await
        )
        .code,
        ErrorCode::Forbidden
    );
    assert_eq!(
        error(identity_call(&mut client, Some("agent001"), "post", json!({})).await).code,
        ErrorCode::Unauthorized
    );
    assert_eq!(result(client.call("whoami", json!({})).await)["id"], HUMAN);
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn agent_cannot_mint() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    for method in [
        "identity_mint",
        "identity_list",
        "console_watch",
        "console_snapshot",
        "chat_history",
        "file_text",
    ] {
        assert_eq!(
            error(
                identity_call(
                    &mut client,
                    Some("agent001"),
                    method,
                    json!({"local_token":"a".repeat(64)})
                )
                .await
            )
            .code,
            ErrorCode::Forbidden
        );
    }
    assert!(daemon.hub.state.posts.lock().unwrap().is_empty());
    assert_eq!(
        daemon
            .hub
            .state
            .mint_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn human_post_is_unchanged() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    assert_eq!(
        result(
            client
                .call(
                    "post",
                    json!({"project":support::PROJECT,"envelope":support::envelope(2,"human")})
                )
                .await
        ),
        json!({"stream":"feed","seq":1})
    );
    let posts = daemon.hub.state.posts.lock().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].0, format!("Bearer {}", support::ACCESS));
}

async fn agent(
    client: &mut Client,
    method: &str,
    mut raw: serde_json::Value,
) -> agmen_local::Response {
    raw["local_token"] = json!("a".repeat(64));
    identity_call(client, Some("agent001"), method, raw).await
}

#[tokio::test]
async fn identity_reads_are_scoped() {
    let daemon = Daemon::seeded().await;
    daemon.hub.state.inject(dm(1, "agent002"));
    let mut client = daemon.client().await;
    for method in ["inbox", "state", "watch"] {
        let mut client = daemon.client().await;
        assert_eq!(
            error(
                agent(
                    &mut client,
                    method,
                    json!({"project":"projectb2","key":"head"})
                )
                .await
            )
            .code,
            ErrorCode::Forbidden
        );
    }
    assert_eq!(
        error(
            agent(
                &mut client,
                "inbox",
                json!({"project":support::PROJECT,"stream":"inbox:agent002"})
            )
            .await
        )
        .code,
        ErrorCode::BadRequest
    );
    let page = result(agent(&mut client, "inbox", json!({"project":support::PROJECT})).await);
    assert_eq!(page["messages"], json!([]));
    assert_eq!(
        error(client.call("pending", json!({})).await).code,
        ErrorCode::BadRequest
    );
    daemon.stop().await;
}

#[tokio::test]
async fn inbox_paginates_without_draining_pending() {
    let daemon = Daemon::seeded().await;
    for seq in 1..=3 {
        daemon.hub.state.inject(dm(seq, "agent001"));
    }
    wait_position(&daemon, "inbox:agent001", 3).await;
    let mut client = daemon.client().await;
    for limit in [0, 501] {
        assert_eq!(
            error(
                agent(
                    &mut client,
                    "inbox",
                    json!({"project":support::PROJECT,"limit":limit})
                )
                .await
            )
            .code,
            ErrorCode::BadRequest
        );
    }
    for _ in 0..2 {
        let page = result(
            agent(
                &mut client,
                "inbox",
                json!({"project":support::PROJECT,"after":1,"limit":1}),
            )
            .await,
        );
        assert_eq!(page["messages"][0]["seq"], 2);
        assert_eq!(page["next_after"], 2);
        assert_eq!(page["head"], 3);
    }
    assert_eq!(
        result(agent(&mut client, "pending", json!({})).await)
            .as_array()
            .unwrap()
            .len(),
        3
    );
    daemon.stop().await;
}

#[tokio::test]
async fn state_preserves_hub_wrapper() {
    let daemon = Daemon::seeded().await;
    daemon.hub.state.publish("hello");
    let mut client = daemon.client().await;
    let state = result(
        agent(
            &mut client,
            "state",
            json!({"project":support::PROJECT,"key":"head"}),
        )
        .await,
    );
    assert_eq!(state["key"], "head");
    assert_eq!(state["as_of_seq"], 1);
    assert_eq!(state["value"]["feed_seq"], 1);
    assert_eq!(
        error(
            agent(
                &mut client,
                "state",
                json!({"project":support::PROJECT,"key":"lanes"})
            )
            .await
        )
        .code,
        ErrorCode::BadRequest
    );
    assert_eq!(
        result(
            client
                .call("state", json!({"project":support::PROJECT,"key":"roster"}))
                .await
        )["value"],
        support::roster("online")
    );
    daemon.stop().await;
}

fn dm(seq: u64, who: &str) -> serde_json::Value {
    let mut message = support::stamped(seq, "private");
    message["id"] = json!(format!("01JBX6Q4T7W3C2M8Y9K5P{seq:05}"));
    message["verb"] = json!("DM");
    message["to"] = json!([who]);
    message["stream"] = json!(format!("inbox:{who}"));
    message
}

async fn wait_position(daemon: &Daemon, stream: &str, seq: u64) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for _ in 0..500 {
            let connection = rusqlite::Connection::open(daemon.home.database()).unwrap();
            let found =
                connection.query_row("SELECT seq FROM positions WHERE stream=?1", [stream], |r| {
                    r.get::<_, i64>(0)
                });
            if found.is_ok_and(|n| n >= seq as i64) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("position did not reach {stream}:{seq}");
    })
    .await
    .expect("collector within 5 s");
}

async fn qualifying_pending(daemon: &Daemon) {
    let mut mention = support::stamped(1, "mention");
    mention["mentioned"] = json!(["agent001"]);
    daemon.hub.state.inject(mention);
    daemon.hub.state.inject(support::stamped(2, "FYI"));
    daemon.hub.state.inject(dm(1, "agent001"));
    let mut assign = support::stamped(3, "assignment");
    assign["verb"] = json!("ASSIGN");
    assign["data"] = json!({"item":"B-5","to":"agent001"});
    daemon.hub.state.inject(assign);
    daemon.hub.state.inject(dm(1, "agent002"));
    let mut own = support::stamped(4, "self");
    own["from"] = json!("agent001");
    own["mentioned"] = json!(["agent001"]);
    daemon.hub.state.inject(own);
    wait_position(daemon, "feed", 4).await;
    wait_position(daemon, "inbox:agent001", 1).await;
}

async fn assert_pending(daemon: &Daemon) {
    let mut client = daemon.client().await;
    let rows = result(agent(&mut client, "pending", json!({})).await);
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    let feed: Vec<_> = rows
        .iter()
        .filter(|r| r["stream"] == "feed")
        .map(|r| r["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(feed, [1, 3]);
    assert_eq!(
        result(agent(&mut client, "pending", json!({})).await),
        json!([])
    );
}

#[tokio::test]
async fn pending_collects_mentions_and_dms() {
    let daemon = Daemon::seeded().await;
    let mut watch = daemon.client().await;
    let first = agent(&mut watch, "watch", json!({"project":support::PROJECT})).await;
    assert_eq!(result(first)["kind"], "self");
    qualifying_pending(&daemon).await;
    assert_pending(&daemon).await;
    daemon.stop().await;
}

#[tokio::test]
async fn pending_collects_without_watch_clients() {
    let daemon = Daemon::seeded().await;
    qualifying_pending(&daemon).await;
    assert_pending(&daemon).await;
    daemon.stop().await;
}

#[tokio::test]
async fn pending_survives_restart() {
    let mut daemon = Daemon::seeded().await;
    daemon.hub.state.inject(dm(1, "agent001"));
    wait_position(&daemon, "inbox:agent001", 1).await;
    daemon.restart().await;
    let mut client = daemon.client().await;
    assert_eq!(
        result(agent(&mut client, "pending", json!({})).await)
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        result(agent(&mut client, "pending", json!({})).await),
        json!([])
    );
    daemon.stop().await;
}

#[tokio::test]
async fn pending_replays_after_cursor_crash_window() {
    let mut daemon = Daemon::seeded().await;
    daemon.pause().await;
    // The client cursor is independent of the daemon position. A crash
    // after the client's commit must not advance the daemon's position.
    let dir = daemon.home.dir.path().join("identities/agent001");
    std::fs::create_dir_all(&dir).unwrap();
    let cursors = agmen_client::Cursors::open(dir.join("agmen.db")).unwrap();
    cursors
        .advance(&"inbox:agent001".parse().unwrap(), 1)
        .unwrap();
    daemon.hub.state.inject(dm(1, "agent001"));
    daemon.resume();
    wait_position(&daemon, "inbox:agent001", 1).await;
    let mut client = daemon.client().await;
    assert_eq!(
        result(agent(&mut client, "pending", json!({})).await)
            .as_array()
            .unwrap()
            .len(),
        1
    );
    daemon.stop().await;
}

#[tokio::test]
async fn pending_cross_stream_copy_is_once() {
    let daemon = Daemon::seeded().await;
    let mut message = support::stamped(1, "assignment");
    message["verb"] = json!("ASSIGN");
    message["data"] = json!({"item":"B-5","to":"agent001"});
    daemon.hub.state.inject(message.clone());
    wait_position(&daemon, "feed", 1).await;
    let mut client = daemon.client().await;
    assert_eq!(
        result(agent(&mut client, "pending", json!({})).await)
            .as_array()
            .unwrap()
            .len(),
        1
    );
    message["stream"] = json!("inbox:agent001");
    daemon.hub.state.inject(message);
    wait_position(&daemon, "inbox:agent001", 1).await;
    assert_eq!(
        result(agent(&mut client, "pending", json!({})).await),
        json!([])
    );
    daemon.stop().await;
}

#[tokio::test]
async fn pending_frame_cap_keeps_undelivered_tail() {
    let daemon = Daemon::seeded().await;
    for seq in 1..=300 {
        let mut message = dm(seq, "agent001");
        message["body"] = json!("\"".repeat(30_000));
        daemon.hub.state.inject(message);
    }
    wait_position(&daemon, "inbox:agent001", 300).await;
    let mut client = daemon.client().await;
    let first = result(agent(&mut client, "pending", json!({})).await);
    let first = first.as_array().unwrap();
    assert!(!first.is_empty() && first.len() < 300);
    assert_eq!(first[0]["seq"], 1);
    let second = result(agent(&mut client, "pending", json!({})).await);
    assert_eq!(second[0]["seq"], first.len() + 1);
    assert_eq!(first.len() + second.as_array().unwrap().len(), 300);
    daemon.stop().await;
}

#[tokio::test]
async fn pending_on_disk_failure_is_error() {
    let daemon = Daemon::seeded().await;
    let connection = rusqlite::Connection::open(daemon.home.database()).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_pending BEFORE INSERT ON pending BEGIN SELECT RAISE(FAIL,'injected disk failure'); END;").unwrap();
    let mut client = daemon.client().await;
    let mut watch = daemon.client().await;
    assert_eq!(
        result(agent(&mut watch, "watch", json!({"project":support::PROJECT})).await)["kind"],
        "self"
    );
    daemon.hub.state.inject(dm(1, "agent001"));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for _ in 0..10 {
            let response = watch.read().await;
            if matches!(response.outcome, agmen_local::Outcome::Err { .. }) {
                return;
            }
        }
        panic!("expected collector error");
    })
    .await
    .unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM positions WHERE stream='inbox:agent001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        result(agent(&mut client, "pending", json!({})).await),
        json!([])
    );
    daemon.stop().await;
}

#[tokio::test]
async fn watch_streams_events() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    assert_eq!(
        result(agent(&mut client, "watch", json!({"project":support::PROJECT})).await)["kind"],
        "self"
    );
    daemon.hub.state.publish("one");
    daemon.hub.state.publish("two");
    daemon.hub.state.inject(dm(1, "agent002"));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut seq = 0;
        for _ in 0..10 {
            let response = client.read().await;
            assert_eq!(response.id, 99);
            let event = result(response);
            if event["kind"] == "message" {
                seq += 1;
                assert_eq!(event["message"]["stream"], "feed");
                assert_eq!(event["message"]["seq"], seq);
                if seq == 2 {
                    return;
                }
            }
        }
        panic!("missing messages");
    })
    .await
    .unwrap();
    daemon.stop().await;
}

#[tokio::test]
async fn watch_slow_reader_closes() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    result(agent(&mut client, "watch", json!({"project":support::PROJECT})).await);
    for seq in 1..=600 {
        daemon
            .hub
            .state
            .inject(support::stamped(seq, &"x".repeat(32_000)));
    }
    wait_position(&daemon, "feed", 600).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for _ in 0..605 {
            match client.try_read().await {
                Err(_) => return,
                Ok(response) if matches!(response.outcome, agmen_local::Outcome::Err { .. }) => {
                    return;
                }
                Ok(_) => {}
            }
        }
        panic!("slow reader silently lost messages");
    })
    .await
    .unwrap();
    daemon.stop().await;
}

#[tokio::test]
async fn collector_ignores_inactive_owners() {
    let daemon = Daemon::seeded().await;
    let connection = rusqlite::Connection::open(daemon.home.database()).unwrap();
    connection.execute("UPDATE identities SET owner='other001',metadata=json_set(metadata,'$.owner_id','other001') WHERE identity='agent002'",[]).unwrap();
    daemon.hub.state.inject(dm(1, "agent002"));
    wait_position(&daemon, "inbox:agent002", 1).await;
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM pending WHERE identity='agent002'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 0,
        "the current login must not enqueue another owner's messages"
    );
    daemon.stop().await;
}

#[tokio::test]
async fn collector_repairs_gap_and_reconnects_from_all_positions() {
    let daemon = Daemon::seeded().await;
    daemon.hub.state.inject(dm(1, "agent001"));
    wait_position(&daemon, "inbox:agent001", 1).await;
    daemon.hub.state.fill(2);
    daemon.hub.state.publish("third");
    wait_position(&daemon, "feed", 3).await;
    daemon.hub.state.drop_streams();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for _ in 0..500 {
            if daemon
                .hub
                .state
                .event_afters
                .lock()
                .unwrap()
                .iter()
                .flatten()
                .any(|after| after.contains("feed:3") && after.contains("inbox:agent001:1"))
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("collector did not reconnect from both durable positions");
    })
    .await
    .unwrap();
    daemon.stop().await;
}

#[tokio::test]
async fn watch_closes_after_identity_removal() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    result(agent(&mut client, "watch", json!({"project":support::PROJECT})).await);
    let connection = rusqlite::Connection::open(daemon.home.database()).unwrap();
    connection
        .execute("DELETE FROM identities WHERE identity='agent001'", [])
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for _ in 0..10 {
            match client.try_read().await {
                Err(_) => return,
                Ok(response) if matches!(response.outcome, agmen_local::Outcome::Err { .. }) => {
                    return;
                }
                Ok(_) => {}
            }
        }
        panic!("removed identity retained its watch");
    })
    .await
    .unwrap();
    daemon.stop().await;
}

#[tokio::test]
async fn human_watch_without_managed_identities_stays_open() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    assert_eq!(
        result(
            client
                .call("watch", json!({"project":support::PROJECT}))
                .await
        )["identity_id"],
        HUMAN
    );
    // Hold across several supervisor ticks before publishing.
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
    daemon.hub.state.publish("still open");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for _ in 0..10 {
            let event = result(client.read().await);
            if event["kind"] == "message" {
                assert_eq!(event["message"]["body"], "still open");
                return;
            }
        }
        panic!("human watch stopped");
    })
    .await
    .unwrap();
    daemon.stop().await;
}

#[tokio::test]
async fn pending_owner_switch_uses_independent_feed_position() {
    use agmen_client::{HumanTokens, Login, TokenKey, store_tokens};
    let mut daemon = Daemon::seeded().await;
    let connection = rusqlite::Connection::open(daemon.home.database()).unwrap();
    connection.execute("UPDATE identities SET owner='other001',metadata=json_set(metadata,'$.owner_id','other001') WHERE identity='agent002'",[]).unwrap();
    let mut message = support::stamped(1, "for the other owner");
    message["mentioned"] = json!(["agent002"]);
    daemon.hub.state.inject(message);
    wait_position(&daemon, "feed", 1).await;
    daemon.pause().await;
    let login = Login {
        hub_url: daemon.hub.url.parse().unwrap(),
        human_id: "other001".parse().unwrap(),
        handle: "other".into(),
    };
    std::fs::write(
        daemon.home.login_file(),
        serde_json::to_vec(&login).unwrap(),
    )
    .unwrap();
    store_tokens(
        &daemon.home.tokens(),
        &login,
        &HumanTokens::new(support::ACCESS.into(), "refresh-other".into(), 3600),
    )
    .await
    .unwrap();
    daemon.home.tokens().set(&TokenKey::new(&"https://agmend.invalid".parse().unwrap(),&"agent002".parse().unwrap()).unwrap(),&json!({"origin":login.hub_url.origin().ascii_serialization(),"owner_id":"other001","local_token":"b".repeat(64)}).to_string()).unwrap();
    daemon.resume();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for _ in 0..500 {
            let count: i64 = connection
                .query_row(
                    "SELECT count(*) FROM pending WHERE identity='agent002'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            if count == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("second owner's feed message was skipped");
    })
    .await
    .expect("second owner catches up from its own durable position");
    let mut client = daemon.client().await;
    let rows = result(
        identity_call(
            &mut client,
            Some("agent002"),
            "pending",
            json!({"local_token":"b".repeat(64)}),
        )
        .await,
    );
    assert_eq!(rows[0]["body"], "for the other owner");
    daemon.stop().await;
}

#[tokio::test]
async fn shutdown_refuses_agent_identity() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    assert_eq!(
        error(
            identity_call(
                &mut client,
                Some("agent001"),
                "shutdown",
                json!({"local_token": "a".repeat(64)})
            )
            .await
        )
        .code,
        ErrorCode::Forbidden
    );
    assert_eq!(result(client.call("ready", json!({})).await)["ready"], true);
}

#[tokio::test]
async fn shutdown_owner_acknowledges_then_releases_socket_and_lock() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    assert_eq!(result(client.call("shutdown", json!({})).await), json!({}));
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        for _ in 0..1500 {
            if !daemon.home.socket().exists() {
                drop(Agmend::bind(daemon.home.config()).unwrap());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("shutdown did not release the socket");
    })
    .await
    .unwrap();
}
