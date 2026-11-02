use std::time::Duration;

use agmen_proto::{Severity, Timestamp};

use crate::{Core, MAX_LISTED, MemStore, NewItem, Reads, Store, StoreTx};

#[tokio::test]
async fn items_hide_security_titles_and_keep_order_and_claim_details() {
    let (store, topics) = crate::claims::tests::setup().await;
    let core = Core::new(store.clone(), Duration::from_secs(60));
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    store.set_now(now);
    let creator = "aaaaaaaa".parse().unwrap();
    let holder = "bbbbbbbb".parse().unwrap();
    let mut tx = store.begin().await.unwrap();
    for (key, severity) in [
        ("B-2", Some(Severity::Security)),
        ("B-1", Some(Severity::High)),
    ] {
        tx.insert_item(&NewItem {
            project: topics.project(),
            key: &key.parse().unwrap(),
            title: "finding title",
            lane: Some(&"L-UI".parse().unwrap()),
            severity,
            created_by: &creator,
        })
        .await
        .unwrap();
    }
    let until = tx
        .insert_claim(
            topics.project(),
            &"B-2".parse().unwrap(),
            &holder,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    tx.assign_item(topics.project(), &"B-2".parse().unwrap(), &holder)
        .await
        .unwrap();
    tx.mark_done(topics.project(), &"B-1".parse().unwrap())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    store.set_now(now + Duration::from_secs(1));
    let mut tx = store.begin().await.unwrap();
    tx.insert_item(&NewItem {
        project: topics.project(),
        key: &"B-0".parse().unwrap(),
        title: "newest",
        lane: None,
        severity: None,
        created_by: &creator,
    })
    .await
    .unwrap();
    tx.commit().await.unwrap();
    store.revoke_sender(topics.project(), &holder).await;
    let list = core.items(topics.project()).await.unwrap();
    assert_eq!(
        list.items
            .iter()
            .map(|item| item.key.as_str())
            .collect::<Vec<_>>(),
        ["B-1", "B-2", "B-0"]
    );
    assert_eq!(list.items[0].title.as_deref(), Some("finding title"));
    assert_eq!(list.items[0].state, "done");
    let secret = &list.items[1];
    assert_eq!(secret.title, None);
    assert_eq!(secret.severity.as_deref(), Some("security"));
    assert_eq!(secret.lane.as_deref(), Some("L-UI"));
    assert_eq!(secret.created_by, creator.as_str());
    assert_eq!(secret.assigned_to.as_deref(), Some(holder.as_str()));
    assert_eq!(secret.holder.as_deref(), Some(holder.as_str()));
    assert_eq!(secret.holder_handle.as_deref(), Some("b"));
    assert_eq!(secret.lease_until, Some(until));
    assert!(serde_json::to_value(&list).unwrap()["items"][1]["title"].is_null());
    let rows = store.reader().items(topics.project()).await.unwrap();
    assert_eq!(rows[1].1, "finding title", "filtering belongs to the core");
    store.set_now(until);
    let expired = core.items(topics.project()).await.unwrap();
    assert_eq!(expired.items[1].holder, None);
    assert_eq!(expired.items[1].holder_handle, None);
    assert_eq!(expired.items[1].lease_until, None);
}

#[tokio::test]
async fn item_lists_are_capped_and_project_scoped() {
    let store = MemStore::default();
    let core = Core::new(store.clone(), Duration::from_secs(60));
    let topics = crate::replay::tests::topics();
    let mut tx = store.begin().await.unwrap();
    for n in 0..=MAX_LISTED {
        tx.insert_item(&NewItem {
            project: topics.project(),
            key: &format!("B-{n}").parse().unwrap(),
            title: "finding",
            lane: None,
            severity: None,
            created_by: &"aaaaaaaa".parse().unwrap(),
        })
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    assert_eq!(
        core.items(topics.project()).await.unwrap().items.len(),
        MAX_LISTED as usize
    );
    assert!(
        core.items(&"project02".parse().unwrap())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    store.fail_next("items");
    assert!(core.items(topics.project()).await.is_err());
}

#[tokio::test]
async fn transaction_reads_include_its_items_until_rollback() {
    let store = MemStore::default();
    let topics = crate::replay::tests::topics();
    let mut tx = store.begin().await.unwrap();
    tx.insert_item(&NewItem {
        project: topics.project(),
        key: &"B-1".parse().unwrap(),
        title: "uncommitted",
        lane: None,
        severity: None,
        created_by: &"aaaaaaaa".parse().unwrap(),
    })
    .await
    .unwrap();
    assert_eq!(tx.items(topics.project()).await.unwrap().len(), 1);
    drop(tx);
    assert!(
        store
            .reader()
            .items(topics.project())
            .await
            .unwrap()
            .is_empty()
    );
}
