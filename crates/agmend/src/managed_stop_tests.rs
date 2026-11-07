use super::*;
use crate::budget::{Budget, Money};
use crate::managed::Worker;

const QUIT_TEST_WAIT: std::time::Duration = std::time::Duration::from_secs(9);

#[tokio::test]
async fn quit_says_goodbye_deepest_first() {
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
    fixture.daemon.core.managed.shutdown().await;
    assert_eq!(
        *senders.lock().unwrap(),
        ["kid00002", "kid00001", "agent001"]
    );
}

#[tokio::test]
async fn quit_waits_for_an_inflight_launch_then_stops_it() {
    let (mut fixture, _) = tree_fixture().await;
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let senders = Arc::new(Mutex::new(Vec::new()));
    let (held_reached, held_release, captured) =
        (reached.clone(), release.clone(), senders.clone());
    publisher(
        &mut fixture,
        Arc::new(move |sender, envelope| {
            let (reached, release, captured) =
                (held_reached.clone(), held_release.clone(), captured.clone());
            Box::pin(async move {
                if matches!(envelope.payload, agmen_proto::Payload::Hello { .. })
                    && sender.identity_id.as_str() == "kid00001"
                {
                    // HELLO precedes registry insertion while launching is held.
                    reached.notify_one();
                    tokio::time::timeout(QUIT_TEST_WAIT, release.notified())
                        .await
                        .unwrap();
                }
                if matches!(envelope.payload, agmen_proto::Payload::Bye {}) {
                    captured
                        .lock()
                        .unwrap()
                        .push(sender.identity_id.to_string());
                }
                Ok(OutboxState::Pending { attempts: 0 })
            })
        }),
    )
    .await;
    settings(&fixture, "budgets = false");
    root(&fixture).await;
    tier(&fixture, "agent001", Some(Tier::Owner)).await;
    let shutdown = async {
        tokio::time::timeout(QUIT_TEST_WAIT, reached.notified())
            .await
            .unwrap();
        tokio::join!(fixture.daemon.core.managed.shutdown(), async {
            release.notify_one();
        });
    };
    let (launched, ()) = tokio::time::timeout(QUIT_TEST_WAIT, async {
        tokio::join!(child(&fixture, "agent001", "late"), shutdown)
    })
    .await
    .unwrap();
    let launched = launched.unwrap();
    for worker in fixture.daemon.core.managed.workers.lock().await.values() {
        assert_eq!(worker.lock().await.state, "offline");
    }
    assert!(
        senders
            .lock()
            .unwrap()
            .iter()
            .any(|id| Some(id.as_str()) == launched["identity"].as_str())
    );
}

#[tokio::test]
async fn quit_discards_a_launch_that_never_finishes() {
    let (mut fixture, _) = tree_fixture().await;
    publisher(
        &mut fixture,
        Arc::new(|sender, envelope| {
            Box::pin(async move {
                if matches!(envelope.payload, agmen_proto::Payload::Hello { .. })
                    && sender.identity_id.as_str() == "kid00001"
                {
                    std::future::pending::<()>().await;
                }
                Ok(OutboxState::Pending { attempts: 0 })
            })
        }),
    )
    .await;
    Arc::get_mut(&mut Arc::get_mut(&mut fixture.daemon.core).unwrap().managed)
        .unwrap()
        .quit_launch_wait = std::time::Duration::from_millis(300);
    settings(&fixture, "budgets = false");
    root(&fixture).await;
    tier(&fixture, "agent001", Some(Tier::Owner)).await;
    let before = git_state(&fixture);
    let (launched, ()) = tokio::time::timeout(QUIT_TEST_WAIT, async {
        tokio::join!(child(&fixture, "agent001", "late"), async {
            // Let the launch pass admission and reach its HELLO.
            while fixture.revoked.lock().unwrap().is_empty()
                && fixture.daemon.core.managed.launching.try_lock().is_ok()
            {
                tokio::task::yield_now().await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            fixture.daemon.core.managed.shutdown().await;
        })
    })
    .await
    .unwrap();
    assert!(launched.unwrap_err().message.contains("shutting down"));
    assert_discarded(&fixture, "kid00001", &before).await;
}

#[tokio::test]
async fn quit_refuses_a_launch_started_after_shutdown_begins() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    let launching = managed.launching.lock().await;
    let ((), launched) = tokio::time::timeout(QUIT_TEST_WAIT, async {
        tokio::join!(managed.shutdown(), async {
            assert!(managed.shutting_down.load(Ordering::Acquire));
            drop(launching);
            managed
                .launch(json!({"project":"project1","name":"late","root":fixture.dir.path().join("repo")}))
                .await
        })
    })
    .await
    .unwrap();
    assert!(
        launched
            .unwrap_err()
            .message
            .contains("daemon is shutting down")
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(managed.workers.lock().await.len(), 1);
}

#[tokio::test]
async fn quit_with_a_stuck_child_still_stops_the_parent() {
    let (mut fixture, _) = tree_fixture().await;
    let reached = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let held = hold_goodbye("kid00002", reached.clone(), release);
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
            held(sender, envelope)
        }),
    )
    .await;
    three_levels(&fixture).await;
    tokio::time::timeout(QUIT_TEST_WAIT, fixture.daemon.core.managed.shutdown())
        .await
        .unwrap();
    tokio::time::timeout(QUIT_TEST_WAIT, reached.notified())
        .await
        .unwrap();
    assert!(senders.lock().unwrap().iter().any(|id| id == "agent001"));
    for worker in fixture.daemon.core.managed.workers.lock().await.values() {
        assert_eq!(worker.lock().await.state, "offline");
    }
}

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

