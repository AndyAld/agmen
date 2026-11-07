use super::*;
use crate::budget::{Budget, Money};
use crate::managed::Worker;

async fn three_levels(fixture: &Fixture) {
    settings(fixture, "budgets = false");
    root(fixture).await;
    child(fixture, "agent001", "k1").await.unwrap();
    tier(fixture, "kid00001", Some(Tier::Owner)).await;
    child(fixture, "kid00001", "k2").await.unwrap();
}

async fn state(fixture: &Fixture, id: &str) -> &'static str {
    worker_of(&fixture.daemon.core.managed, id)
        .await
        .lock()
        .await
        .state
}

#[tokio::test]
async fn stop_stops_the_subtree_children_first() {
    let (fixture, _) = tree_fixture().await;
    three_levels(&fixture).await;
    let result = worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    assert_eq!(result["identity"], "agent001");
    assert_eq!(result["state"], "offline");
    assert_eq!(
        result["stopped"],
        json!(["kid00002", "kid00001", "agent001"])
    );
    for id in ["agent001", "kid00001", "kid00002"] {
        assert_eq!(state(&fixture, id).await, "offline");
    }
    let posts = fixture.posts.lock().unwrap();
    let byes: Vec<_> = posts
        .iter()
        .filter(|post| matches!(post.payload, agmen_proto::Payload::Bye {}))
        .collect();
    assert_eq!(byes.len(), 3);
    // Stop closes each sender after posting its BYE; the response is checked
    // against the sender order separately in stop_bye_senders_are_deepest_first.
}

#[tokio::test]
async fn stop_bye_senders_are_deepest_first() {
    let (mut fixture, _) = tree_fixture().await;
    let senders = Arc::new(Mutex::new(Vec::new()));
    let captured = senders.clone();
    publisher(
        &mut fixture,
        Arc::new(move |sender, envelope| {
            if matches!(envelope.payload, agmen_proto::Payload::Bye {}) {
                captured
                    .lock()
                    .unwrap()
                    .push(sender.identity_id.to_string());
            }
            Box::pin(async { Ok(OutboxState::Pending { attempts: 0 }) })
        }),
    )
    .await;
    three_levels(&fixture).await;
    worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap();
    assert_eq!(
        *senders.lock().unwrap(),
        ["kid00002", "kid00001", "agent001"]
    );
}

#[tokio::test]
async fn subtree_stop_has_one_ten_second_deadline() {
    let (mut fixture, _) = tree_fixture().await;
    publisher(
        &mut fixture,
        Arc::new(|_, envelope| {
            Box::pin(async move {
                if matches!(envelope.payload, agmen_proto::Payload::Bye {}) {
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                }
                Ok(OutboxState::Pending { attempts: 0 })
            })
        }),
    )
    .await;
    three_levels(&fixture).await;
    let began = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        fixture
            .daemon
            .core
            .managed
            .stop(json!({"identity":"agent001"})),
    )
    .await
    .unwrap();
    assert!(result.unwrap_err().message.contains("10 s"));
    assert!(began.elapsed() >= std::time::Duration::from_secs(9));
    assert_eq!(state(&fixture, "agent001").await, "idle");
}

#[tokio::test]
async fn stop_deadline_does_not_detach_a_batch_task() {
    let (mut fixture, _) = tree_fixture().await;
    publisher(
        &mut fixture,
        Arc::new(|_, envelope| {
            Box::pin(async move {
                if matches!(envelope.payload, agmen_proto::Payload::Bye {}) {
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                }
                Ok(OutboxState::Pending { attempts: 0 })
            })
        }),
    )
    .await;
    three_levels(&fixture).await;
    let (ready, started) = tokio::sync::oneshot::channel();
    let (ended, finished) = tokio::sync::oneshot::channel();
    struct Ended(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Ended {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _sent = sender.send(());
            }
        }
    }
    let task = tokio::spawn(async move {
        let _ended = Ended(Some(ended));
        ready.send(()).unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    });
    let cleanup = task.abort_handle();
    started.await.unwrap();
    worker_of(&fixture.daemon.core.managed, "kid00001")
        .await
        .lock()
        .await
        .task = Some(task);
    let result = fixture
        .daemon
        .core
        .managed
        .stop(json!({"identity":"agent001"}))
        .await;
    let ended = tokio::time::timeout(std::time::Duration::from_secs(5), finished).await;
    cleanup.abort();
    assert!(result.unwrap_err().message.contains("10 s"));
    assert!(ended.is_ok(), "the aborted stop detached the batch task");
}

