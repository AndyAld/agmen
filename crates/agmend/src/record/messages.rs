//! Synchronous messages operations, filled by the following record tasks.

use agmen_core::{Parent, Roster, Sender, StoreError};
use agmen_proto::{IdentityId, ItemKey, MessageId, ProjectId, Seq, Stream, Topics};
use rusqlite::Connection;

use super::OwnedMessageRow;

pub(super) fn project_exists(
    _conn: &Connection,
    _now: i64,
    _topics: Topics,
) -> Result<bool, StoreError> {
    Err(StoreError::Stored(
        "sqlite project_exists is not built yet".into(),
    ))
}

pub(super) fn sender(
    _conn: &Connection,
    _now: i64,
    _topics: Topics,
    _from: IdentityId,
) -> Result<Option<Sender>, StoreError> {
    Err(StoreError::Stored("sqlite sender is not built yet".into()))
}

pub(super) fn parent(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _sender: IdentityId,
    _reply_to: MessageId,
) -> Result<Option<Parent>, StoreError> {
    Err(StoreError::Stored("sqlite parent is not built yet".into()))
}

pub(super) fn is_delegate(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _worker: IdentityId,
    _delegator: IdentityId,
) -> Result<bool, StoreError> {
    Err(StoreError::Stored(
        "sqlite is_delegate is not built yet".into(),
    ))
}

pub(super) fn recorded(
    _conn: &Connection,
    _now: i64,
    _from: IdentityId,
    _id: MessageId,
) -> Result<Option<(Stream, Seq, Vec<u8>)>, StoreError> {
    Err(StoreError::Stored(
        "sqlite recorded is not built yet".into(),
    ))
}

pub(super) fn roster(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
) -> Result<Roster, StoreError> {
    Err(StoreError::Stored("sqlite roster is not built yet".into()))
}

pub(super) fn ref_targets(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _candidates: Vec<String>,
) -> Result<(Vec<String>, Vec<ItemKey>), StoreError> {
    Err(StoreError::Stored(
        "sqlite ref_targets is not built yet".into(),
    ))
}

pub(super) fn insert_message(
    _conn: &Connection,
    _now: i64,
    row: OwnedMessageRow,
) -> Result<(), StoreError> {
    drop((
        row.project,
        row.stream,
        row.seq,
        row.id,
        row.from,
        row.payload_hash,
        row.envelope_json,
        row.hub_at,
        row.ack_row,
    ));
    Err(StoreError::Stored(
        "sqlite insert_message is not built yet".into(),
    ))
}

pub(super) fn enqueue_publish(
    _conn: &Connection,
    _now: i64,
    _topics: Topics,
    _topic: String,
    _payload: Vec<u8>,
) -> Result<(), StoreError> {
    Err(StoreError::Stored(
        "sqlite enqueue_publish is not built yet".into(),
    ))
}
