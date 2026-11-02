//! Synchronous ledger operations, filled by the following record tasks.

use std::time::Duration;

use agmen_core::{
    ExpiredClaim, ItemLock, ItemRow, LaneHolder, LaneRow, Lapsed, QuotaTaken, ReleaseBy,
    ResolvedLane, StoreError,
};
use agmen_proto::{IdentityId, IdentityKind, ItemKey, LaneName, ProjectId, TenantId, Timestamp};
use rusqlite::Connection;

use super::{OwnedItem, OwnedLend};

pub(super) fn items(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
) -> Result<Vec<ItemRow>, StoreError> {
    Err(StoreError::Stored("sqlite items is not built yet".into()))
}

pub(super) fn lane_globs(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
    Err(StoreError::Stored(
        "sqlite lane_globs is not built yet".into(),
    ))
}

pub(super) fn lane_holders(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
) -> Result<Vec<LaneHolder>, StoreError> {
    Err(StoreError::Stored(
        "sqlite lane_holders is not built yet".into(),
    ))
}

pub(super) fn live_edit_scope(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _id: IdentityId,
) -> Result<Option<(IdentityKind, Option<serde_json::Value>)>, StoreError> {
    Err(StoreError::Stored(
        "sqlite live_edit_scope is not built yet".into(),
    ))
}

pub(super) fn lends_to(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _to: IdentityId,
) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
    Err(StoreError::Stored(
        "sqlite lends_to is not built yet".into(),
    ))
}

pub(super) fn lane_list(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
) -> Result<Vec<LaneRow>, StoreError> {
    Err(StoreError::Stored(
        "sqlite lane_list is not built yet".into(),
    ))
}

pub(super) fn take_quota(
    _conn: &Connection,
    _now: i64,
    _tenant: TenantId,
    _limit: u32,
) -> Result<QuotaTaken, StoreError> {
    Err(StoreError::Stored(
        "sqlite take_quota is not built yet".into(),
    ))
}

pub(super) fn lock_item(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _item: ItemKey,
) -> Result<Option<ItemLock>, StoreError> {
    Err(StoreError::Stored(
        "sqlite lock_item is not built yet".into(),
    ))
}

pub(super) fn release_lapsed(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _item: ItemKey,
) -> Result<Option<Lapsed>, StoreError> {
    Err(StoreError::Stored(
        "sqlite release_lapsed is not built yet".into(),
    ))
}

pub(super) fn active_claim(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _item: ItemKey,
) -> Result<Option<(IdentityId, String)>, StoreError> {
    Err(StoreError::Stored(
        "sqlite active_claim is not built yet".into(),
    ))
}

pub(super) fn extend_claim(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _item: ItemKey,
    _lease: Duration,
) -> Result<Timestamp, StoreError> {
    Err(StoreError::Stored(
        "sqlite extend_claim is not built yet".into(),
    ))
}

pub(super) fn insert_claim(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _item: ItemKey,
    _from: IdentityId,
    _lease: Duration,
) -> Result<Timestamp, StoreError> {
    Err(StoreError::Stored(
        "sqlite insert_claim is not built yet".into(),
    ))
}

pub(super) fn renew_claims(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _from: IdentityId,
    _items: Vec<ItemKey>,
    _lease: Duration,
) -> Result<u64, StoreError> {
    Err(StoreError::Stored(
        "sqlite renew_claims is not built yet".into(),
    ))
}

pub(super) fn release_claim(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _item: ItemKey,
    _from: IdentityId,
    _by: ReleaseBy,
) -> Result<u64, StoreError> {
    Err(StoreError::Stored(
        "sqlite release_claim is not built yet".into(),
    ))
}

pub(super) fn next_item_number(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _prefix: String,
) -> Result<u64, StoreError> {
    Err(StoreError::Stored(
        "sqlite next_item_number is not built yet".into(),
    ))
}

pub(super) fn insert_item(
    _conn: &Connection,
    _now: i64,
    item: OwnedItem,
) -> Result<(), StoreError> {
    drop((
        item.project,
        item.key,
        item.title,
        item.lane,
        item.severity,
        item.created_by,
    ));
    Err(StoreError::Stored(
        "sqlite insert_item is not built yet".into(),
    ))
}

pub(super) fn assign_item(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _item: ItemKey,
    _to: IdentityId,
) -> Result<u64, StoreError> {
    Err(StoreError::Stored(
        "sqlite assign_item is not built yet".into(),
    ))
}

pub(super) fn close_claim(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _item: ItemKey,
    _holder: IdentityId,
) -> Result<u64, StoreError> {
    Err(StoreError::Stored(
        "sqlite close_claim is not built yet".into(),
    ))
}

pub(super) fn mark_done(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _item: ItemKey,
) -> Result<(), StoreError> {
    Err(StoreError::Stored(
        "sqlite mark_done is not built yet".into(),
    ))
}

pub(super) fn lock_lane_owner(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _lane: LaneName,
) -> Result<Option<Option<IdentityId>>, StoreError> {
    Err(StoreError::Stored(
        "sqlite lock_lane_owner is not built yet".into(),
    ))
}

pub(super) fn identity_live(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _id: IdentityId,
) -> Result<bool, StoreError> {
    Err(StoreError::Stored(
        "sqlite identity_live is not built yet".into(),
    ))
}

pub(super) fn identity_edits(
    _conn: &Connection,
    _now: i64,
    _id: IdentityId,
) -> Result<Option<serde_json::Value>, StoreError> {
    Err(StoreError::Stored(
        "sqlite identity_edits is not built yet".into(),
    ))
}

pub(super) fn active_lends(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _lane: LaneName,
    _to: IdentityId,
) -> Result<u64, StoreError> {
    Err(StoreError::Stored(
        "sqlite active_lends is not built yet".into(),
    ))
}

pub(super) fn insert_lend(
    _conn: &Connection,
    _now: i64,
    lend: OwnedLend,
) -> Result<(), StoreError> {
    drop((
        lend.project,
        lend.lane,
        lend.message_id,
        lend.from,
        lend.to,
        lend.paths,
        lend.until,
    ));
    Err(StoreError::Stored(
        "sqlite insert_lend is not built yet".into(),
    ))
}

pub(super) fn replace_lanes(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _lanes: Vec<ResolvedLane>,
) -> Result<(), StoreError> {
    Err(StoreError::Stored(
        "sqlite replace_lanes is not built yet".into(),
    ))
}

pub(super) fn resolve_owner(
    _conn: &Connection,
    _now: i64,
    _project: ProjectId,
    _handle: String,
) -> Result<Option<IdentityId>, StoreError> {
    Err(StoreError::Stored(
        "sqlite resolve_owner is not built yet".into(),
    ))
}

pub(super) fn expire_claims(
    _conn: &Connection,
    _now: i64,
    _limit: u32,
) -> Result<Vec<ExpiredClaim>, StoreError> {
    Err(StoreError::Stored(
        "sqlite expire_claims is not built yet".into(),
    ))
}
