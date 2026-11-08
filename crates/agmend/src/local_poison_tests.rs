use super::*;

/// Drive the production entry point and fail if it ends or closes a watch.
async fn collecting(f: &Fixture, scenario: impl std::future::Future<Output = ()>) {
    let collection = Collection {
        hub: f.collection.hub.clone(),
        origins: f.collection.origins.clone(),
        store: f.collection.store.clone(),
        origin: f.collection.origin.clone(),
        owner: f.collection.owner.clone(),
        state_dir: f.collection.state_dir.clone(),
        project: f.collection.project.clone(),
        tx: f.collection.tx.clone(),
        timing: f.collection.timing,
    };
    let mut watch = collection.tx.subscribe();
    tokio::select! {
        () = collection.run(VecDeque::new()) => panic!("local collector ended"),
        () = async {
            tokio::pin!(scenario);
            loop {
                tokio::select! {
                    () = &mut scenario => break,
                    event = watch.recv() => {
                        assert!(event.unwrap().1.is_ok(), "watch received a terminal error");
                    }
                }
            }
        } => {}
    }
}

async fn delivered(f: &Fixture, identity: &IdentityId, message: &agmen_proto::MessageId) {
    tokio::time::timeout(TEST_WAIT, async {
        loop {
            // Owner ALERTs also reach agents under R31, independently of the
            // message whose exactly-once delivery this probe is checking.
            let rows: Vec<_> = f
                .drain(identity)
                .await
                .into_iter()
                .filter(|row| !matches!(row.client.payload, Payload::Alert { .. }))
                .collect();
            if !rows.is_empty() {
                assert_eq!(rows.len(), 1, "one delivery per message");
                assert_eq!(&rows[0].client.id, message);
                assert!(
                    f.drain(identity)
                        .await
                        .iter()
                        .all(|row| matches!(row.client.payload, Payload::Alert { .. })),
                    "no duplicate delivery"
                );
                return;
            }
            tokio::time::sleep(COLLECT_POLL).await;
        }
    })
    .await
    .expect("healthy recipient did not receive the message");
}

async fn connection(watch: &mut broadcast::Receiver<WatchEvent>, connected: bool) {
    tokio::time::timeout(TEST_WAIT, async {
        loop {
            if let ClientEvent::Connection { connected: actual } =
                watch.recv().await.unwrap().1.unwrap()
            {
                assert_eq!(actual, connected, "status changed before a healthy pass");
                return;
            }
        }
    })
    .await
    .expect("collector did not publish its connection status");
}

