//! Postgres storage adapter, reusing the hub's existing numbering SQL.

mod ledger;

use agmen_core::{LaneHolder, LaneRow, Reads, ResolvedLane, Store, StoreError, StoreTx};
use agmen_proto::{IdentityId, IdentityKind, LaneName, ProjectId, Seq};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::claims::LedgerError;

pub(crate) use ledger::{lane_holders, load_lanes, load_lends};

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