#[tokio::test]
async fn siblings_stop_concurrently_before_the_parent() {
    let (mut fixture, _) = tree_fixture().await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    publisher(
        &mut fixture,
        Arc::new(move |sender, envelope| {
            let barrier = barrier.clone();
            Box::pin(async move {
                if matches!(envelope.payload, agmen_proto::Payload::Bye {})
                    && sender.identity_id.as_str() != "agent001"
                {
                    barrier.wait().await;
                }
                Ok(OutboxState::Pending { attempts: 0 })
            })
        }),
    )
    .await;
    settings(&fixture, "budgets = false");
    root(&fixture).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    child(&fixture, "agent001", "k2").await.unwrap();
    let answer = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        fixture
            .daemon
            .core
            .managed
            .stop(json!({"identity":"agent001"})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        answer["stopped"],
        json!(["kid00001", "kid00002", "agent001"])
    );
}

#[tokio::test]
async fn stop_without_subtree_keeps_children_running() {
    let (fixture, _) = tree_fixture().await;
    three_levels(&fixture).await;
    let answer = worker_call(
        &fixture,
        "agent_stop",
        json!({"identity":"agent001","subtree":false}),
    )
    .await
    .unwrap();
    assert_eq!(answer["stopped"], json!(["agent001"]));
    assert_eq!(state(&fixture, "agent001").await, "offline");
    for id in ["kid00001", "kid00002"] {
        assert_eq!(state(&fixture, id).await, "idle");
    }
}

