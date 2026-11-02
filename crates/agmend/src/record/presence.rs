//! Synchronous presence operations, filled by the following record tasks.

use agmen_core::{LatestHellos, RosterRow, StoreError};
use agmen_proto::ProjectId;
use rusqlite::Connection;

pub(super) fn roster_rows(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
) -> Result<Vec<RosterRow>, StoreError> {
    Err(StoreError::Stored(
        "sqlite roster_rows is not built yet".into(),
    ))
}

pub(super) fn latest_hellos(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
) -> Result<LatestHellos, StoreError> {
    Err(StoreError::Stored(
        "sqlite latest_hellos is not built yet".into(),
    ))
}
