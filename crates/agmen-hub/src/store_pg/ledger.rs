//! Lane SQL. The statements are the ones the hub used before the rules moved
//! into `agmen-core`.

use agmen_core::{
    LaneHolder, LaneRow, MAX_GLOBS as GLOB_CAP, MAX_GLOBS_PER_LANE as GLOBS_PER_LANE_CAP,
    MAX_LANES as LANE_ROW_CAP, MAX_LOADED_LENDS, ResolvedLane, parse_kind,
};
use agmen_proto::{IdentityId, IdentityKind, LaneName, ProjectId};
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
