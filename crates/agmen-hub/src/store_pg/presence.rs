//! Presence roster and lease-expiry SQL, moved text-identical from the hub.

use std::collections::HashMap;

use agmen_core::{ExpiredClaim, RosterRow, StoreError};
use agmen_proto::ProjectId;
use sqlx::{PgExecutor, Postgres, Transaction};

type HelloSqlRow = (String, Option<String>, Option<String>);

type ExpiredSqlRow = (String, String, String, Option<String>, String, String, bool);

type RosterSqlRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Live identities union human members, ordered by handle.
pub(super) async fn roster_rows(
    db: impl PgExecutor<'_>,
    project: &ProjectId,
) -> Result<Vec<RosterRow>, StoreError> {
    let rows: Vec<RosterSqlRow> = sqlx::query_as(
        "SELECT id, kind, handle, role, agent, delegated_by FROM identities
             WHERE project_id = $1 AND revoked_at IS NULL
             UNION ALL
             SELECT h.id, 'human', h.handle, NULL::text, NULL::text, NULL::text FROM memberships m
             JOIN humans h ON h.id = m.human_id
             WHERE m.project_id = $1
             ORDER BY 3",
    )
    .bind(project.as_str())
    .fetch_all(db)
    .await
    .map_err(backend)?;
    Ok(rows
        .into_iter()
        .map(
            |(identity_id, kind, handle, role, agent, delegated_by)| RosterRow {
                identity_id,
                kind,
                handle,
                role,
                agent,
                delegated_by,
            },
        )
        .collect())
}

/// One row per identity that has posted a feed HELLO: its latest agent and model.
///
/// Uses the messages_latest_hello index (0005_presence). At most one row per
/// live identity that has said HELLO.
pub(super) async fn latest_hellos(
    db: impl PgExecutor<'_>,
    project: &ProjectId,
) -> Result<agmen_core::LatestHellos, StoreError> {
    let rows: Vec<HelloSqlRow> = sqlx::query_as(
        "SELECT DISTINCT ON (from_id)
                    from_id, envelope -> 'data' ->> 'agent', envelope -> 'data' ->> 'model'
             FROM messages
             WHERE project_id = $1 AND stream = 'feed' AND envelope ->> 'verb' = 'HELLO'
             ORDER BY from_id, seq DESC",
    )
    .bind(project.as_str())
    .fetch_all(db)
    .await
    .map_err(backend)?;
    let mut hellos = HashMap::with_capacity(rows.len());
    for (from, agent, model) in rows {
        let id = from
            .parse()
            .map_err(|err: agmen_proto::ProtoError| StoreError::Stored(err.to_string()))?;
        hellos.insert(id, (agent, model));
    }
    Ok(hellos)
}

/// Releases up to `limit` expired claims and returns the ones that join.
pub(super) async fn expire_claims(
    tx: &mut Transaction<'_, Postgres>,
    limit: u32,
) -> Result<Vec<ExpiredClaim>, StoreError> {
    let rows: Vec<ExpiredSqlRow> = sqlx::query_as(
        "UPDATE claims c SET released_at = now()
             FROM items i, identities h, projects p
             WHERE c.id IN (
                     SELECT id FROM claims
                     WHERE released_at IS NULL AND lease_until <= now()
                     ORDER BY lease_until LIMIT $1
                     FOR UPDATE SKIP LOCKED)
               AND i.project_id = c.project_id AND i.key = c.item_key
               AND h.id = c.identity_id AND p.id = c.project_id
             RETURNING p.tenant_id, c.project_id, c.item_key, i.lane, c.identity_id, h.handle,
                       i.state = 'open'",
    )
    .bind(i64::from(limit))
    .fetch_all(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(rows
        .into_iter()
        .map(
            |(tenant, project, item, lane, holder, handle, open)| ExpiredClaim {
                tenant,
                project,
                item,
                lane,
                holder,
                handle,
                open,
            },
        )
        .collect())
}

fn backend(err: sqlx::Error) -> StoreError {
    StoreError::Backend(Box::new(err))
}
