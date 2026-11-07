use super::stop::publisher;
use super::*;
use agmen_proto::Timestamp;

const RESUME_TEST_WAIT: std::time::Duration = std::time::Duration::from_secs(9);

async fn stopped_tree(fixture: &Fixture) {
    settings(fixture, "budgets = false");
    root(fixture).await;
    child(fixture, "agent001", "k1").await.unwrap();
    tier(fixture, "kid00001", Some(Tier::Owner)).await;
    child(fixture, "kid00001", "k2").await.unwrap();
    worker_call(fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
}

async fn assert_states(fixture: &Fixture, expected: &[(&str, &str)]) {
    for (id, state) in expected {
        assert_eq!(
            worker_of(&fixture.daemon.core.managed, id)
                .await
                .lock()
                .await
                .state,
            *state,
            "{id}"
        );
    }
}

#[tokio::test]
async fn resume_tree_starts_parents_before_children() {
    let (mut fixture, _) = tree_fixture().await;
    let senders = Arc::new(Mutex::new(Vec::new()));
    let captured = senders.clone();
    publisher(
        &mut fixture,
        Arc::new(move |sender, envelope| {
            if matches!(envelope.payload, agmen_proto::Payload::Hello { .. }) {
                captured
                    .lock()
                    .unwrap()
                    .push(sender.identity_id.to_string());
            }
            Box::pin(async { Ok(OutboxState::Pending { attempts: 0 }) })
        }),
    )
    .await;
    stopped_tree(&fixture).await;
    senders.lock().unwrap().clear();
    let answer = worker_call(
        &fixture,
        "agent_start",
        json!({"identity":"agent001","subtree":true}),
    )
    .await
    .unwrap();
    assert_eq!(answer["identity"], "agent001");
    assert_eq!(answer["state"], "idle");
    assert!(answer["context"].is_string());
    assert_eq!(
        answer["started"],
        json!(["agent001", "kid00001", "kid00002"])
    );
    assert_eq!(answer["skipped"], json!([]));
    assert_eq!(
        *senders.lock().unwrap(),
        ["agent001", "kid00001", "kid00002"]
    );
    assert_states(
        &fixture,
        &[
            ("agent001", "idle"),
            ("kid00001", "idle"),
            ("kid00002", "idle"),
        ],
    )
    .await;
    for id in ["agent001", "kid00001", "kid00002"] {
        assert!(!agent_dir(&fixture, id).join("stopped.json").exists());
    }
    enqueue(&fixture, 1, "kid00002").await;
    batch(&fixture.daemon.core.managed, "kid00002").await;
    assert_eq!(fixture.runner.runs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn resume_without_subtree_starts_only_the_agent() {
    let (fixture, _) = tree_fixture().await;
    stopped_tree(&fixture).await;
    for extra in [json!({}), json!({"subtree":false})] {
        let mut params = json!({"identity":"agent001"});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let answer = worker_call(&fixture, "agent_start", params).await.unwrap();
        assert_eq!(answer["identity"], "agent001");
        assert_eq!(answer["state"], "idle");
        assert!(answer.get("started").is_none());
        assert!(answer.get("skipped").is_none());
        assert_states(
            &fixture,
            &[
                ("agent001", "idle"),
                ("kid00001", "offline"),
                ("kid00002", "offline"),
            ],
        )
        .await;
        worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn resume_tree_reports_a_failed_child_and_starts_the_rest() {
    let (fixture, _) = tree_fixture().await;
    stopped_tree(&fixture).await;
    worker_call(&fixture, "agent_start", json!({"identity":"agent001"}))
        .await
        .unwrap();
    tier(&fixture, "agent001", Some(Tier::Owner)).await;
    child(&fixture, "agent001", "sibling").await.unwrap();
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    let folder = worker_of(&fixture.daemon.core.managed, "kid00001")
        .await
        .lock()
        .await
        .record
        .worktree
        .clone();
    std::fs::remove_dir_all(&folder).unwrap();
    let failure = worker_call(&fixture, "agent_start", json!({"identity":"kid00001"}))
        .await
        .unwrap_err();
    let answer = worker_call(
        &fixture,
        "agent_start",
        json!({"identity":"agent001","subtree":true}),
    )
    .await
    .unwrap();
    assert_eq!(answer["started"], json!(["agent001", "kid00003"]));
    assert_eq!(
        answer["skipped"],
        json!([
            {"identity":"kid00001", "reason":failure.message},
            {"identity":"kid00002", "reason":"parent not started"}
        ])
    );
    assert_states(
        &fixture,
        &[
            ("agent001", "idle"),
            ("kid00001", "offline"),
            ("kid00002", "offline"),
            ("kid00003", "idle"),
        ],
    )
    .await;
}

#[tokio::test]
async fn resume_tree_skips_running_members() {
    let (fixture, _) = tree_fixture().await;
    stopped_tree(&fixture).await;
    worker_call(&fixture, "agent_start", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    let answer = worker_call(
        &fixture,
        "agent_start",
        json!({"identity":"agent001","subtree":true}),
    )
    .await
    .unwrap();
    assert_eq!(answer["started"], json!(["agent001", "kid00002"]));
    assert_eq!(
        answer["skipped"],
        json!([{"identity":"kid00001", "reason":"already running"}])
    );
    assert_states(
        &fixture,
        &[
            ("agent001", "idle"),
            ("kid00001", "idle"),
            ("kid00002", "idle"),
        ],
    )
    .await;
}

#[tokio::test]
async fn resume_tree_starts_descendants_below_a_running_member() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    root(&fixture).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    tier(&fixture, "kid00001", Some(Tier::Owner)).await;
    child(&fixture, "kid00001", "k2").await.unwrap();
    tier(&fixture, "kid00002", Some(Tier::Owner)).await;
    child(&fixture, "kid00002", "k3").await.unwrap();
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    worker_call(&fixture, "agent_start", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    let answer = worker_call(
        &fixture,
        "agent_start",
        json!({"identity":"agent001","subtree":true}),
    )
    .await
    .unwrap();
    assert_eq!(
        answer["started"],
        json!(["agent001", "kid00002", "kid00003"])
    );
    assert_eq!(
        answer["skipped"],
        json!([{"identity":"kid00001", "reason":"already running"}])
    );
    assert_states(
        &fixture,
        &[
            ("agent001", "idle"),
            ("kid00001", "idle"),
            ("kid00002", "idle"),
            ("kid00003", "idle"),
        ],
    )
    .await;
}

#[tokio::test]
async fn resume_tree_root_failure_starts_no_descendants() {
    let (fixture, _) = tree_fixture().await;
    stopped_tree(&fixture).await;
    let folder = worker_of(&fixture.daemon.core.managed, "agent001")
        .await
        .lock()
        .await
        .record
        .worktree
        .clone();
    std::fs::remove_dir_all(folder).unwrap();
    let single = worker_call(&fixture, "agent_start", json!({"identity":"agent001"}))
        .await
        .unwrap_err();
    let tree = worker_call(
        &fixture,
        "agent_start",
        json!({"identity":"agent001","subtree":true}),
    )
    .await
    .unwrap_err();
    assert_eq!(tree, single);
    assert_states(
        &fixture,
        &[
            ("agent001", "offline"),
            ("kid00001", "offline"),
            ("kid00002", "offline"),
        ],
    )
    .await;
}

#[tokio::test]
async fn resume_tree_deadline_reports_every_unreached_member() {
    const DEADLINE_TEST_WAIT: std::time::Duration = std::time::Duration::from_secs(35);
    let (fixture, _) = tree_fixture().await;
    stopped_tree(&fixture).await;
    let child = worker_of(&fixture.daemon.core.managed, "kid00001").await;
    let held = child.lock().await;
    let answer = tokio::time::timeout(
        DEADLINE_TEST_WAIT,
        fixture
            .daemon
            .core
            .managed
            .start(json!({"identity":"agent001","subtree":true})),
    )
    .await
    .unwrap()
    .unwrap();
    drop(held);
    assert_eq!(answer["started"], json!(["agent001"]));
    assert_eq!(
        answer["skipped"],
        json!([
            {"identity":"kid00001", "reason":"deadline"},
            {"identity":"kid00002", "reason":"deadline"}
        ])
    );
    assert_states(
        &fixture,
        &[
            ("agent001", "idle"),
            ("kid00001", "offline"),
            ("kid00002", "offline"),
        ],
    )
    .await;
    assert!(fixture.daemon.core.managed.launching.try_lock().is_ok());
}

#[tokio::test]
async fn resume_parameters_keep_attach_and_remove_strict() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    for method in ["agent_attach", "agent_remove"] {
        let error = worker_call(
            &fixture,
            method,
            json!({"identity":"agent001","subtree":false}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert!(error.message.contains("unknown field"));
    }
    for extra in [json!({"subtree":"true"}), json!({"unknown":true})] {
        let mut params = json!({"identity":"agent001"});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            worker_call(&fixture, "agent_start", params)
                .await
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
    }
    let answer = worker_call(&fixture, "agent_start", json!({"identity":"agent001","project":"project1","root":fixture.dir.path().join("repo"),"subtree":true})).await.unwrap();
    assert_eq!(answer["started"], json!(["agent001"]));
    assert_eq!(answer["skipped"], json!([]));
}

#[tokio::test]
async fn resume_tree_skips_a_revoked_local_child_and_its_descendants() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    let project = worker_call(&fixture, "local_project_create", json!({"name":"resume"}))
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
    let parent = worker_call(&fixture, "agent_launch", json!({"project":project,"name":"worker","root":fixture.dir.path().join("repo"),"role":"chain"})).await.unwrap();
    let parent = parent["identity"].as_str().unwrap();
    tier(&fixture, parent, Some(Tier::Owner)).await;
    let first = child(&fixture, parent, "first").await.unwrap();
    let first = first["identity"].as_str().unwrap();
    let sibling = child(&fixture, parent, "sibling").await.unwrap();
    let sibling = sibling["identity"].as_str().unwrap();
    tier(&fixture, first, Some(Tier::Owner)).await;
    let grandchild = child(&fixture, first, "grandchild").await.unwrap();
    let grandchild = grandchild["identity"].as_str().unwrap();
    worker_call(&fixture, "agent_stop", json!({"identity":parent}))
        .await
        .unwrap();
    assert!(
        fixture
            .daemon
            .core
            .origins
            .local()
            .revoke(
                &project.as_str().unwrap().parse().unwrap(),
                &first.parse().unwrap()
            )
            .await
            .unwrap()
    );
    let failure = worker_call(&fixture, "agent_start", json!({"identity":first}))
        .await
        .unwrap_err();
    assert_eq!(
        failure.message,
        "local identity has been revoked; launch a new agent"
    );
    let answer = worker_call(
        &fixture,
        "agent_start",
        json!({"identity":parent,"subtree":true}),
    )
    .await
    .unwrap();
    assert_eq!(answer["started"], json!([parent, sibling]));
    assert_eq!(
        answer["skipped"],
        json!([
            {"identity":first,"reason":failure.message},
            {"identity":grandchild,"reason":"parent not started"}
        ])
    );
    assert_states(
        &fixture,
        &[
            (parent, "idle"),
            (first, "offline"),
            (sibling, "idle"),
            (grandchild, "offline"),
        ],
    )
    .await;
}

#[tokio::test]
async fn resume_tree_quit_cancels_a_stuck_start_within_the_managed_deadline() {
    let (mut fixture, _) = tree_fixture().await;
    let reached = Arc::new(tokio::sync::Notify::new());
    let held_reached = reached.clone();
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let held_armed = armed.clone();
    let senders = Arc::new(Mutex::new(Vec::new()));
    let captured = senders.clone();
    publisher(
        &mut fixture,
        Arc::new(move |sender, envelope| {
            let (reached, captured, armed) =
                (held_reached.clone(), captured.clone(), held_armed.clone());
            Box::pin(async move {
                if armed.load(Ordering::Acquire)
                    && matches!(envelope.payload, agmen_proto::Payload::Hello { .. })
                {
                    captured
                        .lock()
                        .unwrap()
                        .push(sender.identity_id.to_string());
                    if sender.identity_id.as_str() == "kid00001" {
                        reached.notify_one();
                        std::future::pending::<()>().await;
                    }
                }
                Ok(OutboxState::Pending { attempts: 0 })
            })
        }),
    )
    .await;
    stopped_tree(&fixture).await;
    armed.store(true, Ordering::Release);
    let managed = &fixture.daemon.core.managed;
    let (answer, ()) = tokio::time::timeout(RESUME_TEST_WAIT, async {
        tokio::join!(
            worker_call(
                &fixture,
                "agent_start",
                json!({"identity":"agent001","subtree":true})
            ),
            async {
                reached.notified().await;
                tokio::time::timeout(crate::managed::QUIT_WAIT, managed.shutdown())
                    .await
                    .unwrap();
            }
        )
    })
    .await
    .unwrap();
    let answer = answer.unwrap();
    assert_eq!(answer["started"], json!(["agent001"]));
    assert_eq!(
        answer["skipped"],
        json!([
            {"identity":"kid00001", "reason":"daemon is shutting down"},
            {"identity":"kid00002", "reason":"daemon is shutting down"}
        ])
    );
    assert_eq!(*senders.lock().unwrap(), ["agent001", "kid00001"]);
    assert_states(
        &fixture,
        &[
            ("agent001", "offline"),
            ("kid00001", "offline"),
            ("kid00002", "offline"),
        ],
    )
    .await;
    assert!(managed.launching.try_lock().is_ok());
}

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
