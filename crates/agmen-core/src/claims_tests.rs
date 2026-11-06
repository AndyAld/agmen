use std::time::Duration;

use agmen_proto::{
    ClientEnvelope, DenyReason, IdentityId, IdentityKind, ItemKey, Payload, ProjectId, Stream,
    Timestamp, Topics,
};

use crate::{
    AckCopy, Draft, LedgerError, LedgerMessage, MemStore, NewItem, Reads, ResolvedLane, Store,
    StoreTx, apply_ledger, envelope_hash, new_message_id, write,
};

pub(crate) const LEASE: Duration = Duration::from_secs(60);

pub(crate) async fn setup() -> (MemStore, Topics) {
    let store = MemStore::default();
    let topics = Topics::new("tenant01".parse().unwrap(), "project01".parse().unwrap());
    for (id, handle, kind) in [
        ("aaaaaaaa", "a", IdentityKind::Agent),
        ("bbbbbbbb", "b", IdentityKind::Agent),
        ("oooooooo", "o", IdentityKind::Agent),
        ("cccccccc", "c", IdentityKind::Coordinator),
    ] {
        store
            .seed_live_identity(topics.project(), &id.parse().unwrap(), handle, kind)
            .await
            .unwrap();
    }
    (store, topics)
}

pub(crate) fn message(from: &str, handle: &str, payload: Payload) -> LedgerMessage {
    let now = Timestamp::now();
    LedgerMessage {
        env: ClientEnvelope {
            v: 1,
            id: new_message_id(now).unwrap(),
            reply_to: None,
            to: Vec::new(),
            mentions: Vec::new(),
            items: Vec::new(),
            lane: None,
            body: "ledger test".try_into().unwrap(),
            valid_until: Some(now + LEASE),
            sent_at: now,
            payload,
        },
        from: from.parse().unwrap(),
        from_handle: handle.into(),
        from_kind: IdentityKind::Agent,
        from_owner: None,
        hub_at: now,
    }
}

