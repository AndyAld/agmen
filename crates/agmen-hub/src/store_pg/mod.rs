//! Postgres storage adapter, reusing the hub's existing numbering SQL.

mod ledger;
mod messages;

use std::time::Duration;

use agmen_core::{
    LaneHolder, LaneRow, MessageRow, Reads, ResolvedLane, Roster, Store, StoreError, StoreTx,
};
use agmen_proto::{IdentityId, IdentityKind, ItemKey, LaneName, ProjectId, Seq, Timestamp, Topics};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::claims::LedgerError;

#[derive(Debug)]
pub(crate) struct PgStore {
    pool: PgPool,
}

#[derive(Debug)]
pub(crate) struct PgReader<'a>(&'a PgPool);

#[derive(Debug)]
pub(crate) struct PgTx {
    tx: Transaction<'static, Postgres>,
}

impl PgStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl PgTx {
    pub(crate) fn sqlx(&mut self) -> &mut Transaction<'static, Postgres> {
        &mut self.tx
    }
}

impl Reads for PgReader<'_> {
    async fn roster(&mut self, project: &ProjectId) -> Result<Roster, StoreError> {
        messages::roster(self.0, project).await
    }

    async fn ref_targets(
        &mut self,
        project: &ProjectId,
        candidates: &[String],
    ) -> Result<(Vec<String>, Vec<ItemKey>), StoreError> {
        let mut db = self
            .0
            .acquire()
            .await
            .map_err(|err| StoreError::Backend(Box::new(err)))?;
        messages::ref_targets(&mut db, project, candidates).await
    }

    async fn lane_globs(
        &mut self,
        project: &ProjectId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        ledger::load_lanes(self.0, project.as_str())
            .await
            .map_err(from_ledger)
    }

    async fn lane_holders(&mut self, project: &ProjectId) -> Result<Vec<LaneHolder>, StoreError> {
        ledger::lane_holders(self.0, project.as_str())
            .await
            .map_err(from_ledger)
    }

    async fn live_edit_scope(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<Option<(IdentityKind, Option<serde_json::Value>)>, StoreError> {
        ledger::live_edit_scope(self.0, project, id)
            .await
            .map_err(from_ledger)
    }

    async fn lends_to(
        &mut self,
        project: &ProjectId,
        to: &IdentityId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        ledger::load_lends(self.0, project.as_str(), to)
            .await
            .map_err(from_ledger)
    }

    async fn lane_list(&mut self, project: &ProjectId) -> Result<Vec<LaneRow>, StoreError> {
        ledger::lane_list(self.0, project)
            .await
            .map_err(from_ledger)
    }
}

impl Reads for PgTx {
    async fn roster(&mut self, project: &ProjectId) -> Result<Roster, StoreError> {
        messages::roster(connection(&mut self.tx), project).await
    }

    async fn ref_targets(
        &mut self,
        project: &ProjectId,
        candidates: &[String],
    ) -> Result<(Vec<String>, Vec<ItemKey>), StoreError> {
        messages::ref_targets(connection(&mut self.tx), project, candidates).await
    }

    async fn lane_globs(
        &mut self,
        project: &ProjectId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        ledger::load_lanes(connection(&mut self.tx), project.as_str())
            .await
            .map_err(from_ledger)
    }

    async fn lane_holders(&mut self, project: &ProjectId) -> Result<Vec<LaneHolder>, StoreError> {
        ledger::lane_holders(connection(&mut self.tx), project.as_str())
            .await
            .map_err(from_ledger)
    }

    async fn live_edit_scope(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<Option<(IdentityKind, Option<serde_json::Value>)>, StoreError> {
        ledger::live_edit_scope(connection(&mut self.tx), project, id)
            .await
            .map_err(from_ledger)
    }

    async fn lends_to(
        &mut self,
        project: &ProjectId,
        to: &IdentityId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        ledger::load_lends(connection(&mut self.tx), project.as_str(), to)
            .await
            .map_err(from_ledger)
    }

    async fn lane_list(&mut self, project: &ProjectId) -> Result<Vec<LaneRow>, StoreError> {
        ledger::lane_list(connection(&mut self.tx), project)
            .await
            .map_err(from_ledger)
    }
}

impl Store for PgStore {
    type Reader<'a> = PgReader<'a>;
    type Tx<'a> = PgTx;

    fn reader(&self) -> Self::Reader<'_> {
        PgReader(&self.pool)
    }

    async fn begin(&self) -> Result<Self::Tx<'_>, StoreError> {
        let tx = self
            .pool
            .begin()
            .await
            .map_err(|source| StoreError::Backend(Box::new(source)))?;
        Ok(PgTx { tx })
    }
}

