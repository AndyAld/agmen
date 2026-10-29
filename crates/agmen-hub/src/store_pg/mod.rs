//! Postgres storage adapter, reusing the hub's existing numbering SQL.

use agmen_core::{Reads, Store, StoreError, StoreTx};
use agmen_proto::Seq;
use sqlx::{PgPool, Postgres, Transaction};

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the hub starts using this adapter in later core tasks"
)]
pub(crate) struct PgStore {
    pool: PgPool,
}

#[derive(Debug)]
#[expect(dead_code, reason = "storage queries are moved in later core tasks")]
pub(crate) struct PgReader<'a>(&'a PgPool);

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the hub starts using this adapter in later core tasks"
)]
pub(crate) struct PgTx {
    tx: Transaction<'static, Postgres>,
}

impl PgTx {
    #[expect(dead_code, reason = "bridges hub SQL while later core tasks move it")]
    pub(crate) fn sqlx(&mut self) -> &mut Transaction<'static, Postgres> {
        &mut self.tx
    }
}

impl Reads for PgReader<'_> {}
impl Reads for PgTx {}

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
}
