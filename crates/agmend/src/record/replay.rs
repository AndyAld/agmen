//! Synchronous replay operations, filled by the following record tasks.

use agmen_core::{Page, StoreError};
use agmen_proto::{ProjectId, Stream};
use rusqlite::Connection;

pub(super) fn page(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _stream: Stream,
    _after: u64,
    _limit: u32,
) -> Result<Page, StoreError> {
    Err(StoreError::Stored("sqlite page is not built yet".into()))
}
