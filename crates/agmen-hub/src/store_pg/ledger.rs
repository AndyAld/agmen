//! Lane SQL. The statements are the ones the hub used before the rules moved
//! into `agmen-core`.

use std::time::Duration;

use agmen_core::{
    LaneHolder, LaneRow, MAX_GLOBS as GLOB_CAP, MAX_GLOBS_PER_LANE as GLOBS_PER_LANE_CAP,
    MAX_LANES as LANE_ROW_CAP, MAX_LOADED_LENDS, ResolvedLane, StoreError, parse_kind,
};
use agmen_proto::{IdentityId, IdentityKind, ItemKey, LaneName, ProjectId, Severity, Timestamp};
use sqlx::{PgExecutor, Postgres, Transaction};

use crate::claims::LedgerError;

/// Live claims one path check will read.
const HOLDER_ROW_CAP: usize = 100_000;

fn stored(what: impl std::fmt::Display) -> LedgerError {
    LedgerError::Stored(what.to_string())
}

/// Lane names and globs, ordered by name.
pub(crate) async fn load_lanes<'e, E>(
    db: E,
    project: &str,
) -> Result<Vec<(LaneName, Vec<String>)>, LedgerError>
where
    E: PgExecutor<'e>,
{
    let rows: Vec<(String, Vec<String>)> =
        sqlx::query_as("SELECT name, globs FROM lanes WHERE project_id = $1 ORDER BY name")
            .bind(project)
            .fetch_all(db)
            .await?;
    if rows.len() > LANE_ROW_CAP {
        return Err(stored(format!("stored lanes exceed {LANE_ROW_CAP}")));
    }
    let mut total = 0_usize;
    let mut loaded = Vec::with_capacity(rows.len());
    for (name, globs) in rows {
        if globs.len() > GLOBS_PER_LANE_CAP || total.saturating_add(globs.len()) > GLOB_CAP {
            return Err(stored("stored lane globs exceed the glob cap"));
        }
        total += globs.len();
        loaded.push((name.parse().map_err(stored)?, globs));
    }
    Ok(loaded)
}

/// Active lends to `to`, as the lane and its path globs.
pub(crate) async fn load_lends<'e, E>(
    db: E,
    project: &str,
    to: &IdentityId,
) -> Result<Vec<(LaneName, Vec<String>)>, LedgerError>
where
    E: PgExecutor<'e>,
{
    let rows: Vec<(String, Vec<String>)> = sqlx::query_as(
        "SELECT g.lane, g.paths FROM lane_lends g
             JOIN lanes l ON l.project_id=g.project_id AND l.name=g.lane
             JOIN identities i ON i.id=g.from_id AND i.project_id=g.project_id
             WHERE g.project_id=$1 AND g.to_id=$2 AND g.until_at > now()
               AND i.revoked_at IS NULL
               AND (l.owner_identity_id=g.from_id OR i.kind='coordinator')
             ORDER BY g.lane, g.until_at, g.message_id LIMIT $3",
    )
    .bind(project)
    .bind(to.as_str())
    .bind(MAX_LOADED_LENDS)
    .fetch_all(db)
    .await?;
    let cap = usize::try_from(MAX_LOADED_LENDS).map_err(stored)?;
    if rows.len() > cap {
        return Err(stored("stored lends exceed the load cap"));
    }
    rows.into_iter()
        .map(|(lane, paths)| Ok((lane.parse().map_err(stored)?, paths)))
        .collect()
}

/// Who holds a live claim on an item with a lane, oldest claim first.
pub(crate) async fn lane_holders<'e, E>(
    db: E,
    project: &str,
) -> Result<Vec<LaneHolder>, LedgerError>
where
    E: PgExecutor<'e>,
{
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT i.lane, c.identity_id, h.handle
         FROM claims c
         JOIN items i ON i.project_id = c.project_id AND i.key = c.item_key
         JOIN identities h ON h.id = c.identity_id
         WHERE c.project_id = $1 AND c.released_at IS NULL AND c.lease_until > now()
           AND i.lane IS NOT NULL
         ORDER BY c.granted_at, c.id",
    )
    .bind(project)
    .fetch_all(db)
    .await?;
    if rows.len() > HOLDER_ROW_CAP {
        return Err(stored(format!(
            "more than {HOLDER_ROW_CAP} live lane claims"
        )));
    }
    rows.into_iter()
        .map(|(lane, identity, handle)| {
            Ok(LaneHolder {
                lane: lane.parse().map_err(stored)?,
                identity: identity.parse().map_err(stored)?,
                handle,
            })
        })
        .collect()
}