#[tokio::test]
async fn child_may_stop_only_its_descendants() {
    let (fixture, _) = tree_fixture().await;
    three_levels(&fixture).await;
    child(&fixture, "agent001", "sibling").await.unwrap();
    for target in ["agent001", "kid00001", "kid00003"] {
        let error = child_call(
            &fixture,
            "kid00001",
            "child_stop",
            json!({"identity":target}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Forbidden, "{target}");
    }
    let answer = child_call(
        &fixture,
        "agent001",
        "child_stop",
        json!({"identity":"kid00001"}),
    )
    .await
    .unwrap();
    assert_eq!(answer["stopped"], json!(["kid00002", "kid00001"]));
    assert_eq!(state(&fixture, "agent001").await, "idle");
    assert_eq!(state(&fixture, "kid00003").await, "idle");
}

#[tokio::test]
async fn parent_cycle_on_disk_stops_without_looping() {
    let (fixture, _) = tree_fixture().await;
    three_levels(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    for (id, parent) in [("agent001", "kid00001"), ("kid00001", "agent001")] {
        let file = managed.agent_dir(&id.parse().unwrap()).join("agent.json");
        let mut saved: Value = serde_json::from_str(&role_file(&file).unwrap()).unwrap();
        saved["parent"] = json!(parent);
        std::fs::write(file, serde_json::to_vec(&saved).unwrap()).unwrap();
    }
    let restored = restarted(&fixture).await;
    let answer = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        restored.stop(json!({"identity":"agent001"})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        answer["stopped"],
        json!(["kid00002", "kid00001", "agent001"])
    );
}

#[tokio::test]
async fn remove_with_live_descendants_requires_subtree_stop() {
    let (fixture, _) = tree_fixture().await;
    three_levels(&fixture).await;
    worker_call(
        &fixture,
        "agent_stop",
        json!({"identity":"kid00001","subtree":false}),
    )
    .await
    .unwrap();
    for target in ["agent001", "kid00001"] {
        let error = worker_call(&fixture, "agent_remove", json!({"identity":target}))
            .await
            .unwrap_err();
        assert!(error.message.contains("stop the subtree first"));
    }
    assert!(fixture.revoked.lock().unwrap().is_empty());
    worker_call(&fixture, "agent_stop", json!({"identity":"kid00001"}))
        .await
        .unwrap();
    worker_call(&fixture, "agent_remove", json!({"identity":"kid00001"}))
        .await
        .unwrap();
}

/// Holds the goodbye of `held` until `release` fires, and says when it arrives.
fn hold_goodbye(
    held: &'static str,
    reached: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
) -> TestPublisher {
    Arc::new(move |sender, envelope| {
        let (reached, release) = (reached.clone(), release.clone());
        Box::pin(async move {
            if matches!(envelope.payload, agmen_proto::Payload::Bye {})
                && sender.identity_id.as_str() == held
            {
                reached.notify_one();
                release.notified().await;
            }
            Ok(OutboxState::Pending { attempts: 0 })
        })
    })
}

/// Waits for the held goodbye, then lets `launcher` (still running in the owner
/// tier) ask for a child and gives the call two seconds. Only the launching
/// lock keeps that child from being born under a parent that is about to
/// stop, so the call must still be waiting when the time is up.
async fn launch_during_stop(
    fixture: &Fixture,
    launcher: &str,
    (reached, release): (&tokio::sync::Notify, &tokio::sync::Notify),
) -> (bool, Result<Value, ApiError>) {
    reached.notified().await;
    let finished = std::sync::atomic::AtomicBool::new(false);
    let call = async {
        let result = child(fixture, launcher, "late").await;
        finished.store(true, Ordering::SeqCst);
        result
    };
    let hold = async {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let early = finished.load(Ordering::SeqCst);
        release.notify_one();
        early
    };
    let (result, early) = tokio::join!(call, hold);
    (early, result)
}

#[tokio::test]
async fn stop_and_launch_do_not_interleave() {
    let (mut fixture, _) = tree_fixture().await;
    let (reached, release) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    publisher(
        &mut fixture,
        hold_goodbye("kid00001", reached.clone(), release.clone()),
    )
    .await;
    settings(&fixture, "budgets = false");
    root(&fixture).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    // kid00001 stops first and its goodbye is held, so agent001 still runs.
    let stop = fixture
        .daemon
        .core
        .managed
        .stop(json!({"identity":"agent001"}));
    let ((early, launched), stopped) = tokio::join!(
        launch_during_stop(&fixture, "agent001", (&reached, &release)),
        stop
    );
    assert!(!early, "a child was launched while its parent was stopping");
    assert_eq!(launched.unwrap_err().code, ErrorCode::Forbidden);
    stopped.unwrap();
    for worker in fixture.daemon.core.managed.workers.lock().await.values() {
        assert_eq!(worker.lock().await.state, "offline");
    }
}

#[tokio::test]
async fn a_child_cannot_be_born_while_its_parent_stops_a_subtree() {
    let (mut fixture, _) = tree_fixture().await;
    let (reached, release) = (
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(tokio::sync::Notify::new()),
    );
    publisher(
        &mut fixture,
        hold_goodbye("kid00002", reached.clone(), release.clone()),
    )
    .await;
    three_levels(&fixture).await;
    // agent001 stops kid00001's subtree; kid00002 stops first and is held,
    // while kid00001 still runs and asks for a child.
    let stop = child_call(
        &fixture,
        "agent001",
        "child_stop",
        json!({"identity":"kid00001"}),
    );
    let ((early, launched), stopped) = tokio::join!(
        launch_during_stop(&fixture, "kid00001", (&reached, &release)),
        stop
    );
    assert!(!early, "a child was launched while its parent was stopping");
    assert_eq!(launched.unwrap_err().code, ErrorCode::Forbidden);
    assert_eq!(stopped.unwrap()["stopped"], json!(["kid00002", "kid00001"]));
    for id in ["kid00001", "kid00002"] {
        assert_eq!(state(&fixture, id).await, "offline");
    }
    assert_eq!(state(&fixture, "agent001").await, "idle");
    assert_eq!(fixture.daemon.core.managed.workers.lock().await.len(), 3);
}

/// An agent of another person, running beside ours. `parent` links it into
/// the tree as a corrupt record could.
async fn stranger(fixture: &Fixture, parent: Option<&str>) {
    let managed = &fixture.daemon.core.managed;
    let mut record = worker_of(managed, "agent001")
        .await
        .lock()
        .await
        .record
        .clone();
    record.identity.identity_id = "other001".parse().unwrap();
    record.identity.owner_id = "owner002".parse().unwrap();
    record.parent = parent.map(|parent| parent.parse().unwrap());
    record.shared_budget = false;
    let mut worker = Worker::offline(record, Budget::new(Money::from_usd("5").unwrap()));
    worker.state = "idle";
    let id: agmen_proto::IdentityId = "other001".parse().unwrap();
    let mut tree = managed.tree.lock().await;
    let mut workers = managed.workers.lock().await;
    tree.insert(id.clone(), parent.map(|parent| parent.parse().unwrap()));
    workers.insert(id, Arc::new(tokio::sync::Mutex::new(worker)));
}

fn goodbyes(fixture: &Fixture) -> usize {
    fixture
        .posts
        .lock()
        .unwrap()
        .iter()
        .filter(|post| matches!(post.payload, agmen_proto::Payload::Bye {}))
        .count()
}

#[tokio::test]
async fn child_stop_refuses_other_peoples_agents_and_unknown_ids_alike() {
    let (fixture, _) = tree_fixture().await;
    three_levels(&fixture).await;
    stranger(&fixture, None).await;
    let mut refusals = Vec::new();
    for target in ["other001", "nobody01"] {
        for caller in ["agent001", "kid00001"] {
            let error = child_call(&fixture, caller, "child_stop", json!({"identity":target}))
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::Forbidden,
                "{caller} stopping {target}"
            );
            refusals.push(error.message);
        }
    }
    // Nothing tells a stranger's agent from an id that does not exist.
    assert!(refusals.windows(2).all(|pair| pair[0] == pair[1]));
    for id in ["agent001", "kid00001", "kid00002", "other001"] {
        assert_eq!(state(&fixture, id).await, "idle", "{id}");
    }
    assert_eq!(goodbyes(&fixture), 0);
}

#[tokio::test]
async fn an_agent_of_another_person_in_the_tree_stops_nothing() {
    let (fixture, _) = tree_fixture().await;
    three_levels(&fixture).await;
    // A corrupt record puts it below kid00001.
    stranger(&fixture, Some("kid00001")).await;
    let error = worker_call(&fixture, "agent_stop", json!({"identity":"agent001"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Forbidden);
    let error = child_call(
        &fixture,
        "agent001",
        "child_stop",
        json!({"identity":"kid00001"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Forbidden);
    for id in ["agent001", "kid00001", "kid00002", "other001"] {
        assert_eq!(state(&fixture, id).await, "idle", "{id}");
    }
    assert_eq!(goodbyes(&fixture), 0);
}

#[tokio::test]
async fn child_stop_without_subtree_leaves_the_targets_children_running() {
    let (fixture, _) = tree_fixture().await;
    three_levels(&fixture).await;
    let answer = child_call(
        &fixture,
        "agent001",
        "child_stop",
        json!({"identity":"kid00001","subtree":false}),
    )
    .await
    .unwrap();
    assert_eq!(answer["stopped"], json!(["kid00001"]));
    assert_eq!(state(&fixture, "kid00001").await, "offline");
    assert_eq!(state(&fixture, "kid00002").await, "idle");
}

#[tokio::test]
async fn child_stop_from_an_others_tier_batch_stops_nothing() {
    let (fixture, _) = tree_fixture().await;
    three_levels(&fixture).await;
    for batch_tier in [Some(Tier::Others), None] {
        tier(&fixture, "agent001", batch_tier).await;
        let error = child_call(
            &fixture,
            "agent001",
            "child_stop",
            json!({"identity":"kid00001"}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Forbidden);
        assert_eq!(error.message, TIER_REFUSAL);
    }
    for id in ["agent001", "kid00001", "kid00002"] {
        assert_eq!(state(&fixture, id).await, "idle", "{id}");
    }
    assert_eq!(goodbyes(&fixture), 0);
}

pub(super) type TestPublisher = Arc<
    dyn Fn(
            crate::state::ManagedIdentity,
            agmen_proto::ClientEnvelope,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<OutboxState, ApiError>> + Send>,
        > + Send
        + Sync,
>;

pub(super) async fn publisher(fixture: &mut Fixture, publisher: TestPublisher) {
    Arc::get_mut(&mut Arc::get_mut(&mut fixture.daemon.core).unwrap().managed)
        .unwrap()
        .identities
        .take();
    Arc::get_mut(&mut Arc::get_mut(&mut fixture.daemon.core).unwrap().identities)
        .unwrap()
        .publisher = Some(publisher);
    fixture
        .daemon
        .core
        .managed
        .attach(&fixture.daemon.core.identities)
        .await;
}
