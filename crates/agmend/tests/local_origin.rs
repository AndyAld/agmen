//! Local coordination and routing beside the existing hub origin.

#![cfg(test)]
#![cfg(feature = "test-hooks")]

mod support;

use std::time::Duration;

use agmen_local::ErrorCode;
use agmen_proto::{ClientEnvelope, DeliveryReply, Payload, ProjectId, Timestamp};
use agmend::{Agmend, Config};
use serde_json::{Value, json};
use support::{Client, Daemon, Home, PROJECT, error, result};

const SHORT_LEASE: Duration = Duration::from_millis(200);
const SWEEP_WAIT: Duration = Duration::from_secs(2);
const POLL_EVERY: Duration = Duration::from_millis(20);
const SWEEP_POLLS: usize = 100;
const OWNER_METHODS: [&str; 3] = ["local_project_create", "local_projects", "lanes_set"];

async fn unsigned_home() -> Home {
    let dir = tempfile::Builder::new()
        .prefix("local")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    Home { dir }
}

async fn start(
    home: &Home,
    config: Config,
) -> (
    Client,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
    agmend::LocalOriginTest,
) {
    let daemon = Agmend::bind(config).unwrap();
    let local = daemon.test_local_origin();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(daemon.serve_until(async {
        stopped.await.unwrap();
    }));
    let mut client = Client::connect(&home.socket()).await;
    result(client.call("hello", json!({"api": 1})).await);
    (client, stop, task, local)
}

async fn create(client: &mut Client, name: &str) -> Value {
    result(
        client
            .call("local_project_create", json!({"name": name}))
            .await,
    )
}

fn envelope(payload: Payload) -> ClientEnvelope {
    let now = Timestamp::now();
    ClientEnvelope {
        v: 1,
        id: agmen_core::new_message_id(now).unwrap(),
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: "local origin test".try_into().unwrap(),
        valid_until: Some(now + SWEEP_WAIT),
        payload,
        sent_at: now,
    }
}