#[derive(sqlx::FromRow)]
struct LaneListRow {
    name: String,
    globs: Vec<String>,
    owner_identity_id: Option<String>,
    handle: Option<String>,
    v: i64,
}

/// Every lane of `project`, ordered by name.
pub(crate) async fn lane_list<'e, E>(
    db: E,
    project: &ProjectId,
) -> Result<Vec<LaneRow>, LedgerError>
where
    E: PgExecutor<'e>,
{
    let rows: Vec<LaneListRow> = sqlx::query_as(
        "SELECT l.name, l.globs, l.owner_identity_id, i.handle, l.v
         FROM lanes l LEFT JOIN identities i ON i.id = l.owner_identity_id
         WHERE l.project_id = $1 ORDER BY l.name",
    )
    .bind(project.as_str())
    .fetch_all(db)
    .await?;
    if rows.len() > LANE_ROW_CAP {
        return Err(stored(format!("stored lanes exceed {LANE_ROW_CAP}")));
    }
    let mut listed = Vec::with_capacity(rows.len());
    for row in rows {
        row.name.parse::<LaneName>().map_err(stored)?;
        if let Some(owner_id) = &row.owner_identity_id {
            owner_id.parse::<IdentityId>().map_err(stored)?;
        }
        listed.push(LaneRow {
            name: row.name,
            globs: row.globs,
            owner: row.owner_identity_id,
            owner_handle: row.handle,
            v: row.v,
        });
    }
    Ok(listed)
}

/// The id of a live identity named by id or by handle.
pub(crate) async fn resolve_owner<'e, E>(
    db: E,
    project: &ProjectId,
    handle: &str,
) -> Result<Option<IdentityId>, LedgerError>
where
    E: PgExecutor<'e>,
{
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM identities
         WHERE project_id = $1 AND revoked_at IS NULL AND (id = $2 OR handle = $2)",
    )
    .bind(project.as_str())
    .bind(handle)
    .fetch_optional(db)
    .await?;
    id.map(|id| id.parse().map_err(stored)).transpose()
}

/// The live identity's kind and stored edit scope. `Ok(None)` means missing
/// or revoked.
pub(crate) async fn live_edit_scope<'e, E>(
    db: E,
    project: &ProjectId,
    id: &IdentityId,
) -> Result<Option<(IdentityKind, Option<serde_json::Value>)>, LedgerError>
where
    E: PgExecutor<'e>,
{
    let row: Option<(String, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT kind, edits FROM identities WHERE id = $1 AND project_id = $2 AND revoked_at IS NULL",
    )
    .bind(id.as_str())
    .bind(project.as_str())
    .fetch_optional(db)
    .await?;
    let Some((kind, edits)) = row else {
        return Ok(None);
    };
    Ok(Some((
        parse_kind(&kind)
            // Preserve the ledger's stored-value error text.
            .map_err(|_| stored(format!("unknown identity kind {kind}")))?,
        edits,
    )))
}

