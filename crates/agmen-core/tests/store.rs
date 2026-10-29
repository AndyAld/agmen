#![cfg(test)]

use agmen_core::{ItemLock, Lapsed, NewItem, NewLend, ReleaseBy};
use agmen_proto::Timestamp;
use std::cell::Cell;
use std::time::Duration;

use agmen_core::{
    Core, LaneHolder, LaneRow, MEM_MAX_ROWS, MemStore, MessageRow, Reads, ResolvedLane, Roster,
    Store, StoreError, StoreTx,
};
use agmen_proto::{IdentityId, IdentityKind, ItemKey, LaneName, ProjectId, Seq, Topics};
use tokio::sync::OwnedMutexGuard;

#[tokio::test]
async fn next_seq_starts_at_one_per_key() -> Result<(), StoreError> {
    let store = MemStore::default();
    let mut tx = store.begin().await?;
    assert_eq!(tx.next_seq("a").await?, Seq(1));
    assert_eq!(tx.next_seq("a").await?, Seq(2));
    assert_eq!(tx.next_seq("b").await?, Seq(1));
    tx.commit().await?;

    let mut tx = store.begin().await?;
    assert_eq!(tx.next_seq("a").await?, Seq(3));
    Ok(())
}

#[tokio::test]
async fn dropped_tx_records_nothing() -> Result<(), StoreError> {
    let store = MemStore::default();
    let mut tx = store.begin().await?;
    assert_eq!(tx.next_seq("a").await?, Seq(1));
    drop(tx);

    let mut tx = store.begin().await?;
    assert_eq!(tx.next_seq("a").await?, Seq(1));
    Ok(())
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the compile-only Send check implements every storage statement"
)]
fn tx_is_send_with_a_non_sync_state() {
    fn assert_send<T: Send>() {}
    assert_send::<OwnedMutexGuard<Cell<u8>>>();

    struct NonSyncTx(OwnedMutexGuard<Cell<u8>>);

    impl Reads for NonSyncTx {
        async fn roster(&mut self, _project: &ProjectId) -> Result<Roster, StoreError> {
            Ok(Roster::default())
        }

        async fn ref_targets(
            &mut self,
            _project: &ProjectId,
            _candidates: &[String],
        ) -> Result<(Vec<String>, Vec<ItemKey>), StoreError> {
            Ok((Vec::new(), Vec::new()))
        }

        async fn lane_globs(
            &mut self,
            _project: &ProjectId,
        ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
            Ok(Vec::new())
        }

        async fn lane_holders(
            &mut self,
            _project: &ProjectId,
        ) -> Result<Vec<LaneHolder>, StoreError> {
            Ok(Vec::new())
        }

        async fn live_edit_scope(
            &mut self,
            _project: &ProjectId,
            _id: &IdentityId,
        ) -> Result<Option<(IdentityKind, Option<serde_json::Value>)>, StoreError> {
            Ok(None)
        }

        async fn lends_to(
            &mut self,
            _project: &ProjectId,
            _to: &IdentityId,
        ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
            Ok(Vec::new())
        }

        async fn lane_list(&mut self, _project: &ProjectId) -> Result<Vec<LaneRow>, StoreError> {
            Ok(Vec::new())
        }
    }

    impl StoreTx for NonSyncTx {
        async fn lock_item(
            &mut self,
            _project: &ProjectId,
            _item: &ItemKey,
        ) -> Result<Option<ItemLock>, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn release_lapsed(
            &mut self,
            _project: &ProjectId,
            _item: &ItemKey,
        ) -> Result<Option<Lapsed>, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn active_claim(
            &mut self,
            _project: &ProjectId,
            _item: &ItemKey,
        ) -> Result<Option<(IdentityId, String)>, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn extend_claim(
            &mut self,
            _project: &ProjectId,
            _item: &ItemKey,
            _lease: Duration,
        ) -> Result<Timestamp, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn insert_claim(
            &mut self,
            _project: &ProjectId,
            _item: &ItemKey,
            _from: &IdentityId,
            _lease: Duration,
        ) -> Result<Timestamp, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn renew_claims(
            &mut self,
            _project: &ProjectId,
            _from: &IdentityId,
            _items: &[ItemKey],
            _lease: Duration,
        ) -> Result<u64, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn release_claim(
            &mut self,
            _project: &ProjectId,
            _item: &ItemKey,
            _from: &IdentityId,
            _by: ReleaseBy,
        ) -> Result<u64, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn next_item_number(
            &mut self,
            _project: &ProjectId,
            _prefix: &str,
        ) -> Result<u64, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn insert_item(&mut self, _item: &NewItem<'_>) -> Result<(), StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn assign_item(
            &mut self,
            _project: &ProjectId,
            _item: &ItemKey,
            _to: &IdentityId,
        ) -> Result<u64, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn close_claim(
            &mut self,
            _project: &ProjectId,
            _item: &ItemKey,
            _holder: &IdentityId,
        ) -> Result<u64, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn mark_done(
            &mut self,
            _project: &ProjectId,
            _item: &ItemKey,
        ) -> Result<(), StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn lock_lane_owner(
            &mut self,
            _project: &ProjectId,
            _lane: &LaneName,
        ) -> Result<Option<Option<IdentityId>>, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn identity_live(
            &mut self,
            _project: &ProjectId,
            _id: &IdentityId,
        ) -> Result<bool, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn identity_edits(
            &mut self,
            _id: &IdentityId,
        ) -> Result<Option<serde_json::Value>, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn active_lends(
            &mut self,
            _project: &ProjectId,
            _lane: &LaneName,
            _to: &IdentityId,
        ) -> Result<u64, StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn insert_lend(&mut self, _lend: &NewLend<'_>) -> Result<(), StoreError> {
            Err(StoreError::Stored("compile-only transaction".into()))
        }

        async fn insert_message(&mut self, _row: &MessageRow<'_>) -> Result<(), StoreError> {
            Ok(())
        }

        async fn enqueue_publish(
            &mut self,
            _topics: &Topics,
            _topic: &str,
            _payload: &[u8],
        ) -> Result<(), StoreError> {
            Ok(())
        }

        async fn lock_head(&mut self, _key: &str) -> Result<(), StoreError> {
            Ok(())
        }

        async fn next_seq(&mut self, _key: &str) -> Result<Seq, StoreError> {
            Ok(Seq(u64::from(self.0.get())))
        }

        async fn commit(self) -> Result<(), StoreError> {
            Ok(())
        }

        async fn replace_lanes(
            &mut self,
            _project: &ProjectId,
            _lanes: &[ResolvedLane],
        ) -> Result<(), StoreError> {
            Ok(())
        }

        async fn resolve_owner(
            &mut self,
            _project: &ProjectId,
            _handle: &str,
        ) -> Result<Option<IdentityId>, StoreError> {
            Ok(None)
        }
    }

    fn assert_tx<T: StoreTx>() {}
    assert_tx::<NonSyncTx>();
}