#[tokio::test]
async fn fu_poison_local_skips_unreadable_identity_and_alerts_once_across_wakes() {
    let (logs, _capture) = LogBytes::capture();
    let f = Fixture::new().await;
    let poison = &f.agents[0];
    f.execute_state(format!(
        "UPDATE identities SET metadata='{{' WHERE identity='{poison}'"
    ))
    .await;
    let first = f.post(Some(&f.agents[1])).await;
    collecting(&f, async {
        delivered(&f, &f.agents[1], &first.id).await;
        tokio::time::timeout(TEST_WAIT, async {
            while feed_alerts(&f).await == 0 {
                tokio::time::sleep(COLLECT_POLL).await;
            }
        })
        .await
        .unwrap();
        let second = f.post(Some(&f.agents[1])).await;
        delivered(&f, &f.agents[1], &second.id).await;
        tokio::time::sleep(SUPERVISE_PROBE).await;
        let alerts = feed_alert_texts(&f).await;
        assert_eq!(alerts.len(), 1, "{alerts:?}");
        assert!(alerts[0].contains(poison.as_str()));
        assert!(alerts[0].contains("dropped"));
        let path = f._dir.path().join("agmend.db");
        let poison = poison.clone();
        let pending: i64 = tokio::task::spawn_blocking(move || {
            rusqlite::Connection::open(path)
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM pending WHERE identity=?1",
                    [poison.as_str()],
                    |row| row.get(0),
                )
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(
            pending, 0,
            "the unreadable identity receives no pending rows"
        );
        assert_eq!(warns_about_broken(&logs).await.len(), 1);
        let readable = f
            .daemon
            .core
            .identities
            .readable(
                agmen_client::LOCAL_ORIGIN.into(),
                f.collection.owner.clone(),
            )
            .await
            .unwrap();
        assert_eq!(readable.len(), 1, "listing skips only the unreadable row");
    })
    .await;
    f.stop().await;
}

#[tokio::test]
async fn fu_poison_local_holds_one_message_and_delivers_on_other_streams() {
    let mut f = Fixture::new().await;
    f.collection.timing.max = Duration::from_millis(50);
    let poison = &f.agents[0];
    // Corrupt after the first page was listed, so the base reaches Held rather
    // than failing earlier while listing identities.
    f.post(None).await;
    let mut held = feed_message(2, "FYI", &[], &[]).client;
    held.mentions
        .push(agmen_proto::Mention::Identity(poison.clone()));
    f.collection
        .origins
        .local()
        .post_as_person(&f.collection.project, &held)
        .await
        .unwrap();
    f.execute_state(format!(
        "CREATE TRIGGER corrupt_after_feed AFTER INSERT ON positions
         WHEN NEW.stream='feed' AND NEW.seq=1
         BEGIN UPDATE identities SET metadata='{{}}' WHERE identity='{poison}'; END;"
    ))
    .await;
    collecting(&f, async {
        tokio::time::timeout(TEST_WAIT, async {
            while !feed_alert_texts(&f)
                .await
                .iter()
                .any(|text| text.contains("is held"))
            {
                tokio::time::sleep(COLLECT_POLL).await;
            }
        })
        .await
        .unwrap();
        let healthy = f.post(Some(&f.agents[1])).await;
        delivered(&f, &f.agents[1], &healthy.id).await;
        tokio::time::sleep(SUPERVISE_PROBE).await;
        let held_alerts: Vec<_> = feed_alert_texts(&f)
            .await
            .into_iter()
            .filter(|text| text.contains("is held"))
            .collect();
        assert_eq!(held_alerts.len(), 1, "{held_alerts:?}");
        assert!(held_alerts[0].contains("feed #2"));
        assert!(held_alerts[0].contains(poison.as_str()));
        assert_eq!(f.collection.positions().await.unwrap()[&Stream::Feed], 1);
        assert!(f.drain(poison).await.is_empty());
    })
    .await;
    f.stop().await;
}

#[tokio::test]
async fn fu_poison_queue_alert_worker_skips_unreadable_rows_for_the_same_owner() {
    // Exercise both a valid JSON document with invalid metadata, and invalid
    // JSON that SQLite's json_extract itself would reject.
    for metadata in ["{\"queue_full\":{\"alerts\":1}}", "{"] {
        let f = Fixture::new().await;
        let poison = &f.agents[0];
        let healthy = &f.agents[1];
        f.execute_state(format!(
            "UPDATE identities SET metadata='{metadata}' WHERE identity='{poison}';
             UPDATE identities SET metadata=json_set(metadata,'$.queue_full.alerts',1,'$.queue_full.pending',1)
             WHERE identity='{healthy}';"
        )).await;
        let worker = f.daemon.core.identities.queue_alert_worker();
        let probe = async {
            for count in 1..=2 {
                tokio::time::timeout(COLLECT_WAIT, async {
                    while feed_alerts(&f).await < count {
                        tokio::time::sleep(COLLECT_POLL).await;
                    }
                })
                .await
                .expect("healthy identity's full queue ALERT was blocked");
                if count == 1 {
                    tokio::time::sleep(SUPERVISE_PROBE).await;
                    f.execute_state(format!(
                        "UPDATE identities SET metadata=json_set(metadata,'$.queue_full.alerts',1)
                         WHERE identity='{healthy}';"
                    ))
                    .await;
                }
            }
            tokio::time::sleep(SUPERVISE_PROBE).await;
            let alerts = feed_alert_texts(&f).await;
            assert_eq!(alerts.len(), 2, "{alerts:?}");
            assert!(alerts.iter().all(|text| text.contains("queue is full")));
        };
        tokio::select! {
            () = worker => panic!("queue ALERT worker ended"),
            () = probe => {}
        }
        f.stop().await;
    }
}

#[tokio::test]
async fn fu_poison_local_permanent_store_failure_stays_offline_and_recovers() {
    let (logs, _capture) = LogBytes::capture();
    let mut f = Fixture::new().await;
    assert_eq!(f.collection.timing.max, Duration::from_secs(30));
    // The existing per-collection timing seam keeps this interval test short.
    f.collection.timing.initial = Duration::from_millis(5);
    f.collection.timing.max = Duration::from_millis(150);
    f.execute_state(
        "CREATE TRIGGER fail_local_position BEFORE INSERT ON positions
         BEGIN SELECT RAISE(ABORT,'injected permanent failure'); END;"
            .into(),
    )
    .await;
    let message = f.post(Some(&f.agents[1])).await;
    let mut watch = f.collection.tx.subscribe();
    collecting(&f, async {
        // No connected event until an entire pass has succeeded.
        connection(&mut watch, false).await;
        for _ in 0..2 {
            let failed_at = tokio::time::Instant::now();
            connection(&mut watch, false).await;
            assert!(failed_at.elapsed() >= f.collection.timing.max);
        }
        assert!(f.drain(&f.agents[1]).await.is_empty());
        f.execute_state("DROP TRIGGER fail_local_position".into())
            .await;
        connection(&mut watch, true).await;
        delivered(&f, &f.agents[1], &message.id).await;
        let text = logs.text();
        assert_eq!(
            text.lines()
                .filter(|line| line.contains("WARN") && line.contains("needs repair"))
                .count(),
            1,
            "{text}"
        );
        assert!(
            text.lines()
                .any(|line| line.contains("DEBUG") && line.contains("local ingest failed")),
            "{text}"
        );
    })
    .await;
    f.stop().await;
}

#[tokio::test]
async fn fu_poison_local_transient_store_failure_retries_and_delivers_once() {
    let mut f = Fixture::new().await;
    f.collection.timing.initial = Duration::from_millis(20);
    f.collection.timing.max = Duration::from_millis(150);
    let message = f.post(Some(&f.agents[1])).await;
    let path = f._dir.path().join("agmend.db");
    let locked = tokio::task::spawn_blocking(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        connection
    })
    .await
    .unwrap();
    let mut watch = f.collection.tx.subscribe();
    collecting(&f, async {
        tokio::time::timeout(TEST_WAIT, async {
            loop {
                if matches!(
                    watch.recv().await.unwrap().1.unwrap(),
                    ClientEvent::Connection { connected: false }
                ) {
                    break;
                }
            }
        })
        .await
        .expect("busy store did not produce offline status");
        tokio::task::spawn_blocking(move || {
            locked.execute_batch("ROLLBACK").unwrap();
        })
        .await
        .unwrap();
        connection(&mut watch, true).await;
        delivered(&f, &f.agents[1], &message.id).await;
    })
    .await;
    f.stop().await;
}