#[tokio::test]
async fn local_project_create_needs_no_login() {
    let home = unsigned_home().await;
    let (mut client, stop, task, _) = start(&home, home.config()).await;
    assert!(!home.dir.path().join("local.db").exists());
    let created = create(&mut client, "solo").await;
    assert!(
        created["project"]
            .as_str()
            .unwrap()
            .parse::<ProjectId>()
            .is_ok()
    );
    assert!(created["tenant"].is_string());
    assert!(created["person"]["id"].is_string());
    let projects = result(client.call("local_projects", json!({})).await);
    assert_eq!(
        projects,
        json!({"projects": [{"project": created["project"], "name": "solo"}]})
    );
    assert_eq!(
        result(client.call("ready", json!({})).await),
        json!({"ready": true, "schemas": {"agmend.db": 2, "local.db": 1}})
    );
    let stores = agmend::store_snapshot(home.dir.path()).unwrap();
    assert!(stores["stores"].as_array().unwrap().iter().any(|store| {
        store["path"].as_str().unwrap().ends_with("/local.db") && store["schema_version"] == 1
    }));
    stop.send(()).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn unknown_project_without_login_names_both_origins() {
    let home = unsigned_home().await;
    let (mut client, stop, task, _) = start(&home, home.config()).await;
    let err = error(
        client
            .call("console_snapshot", json!({"project": "unknown1"}))
            .await,
    );
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(err.message.contains("this machine"), "{err:?}");
    assert!(err.message.contains("not signed in to a hub"), "{err:?}");
    assert!(!home.dir.path().join("local.db").exists());
    stop.send(()).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn hub_project_is_never_served_locally() {
    let daemon = Daemon::start().await;
    daemon.hub.state.fill(3);
    let mut client = daemon.client().await;
    let created = create(&mut client, "beside-hub").await;
    assert_eq!(created["person"]["handle"], "andy");
    let snapshot = result(
        client
            .call("console_snapshot", json!({"project": PROJECT}))
            .await,
    );
    assert_eq!(snapshot["messages"].as_array().unwrap().len(), 3);
    assert_eq!(snapshot["messages"][0]["body"], "message 1");
    assert_eq!(snapshot["me"]["id"], support::HUMAN);
    daemon.stop().await;
}

#[tokio::test]
async fn newer_record_leaves_hub_projects_working() {
    let hub = support::fake_hub().await;
    hub.state.fill(1);
    let home = Home::signed_in(&hub.url).await;
    let path = home.dir.path().join("local.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    let (mut client, stop, task, _) = start(&home, home.config()).await;
    let err = error(client.call("local_projects", json!({})).await);
    assert_eq!(err.code, ErrorCode::Version);
    assert!(err.message.contains("local.db"), "{err:?}");
    let snapshot = result(
        client
            .call("console_snapshot", json!({"project": PROJECT}))
            .await,
    );
    assert_eq!(snapshot["messages"][0]["body"], "message 1");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!path.with_file_name(format!("local.db{suffix}")).exists());
    }
    stop.send(()).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn ready_reports_a_newer_record_that_local_calls_refuse() {
    let hub = support::fake_hub().await;
    let home = Home::signed_in(&hub.url).await;
    let path = home.dir.path().join("local.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    let (mut client, stop, task, _) = start(&home, home.config()).await;
    let ready = result(client.call("ready", json!({})).await);
    let answer: agmen_local::ReadyAnswer = serde_json::from_value(ready).unwrap();
    assert!(answer.ready);
    assert_eq!(answer.schemas["agmend.db"], 2);
    assert_eq!(answer.schemas["local.db"], 2);
    let stores = agmend::store_snapshot(home.dir.path()).unwrap();
    assert!(stores["stores"].as_array().unwrap().iter().any(|store| {
        store["path"].as_str().unwrap().ends_with("/local.db") && store["schema_version"] == 2
    }));
    for (method, params) in [
        ("local_projects", json!({})),
        ("local_project_create", json!({"name": "later"})),
    ] {
        let err = error(client.call(method, params).await);
        assert_eq!(err.code, ErrorCode::Version);
        assert!(err.message.contains("local.db"), "{err:?}");
        assert!(!err.message.contains("identity store"), "{err:?}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!path.with_file_name(format!("local.db{suffix}")).exists());
    }
    stop.send(()).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn newer_wal_record_is_refused_without_touching_its_files() {
    let home = unsigned_home().await;
    let source = tempfile::tempdir().unwrap();
    let source_path = source.path().join("future.db");
    let conn = rusqlite::Connection::open(&source_path).unwrap();
    conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE future(marker TEXT); PRAGMA user_version=2; INSERT INTO future VALUES ('preserve');").unwrap();
    let path = home.dir.path().join("local.db");
    let before: Vec<_> = ["", "-wal"]
        .into_iter()
        .map(|suffix| {
            let bytes =
                std::fs::read(source_path.with_file_name(format!("future.db{suffix}"))).unwrap();
            let target = path.with_file_name(format!("local.db{suffix}"));
            std::fs::write(&target, &bytes).unwrap();
            (target, bytes)
        })
        .collect();
    let (mut client, stop, task, _) = start(&home, home.config()).await;
    for method in ["local_projects", "local_project_create"] {
        let params = if method == "local_projects" {
            json!({})
        } else {
            json!({"name": "preserve"})
        };
        let err = error(client.call(method, params).await);
        assert_eq!(err.code, ErrorCode::Version);
        assert!(err.message.contains("local.db"), "{err:?}");
    }
    for (target, bytes) in before {
        assert!(
            std::fs::read(&target).unwrap() == bytes,
            "{} changed",
            target.display()
        );
    }
    for suffix in ["-shm", "-journal"] {
        assert!(!path.with_file_name(format!("local.db{suffix}")).exists());
    }
    stop.send(()).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn lease_sweeper_runs_for_the_local_origin() {
    let home = unsigned_home().await;
    let (mut client, stop, task, local) =
        start(&home, home.config().local_claim_lease(SHORT_LEASE)).await;
    let created = create(&mut client, "leases").await;
    let project = created["project"].as_str().unwrap().parse().unwrap();
    let agent = local.mint_agent(&project, "worker").await.unwrap();
    let found: Payload =
        serde_json::from_value(json!({"verb": "FOUND", "data": {"title": "test item"}})).unwrap();
    assert!(matches!(
        local
            .ingest(&project, &agent, &envelope(found))
            .await
            .unwrap(),
        DeliveryReply::Ack { .. }
    ));
    let snapshot = result(
        client
            .call("console_snapshot", json!({"project": project}))
            .await,
    );
    let item = snapshot["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["verb"] == "ITEM")
        .unwrap();
    let key = item["data"]["key"].as_str().unwrap().parse().unwrap();
    assert!(matches!(
        local
            .ingest(&project, &agent, &envelope(Payload::Claim { item: key }))
            .await
            .unwrap(),
        DeliveryReply::Ack { .. }
    ));
    tokio::time::timeout(SWEEP_WAIT, async {
        for _ in 0..SWEEP_POLLS {
            let snapshot = result(
                client
                    .call("console_snapshot", json!({"project": project}))
                    .await,
            );
            if snapshot["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| {
                    message["verb"] == "RELEASE" && message["data"]["reason"] == "lease_expired"
                })
            {
                return;
            }
            tokio::time::sleep(POLL_EVERY).await;
        }
        panic!("local lease was not released");
    })
    .await
    .unwrap();
    stop.send(()).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn rollcall_is_answered_locally() {
    let home = unsigned_home().await;
    let (mut client, stop, task, local) = start(&home, home.config()).await;
    let created = create(&mut client, "rollcall").await;
    let project = created["project"].as_str().unwrap().parse().unwrap();
    let call = envelope(Payload::Rollcall {});
    local.post_as_person(&project, &call).await.unwrap();
    let snapshot = result(
        client
            .call("console_snapshot", json!({"project": project}))
            .await,
    );
    assert_eq!(snapshot["messages"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["messages"][0]["from"], created["person"]["id"]);
    assert_eq!(snapshot["messages"][1]["verb"], "STATUS");
    assert_eq!(snapshot["messages"][1]["reply_to"], call.id.as_str());
    assert_eq!(snapshot["messages"][1]["from_kind"], "hub");
    stop.send(()).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn invalid_project_names_do_not_create_a_record() {
    let home = unsigned_home().await;
    let (mut client, stop, task, _) = start(&home, home.config()).await;
    for name in ["", " padded", "padded ", "control\nname", &"x".repeat(65)] {
        let err = error(
            client
                .call("local_project_create", json!({"name": name}))
                .await,
        );
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(err.message.contains("1 to 64"), "{err:?}");
        assert!(err.message.contains("control"), "{err:?}");
        assert!(err.message.contains("whitespace"), "{err:?}");
        assert!(!home.dir.path().join("local.db").exists());
    }
    stop.send(()).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn local_owner_methods_refuse_identity_callers() {
    let daemon = Daemon::seeded().await;
    let mut client = daemon.client().await;
    for method in OWNER_METHODS {
        let mut request =
            agmen_local::Request::new(99, method, json!({"local_token": "a".repeat(64)}));
        request.as_ = Some("agent001".parse().unwrap());
        client.send(&request).await;
        assert_eq!(error(client.read().await).code, ErrorCode::Forbidden);
    }
    assert!(!daemon.home.dir.path().join("local.db").exists());
    daemon.stop().await;
}

#[tokio::test]
async fn lanes_set_is_local_only_and_keeps_the_hub_shape() {
    let home = unsigned_home().await;
    let (mut client, stop, task, _) = start(&home, home.config()).await;
    let created = create(&mut client, "lanes").await;
    let project = &created["project"];
    let lanes = result(
        client
            .call(
                "lanes_set",
                json!({"project": project, "lanes": [{"name": "L-UI", "globs": ["ui/**"]}]}),
            )
            .await,
    );
    assert_eq!(
        lanes,
        json!({"v": 1, "lanes": [{"name": "L-UI", "globs": ["ui/**"], "owner": null, "owner_handle": null}]})
    );
    let err = error(
        client
            .call("lanes_set", json!({"project": PROJECT, "lanes": []}))
            .await,
    );
    assert_eq!(err.code, ErrorCode::NotFound);
    let err = error(
        client
            .call(
                "lanes_set",
                json!({"project": project, "lanes": [{"name": "L-UI", "globs": ["["]}]}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert_eq!(
        result(
            client
                .call("lanes_set", json!({"project": project, "lanes": []}))
                .await
        ),
        json!({"v": 0, "lanes": []})
    );
    stop.send(()).unwrap();
    task.await.unwrap();
}
