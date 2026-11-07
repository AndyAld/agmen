//! Local coordination and routing beside the existing hub origin.

#![cfg(test)]
#![cfg(feature = "test-hooks")]

mod support;

use std::time::Duration;

use agmen_local::ErrorCode;
use agmen_proto::{ClientEnvelope, DeliveryReply, ItemKey, Payload, ProjectId, Timestamp};
use agmend::{Agmend, Config};
use serde_json::{Value, json};
use support::{Client, Daemon, Home, PROJECT, error, result};

const SHORT_LEASE: Duration = Duration::from_millis(200);
const SWEEP_WAIT: Duration = Duration::from_secs(2);
const POLL_EVERY: Duration = Duration::from_millis(20);
const SWEEP_POLLS: usize = 100;
/// The claim lease in the heartbeat test. The sweeper looks every quarter lease.
const HEARTBEAT_LEASE: Duration = Duration::from_secs(4);
/// How long after its claim the holder sends the HEARTBEAT: 1.5 s inside the
/// lease, and 1.5 s more than the sweeper's 1 s cadence plus a poll, so the
/// unrenewed claim is seen released while the renewed one has over a second left.
const HEARTBEAT_AFTER: Duration = Duration::from_millis(2_500);
/// The longest any wait for a RELEASE lasts: `LEASE_POLLS` rounds of `POLL_EVERY`.
const LEASE_WAIT: Duration = Duration::from_secs(10);
const LEASE_POLLS: usize = 500;
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

/// The project's feed as the console reads it, oldest first.
async fn feed(client: &mut Client, project: &ProjectId) -> Vec<Value> {
    result(
        client
            .call("console_snapshot", json!({"project": project}))
            .await,
    )["messages"]
        .as_array()
        .unwrap()
        .clone()
}

/// Whether the feed holds the RELEASE `lease_expired` for `item`.
fn lease_expired(feed: &[Value], item: &ItemKey) -> bool {
    feed.iter().any(|message| {
        message["verb"] == "RELEASE"
            && message["data"]["reason"] == "lease_expired"
            && message["data"]["item"] == item.as_str()
    })
}

/// The feed at the first poll where `done` holds, within `LEASE_WAIT`.
async fn feed_until(
    client: &mut Client,
    project: &ProjectId,
    done: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    tokio::time::timeout(LEASE_WAIT, async {
        for _ in 0..LEASE_POLLS {
            let messages = feed(client, project).await;
            if done(&messages) {
                return messages;
            }
            tokio::time::sleep(POLL_EVERY).await;
        }
        panic!("the feed never reached the expected state");
    })
    .await
    .unwrap()
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

/// A HEARTBEAT is routed to the sender's own inbox only (agmen-core routing);
/// what it does is renew the sender's leases. The unrenewed claim beside it
/// shows the sweeper had passed the first lease by the time the renewed one is
/// still held.
#[tokio::test]
async fn local_heartbeat_keeps_a_claim_past_its_first_lease() {
    let home = unsigned_home().await;
    let (mut client, stop, task, local) =
        start(&home, home.config().local_claim_lease(HEARTBEAT_LEASE)).await;
    let created = create(&mut client, "heartbeats").await;
    let project: ProjectId = created["project"].as_str().unwrap().parse().unwrap();
    let renewed_by = local.mint_agent(&project, "renewed").await.unwrap();
    let unrenewed_by = local.mint_agent(&project, "unrenewed").await.unwrap();
    for title in ["renewed", "unrenewed"] {
        let found: Payload =
            serde_json::from_value(json!({"verb": "FOUND", "data": {"title": title}})).unwrap();
        let reply = local
            .ingest(&project, &renewed_by, &envelope(found))
            .await
            .unwrap();
        assert!(matches!(reply, DeliveryReply::Ack { .. }), "{reply:?}");
    }
    let keys: Vec<ItemKey> = feed(&mut client, &project)
        .await
        .iter()
        .filter(|message| message["verb"] == "ITEM")
        .map(|message| message["data"]["key"].as_str().unwrap().parse().unwrap())
        .collect();
    let [renewed, unrenewed] = <[ItemKey; 2]>::try_from(keys).unwrap();
    for (by, item) in [(&renewed_by, &renewed), (&unrenewed_by, &unrenewed)] {
        let claim = Payload::Claim { item: item.clone() };
        let reply = local.ingest(&project, by, &envelope(claim)).await.unwrap();
        assert!(matches!(reply, DeliveryReply::Ack { .. }), "{reply:?}");
    }
    // Both are granted, so a missing RELEASE below means a held claim.
    let granted = feed(&mut client, &project).await;
    for item in [&renewed, &unrenewed] {
        assert!(
            granted
                .iter()
                .any(|message| message["verb"] == "GRANT"
                    && message["data"]["item"] == item.as_str()),
            "{item} was not granted"
        );
    }
    tokio::time::sleep(HEARTBEAT_AFTER).await;
    let beat = Payload::Heartbeat {
        items: vec![renewed.clone()],
    };
    let reply = local
        .ingest(&project, &renewed_by, &envelope(beat))
        .await
        .unwrap();
    assert!(matches!(reply, DeliveryReply::Ack { .. }), "{reply:?}");
    // The claim without a HEARTBEAT lapses at the first lease; the sweeper has
    // run past it when its RELEASE shows, and the renewed claim is still held.
    let seen = feed_until(&mut client, &project, |feed| {
        lease_expired(feed, &unrenewed)
    })
    .await;
    assert!(
        !lease_expired(&seen, &renewed),
        "the HEARTBEAT did not keep the claim past its first lease"
    );
    // One lease after the HEARTBEAT it lapses too.
    feed_until(&mut client, &project, |feed| lease_expired(feed, &renewed)).await;
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