const SERVE_BOUND: std::time::Duration = std::time::Duration::from_secs(5);
/// The ordered 10 s, the 3 s renewal floor and the 6 s hub stop.
const STUCK_QUIT_BOUND: std::time::Duration = std::time::Duration::from_secs(19);

/// Moves the fixture's daemon out to serve on its socket until `stop`. The
/// fixture keeps the same shared state, behind a spare daemon of its own.
fn serve(
    fixture: &mut Fixture,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let spare = fixture.dir.path().join("spare");
    std::fs::create_dir(&spare).unwrap();
    let spare = crate::Agmend::bind(crate::Config::new(
        spare.join("agmend.sock"),
        spare.join("login.json"),
        fixture.tokens.clone(),
    ))
    .unwrap();
    let core = fixture.daemon.core.clone();
    let serving = std::mem::replace(&mut fixture.daemon, spare);
    fixture.daemon.core = core;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(serving.serve_until(async {
        let _stopped = stopped.await;
    }));
    (stop, task)
}

async fn connect(fixture: &Fixture) -> tokio::net::UnixStream {
    use agmen_local::{Outcome, Request, Response, frame};
    let mut stream = tokio::net::UnixStream::connect(fixture.dir.path().join("agmend.sock"))
        .await
        .unwrap();
    frame::write_async(&mut stream, &Request::new(1, "hello", json!({"api":1})))
        .await
        .unwrap();
    let hello: Response = frame::read_async(&mut stream).await.unwrap();
    assert!(matches!(hello.outcome, Outcome::Ok { .. }));
    stream
}

/// Quit used to close the connections first, which dropped the launch
/// before it could discard the identity it had minted.
#[tokio::test]
async fn quit_over_a_connection_discards_a_launch_that_never_finishes() {
    let (mut fixture, _) = tree_fixture().await;
    let reached = Arc::new(tokio::sync::Notify::new());
    let held = reached.clone();
    publisher(
        &mut fixture,
        Arc::new(move |sender, envelope| {
            let reached = held.clone();
            Box::pin(async move {
                if matches!(envelope.payload, agmen_proto::Payload::Hello { .. })
                    && sender.identity_id.as_str() == "agent001"
                {
                    reached.notify_one();
                    std::future::pending::<()>().await;
                }
                Ok(OutboxState::Pending { attempts: 0 })
            })
        }),
    )
    .await;
    Arc::get_mut(&mut Arc::get_mut(&mut fixture.daemon.core).unwrap().managed)
        .unwrap()
        .quit_launch_wait = std::time::Duration::from_millis(300);
    let before = git_state(&fixture);
    let (stop, serving) = serve(&mut fixture);
    let mut stream = connect(&fixture).await;
    let launch = agmen_local::Request::new(
        2,
        "agent_launch",
        json!({"project":"project1","name":"worker","root":fixture.dir.path().join("repo")}),
    );
    agmen_local::frame::write_async(&mut stream, &launch)
        .await
        .unwrap();
    tokio::time::timeout(QUIT_TEST_WAIT, reached.notified())
        .await
        .expect("the launch must reach its HELLO");
    stop.send(()).unwrap();
    tokio::time::timeout(SERVE_BOUND, serving)
        .await
        .expect("quit with a launch in flight exceeded its bound")
        .unwrap();
    assert_discarded_but_the_row(&fixture, "agent001", &before, 0).await;
    let registry = rusqlite::Connection::open(fixture.dir.path().join("agmend.db")).unwrap();
    let rows: i64 = registry
        .query_row(
            "SELECT count(*) FROM identities WHERE identity = 'agent001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0, "store row");
}

/// A launch that is stuck before its post-mint select (here: holding
/// admission for good) cannot be abandoned, and used to run the quit clock
/// out so that no worker got its goodbye.
#[tokio::test]
async fn quit_with_a_launch_stuck_before_its_mint_still_says_goodbye() {
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
    settings(&fixture, "budgets = false");
    root(&fixture).await;
    let managed = fixture.daemon.core.managed.clone();
    let (stop, serving) = serve(&mut fixture);
    // Startup takes admission too; hold it only once the daemon accepts.
    let _stream = connect(&fixture).await;
    let _stuck = managed.launching.lock().await;
    stop.send(()).unwrap();
    tokio::time::timeout(STUCK_QUIT_BOUND, serving)
        .await
        .expect("quit with a stuck launch exceeded its bound")
        .unwrap();
    assert_eq!(*senders.lock().unwrap(), ["agent001"]);
    assert_eq!(state(&fixture, "agent001").await, "offline");
}

/// The supervisor kept ticking while quit waited on a launch, and could start
/// a paid batch seconds after the owner quit.
#[tokio::test]
async fn quit_stops_the_supervisor_from_starting_a_batch() {
    let fixture = Fixture::new().await;
    fixture.runner.mode.store(1, Ordering::SeqCst);
    fixture.launch().await;
    enqueue(&fixture, 1, "agent001").await;
    let managed = &fixture.daemon.core.managed;
    let launching = managed.launching.lock().await;
    tokio::join!(managed.shutdown(), async {
        while !managed.shutting_down.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        managed.tick().await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(fixture.runner.runs.lock().unwrap().is_empty());
        drop(launching);
    });
}