#[tokio::test]
async fn mem_store_is_capped() -> Result<(), StoreError> {
    let store = MemStore::default();
    let mut tx = store.begin().await?;
    for key in 0..MEM_MAX_ROWS {
        assert_eq!(tx.next_seq(&key.to_string()).await?, Seq(1));
    }
    assert!(matches!(
        tx.next_seq("new").await,
        Err(StoreError::Stored(message)) if message == "mem store full"
    ));
    assert!(matches!(
        tx.lock_head("new").await,
        Err(StoreError::Stored(message)) if message == "mem store full"
    ));
    assert_eq!(tx.next_seq("0").await?, Seq(2));
    Ok(())
}

#[tokio::test]
async fn lock_head_does_not_take_a_seq() -> Result<(), StoreError> {
    let store = MemStore::default();
    let mut tx = store.begin().await?;
    tx.lock_head("a").await?;
    tx.lock_head("a").await?;
    tx.commit().await?;

    let mut tx = store.begin().await?;
    assert_eq!(tx.next_seq("a").await?, Seq(1));
    tx.lock_head("a").await?;
    assert_eq!(tx.next_seq("a").await?, Seq(2));
    Ok(())
}

#[tokio::test]
async fn failed_commit_records_nothing() -> Result<(), StoreError> {
    let store = MemStore::default();
    let mut tx = store.begin().await?;
    tx.next_seq("a").await?;
    store.fail_next("commit");
    assert!(matches!(tx.commit().await, Err(StoreError::Backend(_))));

    let mut tx = store.begin().await?;
    assert_eq!(tx.next_seq("a").await?, Seq(1));
    tx.commit().await?;
    Ok(())
}

#[tokio::test]
async fn failure_is_consumed_even_when_tx_is_dropped() -> Result<(), StoreError> {
    let store = MemStore::default();
    let mut tx = store.begin().await?;
    store.fail_next("next_seq");
    assert!(matches!(
        tx.next_seq("a").await,
        Err(StoreError::Backend(_))
    ));
    drop(tx);

    let mut tx = store.begin().await?;
    assert_eq!(tx.next_seq("a").await?, Seq(1));
    Ok(())
}

#[tokio::test]
async fn failure_targets_only_the_named_method() -> Result<(), StoreError> {
    let store = MemStore::default();
    store.fail_next("begin");
    assert!(matches!(store.begin().await, Err(StoreError::Backend(_))));

    store.fail_next("lock_head");
    let mut tx = store.begin().await?;
    assert_eq!(tx.next_seq("a").await?, Seq(1));
    assert!(matches!(
        tx.lock_head("a").await,
        Err(StoreError::Backend(_))
    ));
    tx.lock_head("a").await?;
    assert_eq!(tx.next_seq("a").await?, Seq(2));
    Ok(())
}

#[tokio::test]
async fn core_keeps_its_store_and_lease() -> Result<(), StoreError> {
    let lease = std::time::Duration::from_secs(60);
    let store = MemStore::default();
    let core = Core::new(store.clone(), lease);
    assert_eq!(core.claim_lease(), lease);
    let mut tx = core.store().begin().await?;
    assert_eq!(tx.next_seq("a").await?, Seq(1));
    tx.commit().await?;

    let mut tx = store.begin().await?;
    assert_eq!(tx.next_seq("a").await?, Seq(2));
    Ok(())
}
