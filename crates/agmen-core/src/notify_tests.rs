use agmen_proto::ProjectId;

use super::*;

#[test]
fn a_wake_reaches_only_its_project() {
    let wakes = Wakes::default();
    let a: ProjectId = "projecta1".parse().unwrap();
    let b: ProjectId = "projectb1".parse().unwrap();
    let mut on_a = wakes.watch(&a);
    wakes.wake(&b);
    assert!(
        !on_a.has_changed().unwrap(),
        "a commit in b woke a stream in a"
    );
    wakes.wake(&a);
    assert!(on_a.has_changed().unwrap());
    on_a.mark_unchanged();
    // A project nobody watches keeps no entry.
    drop(on_a);
    wakes.wake(&a);
    assert_eq!(wakes.watched(), 0);
}

#[tokio::test]
async fn hub_posts_wake_after_commit_and_clones_share_the_notifier() {
    use std::time::Duration;

    use crate::{Core, MemStore, Reads, Store};

    let core = Core::new(MemStore::default(), Duration::from_secs(60));
    let topics = crate::replay::tests::topics();
    let mut wake = core.notifier().watch_stored(topics.project());
    let other = core.notifier().watch_stored(&"project02".parse().unwrap());
    core.clone()
        .post_as_hub(&topics, crate::replay::tests::post())
        .await
        .unwrap();
    assert!(wake.has_changed().unwrap());
    assert!(!other.has_changed().unwrap());
    assert_eq!(core.notifier().stored_count(), 1);
    assert_eq!(
        core.store()
            .reader()
            .page(topics.project(), &agmen_proto::Stream::Feed, 0, 1)
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    wake.mark_unchanged();
    tokio::time::timeout(
        Duration::from_secs(1),
        core.notifier().outbox_ready().notified(),
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            core.notifier().outbox_ready().notified()
        )
        .await
        .is_err()
    );
    core.store().fail_next("commit");
    assert!(
        core.post_as_hub(&topics, crate::replay::tests::post())
            .await
            .is_err()
    );
    assert!(!wake.has_changed().unwrap());
    assert_eq!(core.notifier().stored_count(), 1);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            core.notifier().outbox_ready().notified()
        )
        .await
        .is_err()
    );
    assert_eq!(
        core.page(topics.project(), &agmen_proto::Stream::Feed, 0, 1)
            .await
            .unwrap()
            .head,
        1
    );
}