/// Replaces the project's lanes under one new version. The project lock is
/// the first statement.
pub(crate) async fn replace_lanes(
    tx: &mut Transaction<'static, Postgres>,
    project: &ProjectId,
    lanes: &[ResolvedLane],
) -> Result<(), LedgerError> {
    if lanes.len() > LANE_ROW_CAP {
        return Err(stored(format!(
            "a project has at most {LANE_ROW_CAP} lanes"
        )));
    }
    // Serializes PUTs of one project, so each gets its own v. NO KEY UPDATE
    // leaves inserts that reference the project unblocked.
    sqlx::query("SELECT 1 FROM projects WHERE id = $1 FOR NO KEY UPDATE")
        .bind(project.as_str())
        .execute(&mut **tx)
        .await?;
    let v: i64 =
        sqlx::query_scalar("SELECT coalesce(max(v), 0) + 1 FROM lanes WHERE project_id = $1")
            .bind(project.as_str())
            .fetch_one(&mut **tx)
            .await?;
    sqlx::query("DELETE FROM lanes WHERE project_id = $1")
        .bind(project.as_str())
        .execute(&mut **tx)
        .await?;
    for lane in lanes.iter().take(LANE_ROW_CAP) {
        sqlx::query(
            "INSERT INTO lanes (project_id, name, globs, owner_identity_id, v)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(project.as_str())
        .bind(lane.name.as_str())
        .bind(&lane.globs)
        .bind(lane.owner.as_ref().map(IdentityId::as_str))
        .bind(v)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

pub(super) async fn lock_item(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    item: &ItemKey,
) -> Result<Option<agmen_core::ItemLock>, StoreError> {
    let row = sqlx::query_as(
        "SELECT i.state, i.lane, i.assigned_to, l.owner_identity_id
         FROM items i
         LEFT JOIN lanes l ON l.project_id = i.project_id AND l.name = i.lane
         WHERE i.project_id = $1 AND i.key = $2
         FOR UPDATE OF i",
    )
    .bind(project.as_str())
    .bind(item.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(row)
}

pub(super) async fn release_lapsed(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    item: &ItemKey,
) -> Result<Option<agmen_core::Lapsed>, StoreError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "UPDATE claims c SET released_at = now() FROM identities h
         WHERE c.project_id = $1 AND c.item_key = $2 AND c.released_at IS NULL
           AND c.lease_until <= now() AND h.id = c.identity_id
         RETURNING c.identity_id, h.handle",
    )
    .bind(project.as_str())
    .bind(item.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(backend)?;
    row.map(|(holder, handle)| {
        Ok(agmen_core::Lapsed {
            holder: holder.parse().map_err(stored_value)?,
            handle,
        })
    })
    .transpose()
}

pub(super) async fn active_claim(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    item: &ItemKey,
) -> Result<Option<(IdentityId, String)>, StoreError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT c.identity_id, h.handle FROM claims c JOIN identities h ON h.id = c.identity_id
         WHERE c.project_id = $1 AND c.item_key = $2 AND c.released_at IS NULL",
    )
    .bind(project.as_str())
    .bind(item.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(backend)?;
    row.map(|(holder, handle)| Ok((holder.parse().map_err(stored_value)?, handle)))
        .transpose()
}

pub(super) async fn extend_claim(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    item: &ItemKey,
    lease: Duration,
) -> Result<Timestamp, StoreError> {
    let ms: i64 = sqlx::query_scalar(
        "UPDATE claims SET lease_until = now() + $3 * interval '1 millisecond'
                 WHERE project_id = $1 AND item_key = $2 AND released_at IS NULL
                 RETURNING (extract(epoch FROM lease_until) * 1000)::bigint",
    )
    .bind(project.as_str())
    .bind(item.as_str())
    .bind(lease_ms(lease))
    .fetch_one(&mut **tx)
    .await
    .map_err(backend)?;
    Timestamp::from_millisecond(ms).map_err(stored_value)
}

pub(super) async fn insert_claim(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    item: &ItemKey,
    from: &IdentityId,
    lease: Duration,
) -> Result<Timestamp, StoreError> {
    let ms: i64 = sqlx::query_scalar(
        "INSERT INTO claims (project_id, item_key, identity_id, lease_until)
                 VALUES ($1, $2, $3, now() + $4 * interval '1 millisecond')
                 RETURNING (extract(epoch FROM lease_until) * 1000)::bigint",
    )
    .bind(project.as_str())
    .bind(item.as_str())
    .bind(from.as_str())
    .bind(lease_ms(lease))
    .fetch_one(&mut **tx)
    .await
    .map_err(backend)?;
    Timestamp::from_millisecond(ms).map_err(stored_value)
}

pub(super) async fn renew_claims(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    from: &IdentityId,
    items: &[ItemKey],
    lease: Duration,
) -> Result<u64, StoreError> {
    let keys: Vec<&str> = items.iter().map(ItemKey::as_str).collect();
    let result = sqlx::query(
        "UPDATE claims SET lease_until = now() + $4 * interval '1 millisecond'
         WHERE project_id = $1 AND identity_id = $2 AND item_key = ANY($3)
           AND released_at IS NULL AND lease_until > now()",
    )
    .bind(project.as_str())
    .bind(from.as_str())
    .bind(&keys)
    .bind(lease_ms(lease))
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(result.rows_affected())
}

pub(super) async fn release_claim(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    item: &ItemKey,
    from: &IdentityId,
    by: agmen_core::ReleaseBy,
) -> Result<u64, StoreError> {
    let result = sqlx::query(
        "UPDATE claims SET released_at = now()
         WHERE project_id = $1 AND item_key = $2 AND released_at IS NULL
           AND (identity_id = $3 OR $4)",
    )
    .bind(project.as_str())
    .bind(item.as_str())
    .bind(from.as_str())
    .bind(by == agmen_core::ReleaseBy::Coordinator)
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(result.rows_affected())
}

pub(super) async fn next_item_number(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    prefix: &str,
) -> Result<u64, StoreError> {
    let n: i64 = sqlx::query_scalar(
        "INSERT INTO item_counters (project_id, prefix, n) VALUES ($1, $2, 1)
         ON CONFLICT (project_id, prefix) DO UPDATE SET n = item_counters.n + 1
         RETURNING n",
    )
    .bind(project.as_str())
    .bind(prefix)
    .fetch_one(&mut **tx)
    .await
    .map_err(backend)?;
    u64::try_from(n).map_err(stored_value)
}

pub(super) async fn insert_item(
    tx: &mut Transaction<'_, Postgres>,
    item: &agmen_core::NewItem<'_>,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO items (project_id, key, title, lane, severity, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(item.project.as_str())
    .bind(item.key.as_str())
    .bind(item.title)
    .bind(item.lane.map(LaneName::as_str))
    .bind(item.severity.map(severity_str))
    .bind(item.created_by.as_str())
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(())
}

pub(super) async fn assign_item(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    item: &ItemKey,
    to: &IdentityId,
) -> Result<u64, StoreError> {
    let result = sqlx::query(
        "UPDATE items SET assigned_to = $3
         WHERE project_id = $1 AND key = $2
           AND EXISTS (SELECT 1 FROM identities WHERE id = $3 AND project_id = $1)",
    )
    .bind(project.as_str())
    .bind(item.as_str())
    .bind(to.as_str())
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(result.rows_affected())
}

pub(super) async fn close_claim(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    item: &ItemKey,
    holder: &IdentityId,
) -> Result<u64, StoreError> {
    let result = sqlx::query(
        "UPDATE claims SET released_at = now()
         WHERE project_id = $1 AND item_key = $2 AND identity_id = $3
           AND released_at IS NULL AND lease_until > now()",
    )
    .bind(project.as_str())
    .bind(item.as_str())
    .bind(holder.as_str())
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(result.rows_affected())
}

pub(super) async fn mark_done(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    item: &ItemKey,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE items SET state = 'done' WHERE project_id = $1 AND key = $2")
        .bind(project.as_str())
        .bind(item.as_str())
        .execute(&mut **tx)
        .await
        .map_err(backend)?;
    Ok(())
}

pub(super) async fn lock_lane_owner(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    lane: &LaneName,
) -> Result<Option<Option<IdentityId>>, StoreError> {
    let row: Option<Option<String>> = sqlx::query_scalar(
        "SELECT owner_identity_id FROM lanes WHERE project_id=$1 AND name=$2 FOR UPDATE",
    )
    .bind(project.as_str())
    .bind(lane.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(backend)?;
    row.map(|owner| owner.map(|id| id.parse().map_err(stored_value)).transpose())
        .transpose()
}

pub(super) async fn identity_live(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    id: &IdentityId,
) -> Result<bool, StoreError> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM identities WHERE id=$1 AND project_id=$2 AND revoked_at IS NULL)").bind(id.as_str()).bind(project.as_str())
        .fetch_one(&mut **tx).await.map_err(backend)
}

pub(super) async fn identity_edits(
    tx: &mut Transaction<'_, Postgres>,
    id: &IdentityId,
) -> Result<Option<serde_json::Value>, StoreError> {
    let row: Option<Option<serde_json::Value>> =
        sqlx::query_scalar("SELECT edits FROM identities WHERE id=$1")
            .bind(id.as_str())
            .fetch_optional(&mut **tx)
            .await
            .map_err(backend)?;
    Ok(row.flatten())
}

pub(super) async fn active_lends(
    tx: &mut Transaction<'_, Postgres>,
    project: &ProjectId,
    lane: &LaneName,
    to: &IdentityId,
) -> Result<u64, StoreError> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lane_lends WHERE project_id=$1 AND lane=$2 AND to_id=$3 AND until_at > now()").bind(project.as_str()).bind(lane.as_str()).bind(to.as_str())
        .fetch_one(&mut **tx).await.map_err(backend)?;
    u64::try_from(count).map_err(stored_value)
}

pub(super) async fn insert_lend(
    tx: &mut Transaction<'_, Postgres>,
    lend: &agmen_core::NewLend<'_>,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO lane_lends (project_id, lane, message_id, from_id, to_id, paths, until_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7::timestamptz)",
    )
    .bind(lend.project.as_str())
    .bind(lend.lane.as_str())
    .bind(lend.message_id.as_str())
    .bind(lend.from.as_str())
    .bind(lend.to.as_str())
    .bind(lend.paths)
    .bind(lend.until.to_string())
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(())
}

fn lease_ms(lease: Duration) -> i64 {
    i64::try_from(lease.as_millis()).unwrap_or(i64::MAX)
}

const fn severity_str(severity: Severity) -> &'static str {
    match severity {
        Severity::Low => "low",
        Severity::Med => "med",
        Severity::High => "high",
        Severity::Security => "security",
    }
}

fn backend(err: sqlx::Error) -> StoreError {
    StoreError::Backend(Box::new(err))
}

fn stored_value(what: impl std::fmt::Display) -> StoreError {
    StoreError::Stored(format!("a stored value is invalid: {what}"))
}