pub(crate) async fn item(store: &MemStore, topics: &Topics, owned: bool) {
    let mut tx = store.begin().await.unwrap();
    tx.replace_lanes(
        topics.project(),
        &[ResolvedLane {
            name: "L-UI".parse().unwrap(),
            globs: vec!["ui/**".into()],
            owner: owned.then(|| "oooooooo".parse().unwrap()),
        }],
    )
    .await
    .unwrap();
    tx.insert_item(&NewItem {
        project: topics.project(),
        key: &"B-1".parse().unwrap(),
        title: "bug",
        lane: Some(&"L-UI".parse().unwrap()),
        severity: None,
        created_by: &"aaaaaaaa".parse().unwrap(),
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

pub(crate) async fn apply(store: &MemStore, topics: &Topics, msg: &LedgerMessage) {
    let mut tx = store.begin().await.unwrap();
    apply_ledger(&mut tx, topics, msg, LEASE).await.unwrap();
    tx.commit().await.unwrap();
}

pub(crate) fn claim(from: &str, handle: &str) -> LedgerMessage {
    message(
        from,
        handle,
        Payload::Claim {
            item: "B-1".parse().unwrap(),
        },
    )
}

#[tokio::test]
async fn claim_grants_then_denies_held_by() {
    let (store, topics) = setup().await;
    item(&store, &topics, false).await;
    let now = Timestamp::from_millisecond(Timestamp::now().as_millisecond()).unwrap();
    store.set_now(now);
    apply(&store, &topics, &claim("aaaaaaaa", "a")).await;
    apply(&store, &topics, &claim("bbbbbbbb", "b")).await;
    store.set_now(now + Duration::from_secs(1));
    apply(&store, &topics, &claim("aaaaaaaa", "a")).await;
    let rows = store.recorded_messages().await.unwrap();
    let feed: Vec<_> = rows
        .iter()
        .filter(|row| row.stream == Stream::Feed)
        .collect();
    assert_eq!(feed.len(), 3);
    assert_eq!(
        feed[0].client.payload,
        Payload::Grant {
            item: "B-1".parse().unwrap(),
            lease_until: now + LEASE,
        }
    );
    assert_eq!(
        feed[1].client.payload,
        Payload::Deny {
            item: "B-1".parse().unwrap(),
            reason: DenyReason::HeldBy,
        }
    );
    assert_eq!(feed[1].client.body.as_str(), "B-1 denied to b: held by a");
    assert_eq!(
        feed[2].client.payload,
        Payload::Grant {
            item: "B-1".parse().unwrap(),
            lease_until: now + Duration::from_secs(1) + LEASE,
        }
    );
    assert_eq!(rows.len(), 6);
    for pair in rows.as_chunks::<2>().0 {
        assert_eq!(pair[0].client.payload, pair[1].client.payload);
        assert!(matches!(pair[1].stream, Stream::Inbox(_)));
    }
}

#[tokio::test]
async fn claim_in_owned_lane_needs_owner_coordinator_or_assignee() {
    let (store, topics) = setup().await;
    item(&store, &topics, true).await;
    apply(&store, &topics, &claim("aaaaaaaa", "a")).await;
    let mut assigned = message(
        "cccccccc",
        "c",
        Payload::Assign {
            item: "B-1".parse().unwrap(),
            to: "aaaaaaaa".parse().unwrap(),
        },
    );
    assigned.from_kind = IdentityKind::Coordinator;
    apply(&store, &topics, &assigned).await;
    apply(&store, &topics, &claim("aaaaaaaa", "a")).await;
    let rows = store.recorded_messages().await.unwrap();
    assert!(matches!(
        rows[0].client.payload,
        Payload::Deny {
            reason: DenyReason::NotInLane,
            ..
        }
    ));
    assert!(matches!(rows[2].client.payload, Payload::Grant { .. }));
    for (id, handle, kind) in [
        ("oooooooo", "o", IdentityKind::Agent),
        ("cccccccc", "c", IdentityKind::Coordinator),
    ] {
        let (store, topics) = setup().await;
        item(&store, &topics, true).await;
        let mut msg = claim(id, handle);
        msg.from_kind = kind;
        apply(&store, &topics, &msg).await;
        assert!(matches!(
            store.recorded_messages().await.unwrap()[0].client.payload,
            Payload::Grant { .. }
        ));
    }
}

#[tokio::test]
async fn lapsed_claim_is_released_before_granting() {
    let (store, topics) = setup().await;
    item(&store, &topics, false).await;
    let now = Timestamp::now();
    store.set_now(now);
    apply(&store, &topics, &claim("aaaaaaaa", "a")).await;
    store.set_now(now + LEASE);
    apply(&store, &topics, &claim("bbbbbbbb", "b")).await;
    let rows = store.recorded_messages().await.unwrap();
    let feed: Vec<_> = rows
        .iter()
        .filter(|row| row.stream == Stream::Feed)
        .collect();
    assert_eq!(feed.len(), 3);
    assert_eq!(
        feed[1].client.payload,
        Payload::Release {
            item: "B-1".parse().unwrap(),
            reason: Some("lease_expired".into()),
        }
    );
    assert_eq!(feed[1].mentioned, vec!["aaaaaaaa".parse().unwrap()]);
    assert_eq!(
        feed[1].client.body.as_str(),
        "B-1 released: a's lease ran out"
    );
    assert!(matches!(feed[2].client.payload, Payload::Grant { .. }));
    assert!(feed[1].seq < feed[2].seq);
}

#[tokio::test]
async fn ledger_failure_records_nothing() {
    let (store, topics) = setup().await;
    item(&store, &topics, false).await;
    let msg = claim("aaaaaaaa", "a");
    store.fail_next("insert_claim");
    let mut tx = store.begin().await.unwrap();
    tx.lock_head(&format!("{}/feed", topics.project()))
        .await
        .unwrap();
    write(
        &mut tx,
        Draft {
            topics: &topics,
            client: msg.env.clone(),
            from: msg.from.clone(),
            from_handle: msg.from_handle.clone(),
            from_kind: msg.from_kind,
            mentioned: Vec::new(),
            refs: Vec::new(),
            streams: [Stream::Feed].into_iter().collect(),
            hash: envelope_hash(&msg.env).unwrap(),
            hub_at: msg.hub_at,
            ack: AckCopy::Queue,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        apply_ledger(&mut tx, &topics, &msg, LEASE).await,
        Err(LedgerError::Store(_))
    ));
    drop(tx);
    assert!(store.recorded_messages().await.unwrap().is_empty());
    assert!(store.queued_publishes().await.unwrap().is_empty());
    assert!(
        store
            .reader()
            .lane_holders(topics.project())
            .await
            .unwrap()
            .is_empty()
    );
    let mut tx = store.begin().await.unwrap();
    assert!(
        tx.active_claim(topics.project(), &"B-1".parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        tx.next_seq(&format!("{}/feed", topics.project()))
            .await
            .unwrap(),
        agmen_proto::Seq(1)
    );
}

/// `a` holds two claims whose lease ran out: one on open item B-1 and one
/// on done item B-2. Returns the store, its project, B-1 and `a`.
async fn two_lapsed_claims() -> (MemStore, Topics, ItemKey, IdentityId) {
    use crate::Sender;

    let (store, topics) = setup().await;
    store
        .seed_sender(
            &topics,
            Sender {
                id: "aaaaaaaa".parse().unwrap(),
                kind: IdentityKind::Agent,
                handle: "a".to_owned(),
                owner: None,
                verbs: None,
            },
            None,
        )
        .await
        .unwrap();
    let open = "B-1".parse().unwrap();
    let done = "B-2".parse().unwrap();
    let holder = "aaaaaaaa".parse().unwrap();
    let now = Timestamp::now();
    store.set_now(now);
    let mut tx = store.begin().await.unwrap();
    for key in [&open, &done] {
        tx.insert_item(&NewItem {
            project: topics.project(),
            key,
            title: "bug",
            lane: None,
            severity: None,
            created_by: &holder,
        })
        .await
        .unwrap();
    }
    tx.mark_done(topics.project(), &done).await.unwrap();
    tx.insert_claim(topics.project(), &open, &holder, LEASE)
        .await
        .unwrap();
    tx.insert_claim(topics.project(), &done, &holder, LEASE)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    store.set_now(now + LEASE);
    (store, topics, open, holder)
}

#[tokio::test]
async fn lease_sweep_posts_release_for_open_items() {
    use agmen_proto::{Payload, Stream};

    use crate::Core;

    let (store, topics, open, holder) = two_lapsed_claims().await;
    let done: ItemKey = "B-2".parse().unwrap();
    let core = Core::new(store.clone(), LEASE);
    assert_eq!(core.expire_leases().await.unwrap(), 2);
    let rows = store.recorded_messages().await.unwrap();
    let releases: Vec<_> = rows
        .iter()
        .filter(|row| row.stream == Stream::Feed)
        .collect();
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].client.payload,
        Payload::Release {
            item: open.clone(),
            reason: Some("lease_expired".into()),
        }
    );
    assert_eq!(
        releases[0].client.body.as_str(),
        "B-1 released: a's lease ran out"
    );
    assert_eq!(releases[0].mentioned, vec![holder.clone()]);
    let mut tx = store.begin().await.unwrap();
    assert!(
        tx.active_claim(topics.project(), &open)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        tx.active_claim(topics.project(), &done)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn lease_sweep_wakes_the_project_it_posted_in() {
    use std::sync::Mutex;

    use crate::Core;

    let (store, topics, _, _) = two_lapsed_claims().await;
    let core = Core::new(store, LEASE);
    let woken = Mutex::new(Vec::new());
    let other: ProjectId = "project02".parse().unwrap();
    let watches =
        [topics.project(), &other].map(|project| (project, core.notifier().watch_stored(project)));
    core.expire_leases().await.unwrap();
    assert_eq!(
        core.notifier().stored_count(),
        1,
        "one RELEASE must notify exactly once"
    );
    for (project, wake) in watches {
        if wake.has_changed().unwrap() {
            woken.lock().unwrap().push(project.clone());
        }
    }
    assert_eq!(
        *woken.lock().unwrap(),
        [topics.project().clone()],
        "one RELEASE was posted, so one wake, and none for the done item"
    );
}

#[tokio::test]
async fn sweeper_gives_up_after_the_bound() {
    use crate::{Core, StoreError};

    let store = MemStore::default();
    store.fail_always("begin");
    let core = Core::new(store, Duration::from_millis(400));
    // No pause between failures: the public loop waits one second each time.
    let err = core.sweep_until_gave_up(Duration::ZERO).await.unwrap_err();
    assert_eq!(err.failures, 60);
    assert!(matches!(
        err.last,
        LedgerError::Store(StoreError::Backend(_))
    ));
}