impl StoreTx for PgTx {
    async fn lock_item(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> Result<Option<agmen_core::ItemLock>, StoreError> {
        ledger::lock_item(self.sqlx(), project, item).await
    }

    async fn release_lapsed(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> Result<Option<agmen_core::Lapsed>, StoreError> {
        ledger::release_lapsed(self.sqlx(), project, item).await
    }

    async fn active_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> Result<Option<(IdentityId, String)>, StoreError> {
        ledger::active_claim(self.sqlx(), project, item).await
    }

    async fn extend_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        lease: Duration,
    ) -> Result<Timestamp, StoreError> {
        ledger::extend_claim(self.sqlx(), project, item, lease).await
    }

    async fn insert_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        from: &IdentityId,
        lease: Duration,
    ) -> Result<Timestamp, StoreError> {
        ledger::insert_claim(self.sqlx(), project, item, from, lease).await
    }

    async fn renew_claims(
        &mut self,
        project: &ProjectId,
        from: &IdentityId,
        items: &[ItemKey],
        lease: Duration,
    ) -> Result<u64, StoreError> {
        ledger::renew_claims(self.sqlx(), project, from, items, lease).await
    }

    async fn release_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        from: &IdentityId,
        by: agmen_core::ReleaseBy,
    ) -> Result<u64, StoreError> {
        ledger::release_claim(self.sqlx(), project, item, from, by).await
    }

    async fn next_item_number(
        &mut self,
        project: &ProjectId,
        prefix: &str,
    ) -> Result<u64, StoreError> {
        ledger::next_item_number(self.sqlx(), project, prefix).await
    }

    async fn insert_item(&mut self, item: &agmen_core::NewItem<'_>) -> Result<(), StoreError> {
        ledger::insert_item(self.sqlx(), item).await
    }

    async fn assign_item(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        to: &IdentityId,
    ) -> Result<u64, StoreError> {
        ledger::assign_item(self.sqlx(), project, item, to).await
    }

    async fn close_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        holder: &IdentityId,
    ) -> Result<u64, StoreError> {
        ledger::close_claim(self.sqlx(), project, item, holder).await
    }

    async fn mark_done(&mut self, project: &ProjectId, item: &ItemKey) -> Result<(), StoreError> {
        ledger::mark_done(self.sqlx(), project, item).await
    }

    async fn lock_lane_owner(
        &mut self,
        project: &ProjectId,
        lane: &LaneName,
    ) -> Result<Option<Option<IdentityId>>, StoreError> {
        ledger::lock_lane_owner(self.sqlx(), project, lane).await
    }

    async fn identity_live(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<bool, StoreError> {
        ledger::identity_live(self.sqlx(), project, id).await
    }

    async fn identity_edits(
        &mut self,
        id: &IdentityId,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        ledger::identity_edits(self.sqlx(), id).await
    }

    async fn active_lends(
        &mut self,
        project: &ProjectId,
        lane: &LaneName,
        to: &IdentityId,
    ) -> Result<u64, StoreError> {
        ledger::active_lends(self.sqlx(), project, lane, to).await
    }

    async fn insert_lend(&mut self, lend: &agmen_core::NewLend<'_>) -> Result<(), StoreError> {
        ledger::insert_lend(self.sqlx(), lend).await
    }

    async fn insert_message(&mut self, row: &MessageRow<'_>) -> Result<(), StoreError> {
        messages::insert_message(self.sqlx(), row).await
    }

    async fn enqueue_publish(
        &mut self,
        topics: &Topics,
        topic: &str,
        payload: &[u8],
    ) -> Result<(), StoreError> {
        messages::enqueue_publish(self.sqlx(), topics, topic, payload).await
    }

    async fn lock_head(&mut self, key: &str) -> Result<(), StoreError> {
        crate::numbering::lock_head(self.sqlx(), key)
            .await
            .map_err(|source| StoreError::Backend(Box::new(source)))
    }

    async fn next_seq(&mut self, key: &str) -> Result<Seq, StoreError> {
        let seq = crate::numbering::next_seq(self.sqlx(), key)
            .await
            .map_err(|source| StoreError::Backend(Box::new(source)))?;
        let seq = u64::try_from(seq).map_err(|source| StoreError::Backend(Box::new(source)))?;
        Ok(Seq(seq))
    }

    async fn commit(self) -> Result<(), StoreError> {
        self.tx
            .commit()
            .await
            .map_err(|source| StoreError::Backend(Box::new(source)))
    }

    async fn replace_lanes(
        &mut self,
        project: &ProjectId,
        lanes: &[ResolvedLane],
    ) -> Result<(), StoreError> {
        ledger::replace_lanes(self.sqlx(), project, lanes)
            .await
            .map_err(from_ledger)
    }

    async fn resolve_owner(
        &mut self,
        project: &ProjectId,
        handle: &str,
    ) -> Result<Option<IdentityId>, StoreError> {
        ledger::resolve_owner(connection(&mut self.tx), project, handle)
            .await
            .map_err(from_ledger)
    }
}

fn connection<'a>(tx: &'a mut Transaction<'static, Postgres>) -> &'a mut PgConnection {
    tx
}

fn from_ledger(err: LedgerError) -> StoreError {
    match err {
        LedgerError::Database(err) => StoreError::Backend(Box::new(err)),
        LedgerError::Stored(message) => StoreError::Stored(message),
        LedgerError::Post(err) => StoreError::Backend(Box::new(err)),
        LedgerError::Proto(err) => StoreError::Backend(Box::new(err)),
    }
}
