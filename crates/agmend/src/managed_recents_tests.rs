use super::*;
use agmen_proto::Timestamp;

async fn listed(fixture: &Fixture) -> Value {
    worker_call(fixture, "agent_list", json!({})).await.unwrap()["agents"][0].clone()
}

fn agent_dir(fixture: &Fixture, id: &str) -> std::path::PathBuf {
    fixture.daemon.core.managed.agent_dir(&id.parse().unwrap())
}

#[tokio::test]
async fn stop_records_when_the_agent_stopped() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    let before = Timestamp::now();
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    let stopped: Timestamp =
        serde_json::from_value(listed(&fixture).await["stopped_at"].clone()).unwrap();
    assert!(before <= stopped && stopped <= Timestamp::now());
    let path = agent_dir(&fixture, "agent001").join("stopped.json");
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved, json!({"stopped_at": stopped}));
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn already_offline_stop_keeps_the_first_time() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    let first = listed(&fixture).await["stopped_at"].clone();
    assert!(first.as_str().is_some());
    let path = agent_dir(&fixture, "agent001").join("stopped.json");
    let saved = std::fs::read(&path).unwrap();
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    assert_eq!(listed(&fixture).await["stopped_at"], first);
    assert_eq!(std::fs::read(path).unwrap(), saved);
}

#[tokio::test]
async fn quit_marks_every_agent_stopped() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    root(&fixture).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    tier(&fixture, "kid00001", Some(Tier::Owner)).await;
    child(&fixture, "kid00001", "k2").await.unwrap();
    fixture.daemon.core.managed.shutdown().await;
    let mut previous = None;
    for id in ["kid00002", "kid00001", "agent001"] {
        let saved: Value = serde_json::from_slice(
            &std::fs::read(agent_dir(&fixture, id).join("stopped.json")).unwrap(),
        )
        .unwrap();
        let stopped: Timestamp = serde_json::from_value(saved["stopped_at"].clone()).unwrap();
        assert!(previous.is_none_or(|previous| previous <= stopped));
        previous = Some(stopped);
    }
}

#[tokio::test]
async fn crashed_agent_falls_back_to_its_last_activity() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    let dir = agent_dir(&fixture, "agent001");
    let path = dir.join("status.json");
    std::fs::write(&path, r#"{"spent_usd":"0"}"#).unwrap();
    let activity = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(activity)
        .unwrap();
    let expected =
        Timestamp::try_from(std::fs::metadata(path).unwrap().modified().unwrap()).unwrap();
    let loaded = restarted(&fixture).await;
    let listed = loaded.list(json!({})).await.unwrap();
    assert_eq!(listed["agents"][0]["state"], "offline");
    assert_eq!(listed["agents"][0]["stopped_at"], json!(expected));
    assert!(!dir.join("stopped.json").exists());

    for invalid in [
        "not json",
        r#"{"stopped_at":"invalid"}"#,
        r#"{"stopped_at":"2026-11-07T00:00:00Z","extra":true}"#,
    ] {
        std::fs::write(dir.join("stopped.json"), invalid).unwrap();
        let loaded = restarted(&fixture).await;
        assert_eq!(
            loaded.list(json!({})).await.unwrap()["agents"][0]["stopped_at"],
            json!(expected)
        );
    }
    std::fs::remove_file(dir.join("status.json")).unwrap();
    let expected = Timestamp::try_from(
        std::fs::metadata(dir.join("agent.json"))
            .unwrap()
            .modified()
            .unwrap(),
    )
    .unwrap();
    let loaded = restarted(&fixture).await;
    assert_eq!(
        loaded.list(json!({})).await.unwrap()["agents"][0]["stopped_at"],
        json!(expected)
    );
    std::fs::remove_file(dir.join("agent.json")).unwrap();
    assert_eq!(crate::managed::recents::read_stopped(&dir), None);
}

#[tokio::test]
async fn start_clears_the_stop_mark() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    assert!(listed(&fixture).await["stopped_at"].as_str().is_some());
    worker_call(&fixture, "agent_start", json!({"identity":"agent001"}))
        .await
        .unwrap();
    let entry = listed(&fixture).await;
    assert_eq!(entry["state"], "idle");
    assert_eq!(entry.get("stopped_at"), Some(&Value::Null));
    assert!(
        !agent_dir(&fixture, "agent001")
            .join("stopped.json")
            .exists()
    );
    let path = agent_dir(&fixture, "agent001").join("stopped.json");
    std::fs::create_dir(&path).unwrap();
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    assert!(listed(&fixture).await["stopped_at"].as_str().is_some());
    worker_call(&fixture, "agent_start", json!({"identity":"agent001"}))
        .await
        .unwrap();
    assert_eq!(listed(&fixture).await.get("stopped_at"), Some(&Value::Null));
    assert!(path.is_dir());
}

#[tokio::test]
async fn loaded_local_agents_report_whether_their_identity_can_resume() {
    let fixture = Fixture::new().await;
    let project = worker_call(&fixture, "local_project_create", json!({"name":"recents"}))
        .await
        .unwrap()["project"]
        .clone();
    let checkout = worker_call(
        &fixture,
        "local_mint",
        json!({"project":project,"name":"checkout"}),
    )
    .await
    .unwrap();
    std::fs::write(
        fixture.dir.path().join("repo/.agmen/identity.json"),
        serde_json::to_vec(&checkout).unwrap(),
    )
    .unwrap();
    let worker = worker_call(
        &fixture,
        "agent_launch",
        json!({"project":project,"name":"worker","root":fixture.dir.path().join("repo")}),
    )
    .await
    .unwrap();
    worker_call(
        &fixture,
        "agent_stop",
        json!({"identity":worker["identity"]}),
    )
    .await
    .unwrap();
    let loaded = restarted(&fixture).await;
    assert_eq!(
        loaded.list(json!({})).await.unwrap()["agents"][0]["resumable"],
        true
    );
    let id = worker["identity"].as_str().unwrap().parse().unwrap();
    let project = project.as_str().unwrap().parse().unwrap();
    assert!(
        fixture
            .daemon
            .core
            .origins
            .local()
            .revoke(&project, &id)
            .await
            .unwrap()
    );
    let loaded = restarted(&fixture).await;
    assert_eq!(
        loaded.list(json!({})).await.unwrap()["agents"][0]["resumable"],
        false
    );
    let refusal = loaded.start(json!({"identity":id})).await.unwrap_err();
    assert_eq!(
        refusal.message,
        "local identity has been revoked; launch a new agent"
    );
}