#[tokio::test]
async fn fu_poison_local_authority_distinguishes_unreadable_from_missing() {
    let f = Fixture::new().await;
    let poison = f.caller(&f.agents[0]).await;
    let healthy = f.caller(&f.agents[1]).await;
    f.execute_state(format!(
        "UPDATE identities SET metadata='{{' WHERE identity='{}'",
        poison.identity_id
    ))
    .await;
    assert!(
        check_managed(&f.collection.origins, f.collection.store.clone(), &healthy)
            .await
            .unwrap()
    );
    assert!(
        check_managed(&f.collection.origins, f.collection.store.clone(), &poison)
            .await
            .is_err()
    );
    let login = f.collection.origins.local().login().await.unwrap().unwrap();
    let identities = &f.daemon.core.identities;
    // The listing registration and the listener paths decide from still refuses
    // as a whole, so an unreadable row is never read as a missing one.
    assert!(
        identities
            .managed(
                agmen_client::LOCAL_ORIGIN.into(),
                f.collection.owner.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(
        identities
            .owned_by(&login, &healthy.identity_id)
            .await
            .unwrap(),
        Some(healthy)
    );
    assert!(
        identities
            .owned_by(&login, &poison.identity_id)
            .await
            .is_err()
    );
    assert!(
        identities
            .owned_by(&login, &"missing001".parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    f.stop().await;
}

#[tokio::test]
async fn fu_poison_local_initial_catch_up_store_failure_keeps_a_collector() {
    let f = Fixture::new().await;
    let message = f.post(Some(&f.agents[1])).await;
    let path = f._dir.path().join("agmend.db");
    let locked = tokio::task::spawn_blocking(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        connection
    })
    .await
    .unwrap();
    let identities = &f.daemon.core.identities;
    assert!(
        identities
            .project_channel(&f.collection.project)
            .await
            .is_err()
    );
    {
        let collectors = identities.collectors.lock().await;
        assert_eq!(
            collectors.len(),
            1,
            "a failed initial catch-up must still start the collector"
        );
        assert!(!collectors.values().next().unwrap().task.is_finished());
    }
    tokio::task::spawn_blocking(move || {
        locked.execute_batch("ROLLBACK").unwrap();
    })
    .await
    .unwrap();
    delivered(&f, &f.agents[1], &message.id).await;
    f.stop().await;
}

/// A Feed FYI that mentions only `identity`, recorded as the person.
async fn post_mention(f: &Fixture, identity: &IdentityId) {
    let mut message = feed_message(1, "FYI", &[], &[]).client;
    message
        .mentions
        .push(agmen_proto::Mention::Identity(identity.clone()));
    f.collection
        .origins
        .local()
        .post_as_person(&f.collection.project, &message)
        .await
        .unwrap();
}

async fn count_rows(f: &Fixture, table: &'static str, identity: &IdentityId) -> i64 {
    let path = f._dir.path().join("agmend.db");
    let identity = identity.clone();
    tokio::task::spawn_blocking(move || {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE identity=?1"),
                [identity.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn fu_poison_local_held_message_does_not_put_the_collector_on_its_retry_interval() {
    // The production timing: a held stream waits for a repair, and everything
    // else is delivered at the collector's own pace, not 30 s later.
    let f = Fixture::new().await;
    let poison = f.agents[0].clone();
    f.execute_state(format!(
        "UPDATE identities SET metadata='{{}}' WHERE identity='{poison}'"
    ))
    .await;
    post_mention(&f, &poison).await;
    collecting(&f, async {
        tokio::time::timeout(COLLECT_WAIT, async {
            while !feed_alert_texts(&f)
                .await
                .iter()
                .any(|text| text.contains("is held"))
            {
                tokio::time::sleep(COLLECT_POLL).await;
            }
        })
        .await
        .expect("the held message was not reported");
        let healthy = f.post(Some(&f.agents[1])).await;
        tokio::time::timeout(COLLECT_WAIT, delivered(&f, &f.agents[1], &healthy.id))
            .await
            .expect("a held message delayed delivery to a healthy recipient");
    })
    .await;
    f.stop().await;
}

#[tokio::test]
async fn fu_poison_local_held_message_does_not_fail_an_explicit_catch_up() {
    // A daemon that starts with a message still held has no collector yet: the
    // first pending call runs the catch-up itself, and must still answer.
    let f = Fixture::new().await;
    let caller = f.caller(&f.agents[1]).await;
    let poison = f.agents[0].clone();
    f.execute_state(format!(
        "UPDATE identities SET metadata='{{}}' WHERE identity='{poison}'"
    ))
    .await;
    post_mention(&f, &poison).await;
    let message = f.post(Some(&f.agents[1])).await;
    let rows = f
        .daemon
        .core
        .identities
        .pending(&caller)
        .await
        .expect("a held message failed the catch-up of an unrelated identity");
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], json!(message.id));
    f.stop().await;
}

#[tokio::test]
async fn fu_poison_register_over_an_unreadable_row_keeps_the_queue_it_did_not_add() {
    use std::os::unix::fs::PermissionsExt as _;

    let f = Fixture::new().await;
    let row = f.caller(&f.agents[0]).await;
    let poison = row.identity_id.clone();
    f.post(Some(&poison)).await;
    f.collection
        .local_wake(&mut VecDeque::new(), MAX_COLLECTION_PAGES)
        .await
        .unwrap();
    assert_eq!(count_rows(&f, "pending", &poison).await, 1);
    f.execute_state(format!(
        "UPDATE identities SET metadata='{{}}' WHERE identity='{poison}'"
    ))
    .await;
    // The credential store refuses the write that follows storing the row.
    let tokens = f._dir.path().join("tokens");
    std::fs::create_dir_all(&tokens).unwrap();
    std::fs::set_permissions(&tokens, std::fs::Permissions::from_mode(0o500)).unwrap();
    let registered = f.daemon.core.identities.register(row).await;
    std::fs::set_permissions(&tokens, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(registered.is_err());
    assert_eq!(
        count_rows(&f, "pending", &poison).await,
        1,
        "a failed register erased a queue it did not create"
    );
    assert_eq!(count_rows(&f, "identities", &poison).await, 1);
    f.stop().await;
}

#[tokio::test]
async fn fu_poison_stream_list_alert_is_posted_by_a_readable_identity() {
    let f = Fixture::new().await;
    // The unreadable row sorts first, so it is the first sender tried.
    let poison = f.agents[0].clone();
    f.execute_state(format!(
        "UPDATE identities SET metadata='{{' WHERE identity='{poison}'"
    ))
    .await;
    let (owner, project) = (f.collection.owner.clone(), f.collection.project.clone());
    store_work(f.collection.store.clone(), move |store| {
        store.mark_streams_full(agmen_client::LOCAL_ORIGIN, &owner, &project)
    })
    .await
    .unwrap();
    let worker = f.daemon.core.identities.queue_alert_worker();
    let probe = async {
        tokio::time::timeout(COLLECT_WAIT, async {
            loop {
                let page = f
                    .collection
                    .origins
                    .local()
                    .page(
                        &f.collection.project,
                        &Stream::Feed,
                        0,
                        COLLECTION_PAGE_SIZE,
                    )
                    .await
                    .unwrap();
                if let Some(alert) = page.messages.into_iter().find(|row| {
                    matches!(&row.client.payload, Payload::Alert { need, .. } if need.contains("stream list is full"))
                }) {
                    return alert.from;
                }
                tokio::time::sleep(COLLECT_POLL).await;
            }
        })
        .await
        .expect("an unreadable row blocked the stream list ALERT")
    };
    let sender = tokio::select! {
        () = worker => panic!("queue ALERT worker ended"),
        sender = probe => sender,
    };
    assert_eq!(sender, f.agents[1]);
    f.stop().await;
}
