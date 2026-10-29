//! Message copies, publish outbox and message-rule lookups, with the original SQL.

use agmen_core::{Member, MessageRow, Roster, StoreError, parse_kind};
use agmen_proto::{IdentityId, ItemKey, ProjectId, Topics};
use sqlx::{PgExecutor, Postgres, Transaction};

use crate::db::{Role, is_unique_violation};

type MemberRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// A project's live members, with membership and mint roles from storage.
pub(super) async fn roster(
    db: impl PgExecutor<'_>,
    project: &ProjectId,
) -> Result<Roster, StoreError> {
    let rows: Vec<MemberRow> = sqlx::query_as(
        "SELECT id, kind, handle, owner_human_id, NULL::text, role FROM identities
             WHERE project_id = $1 AND revoked_at IS NULL
             UNION ALL
             SELECT m.human_id, 'human', h.handle, NULL, m.role, NULL::text
             FROM memberships m JOIN humans h ON h.id=m.human_id WHERE m.project_id = $1",
    )
    .bind(project.as_str())
    .fetch_all(db)
    .await
    .map_err(backend)?;
    rows.into_iter()
        .map(|(id, kind, handle, owner, role, identity_role)| {
            Ok(Member {
                id: parse_id(&id)?,
                handle,
                kind: parse_kind(&kind).map_err(stored)?,
                identity_role,
                owner: owner.as_deref().map(parse_id).transpose()?,
                role: role
                    .as_deref()
                    .map(str::parse::<Role>)
                    .transpose()
                    .map_err(stored)?
                    .map(Into::into),
            })
        })
        .collect::<Result<_, StoreError>>()
        .map(Roster)
}

pub(super) async fn ref_targets(
    db: &mut sqlx::PgConnection,
    project: &ProjectId,
    candidates: &[String],
) -> Result<(Vec<String>, Vec<ItemKey>), StoreError> {
    let lanes: Vec<String> =
        sqlx::query_scalar("SELECT name FROM lanes WHERE project_id=$1 ORDER BY name")
            .bind(project.as_str())
            .fetch_all(&mut *db)
            .await
            .map_err(backend)?;
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM items WHERE project_id=$1 AND key = ANY($2) ORDER BY key",
    )
    .bind(project.as_str())
    .bind(candidates)
    .fetch_all(&mut *db)
    .await
    .map_err(backend)?;
    let keys = keys
        .iter()
        .map(|key| key.parse().map_err(stored))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((lanes, keys))
}

pub(super) async fn insert_message(
    tx: &mut Transaction<'_, Postgres>,
    row: &MessageRow<'_>,
) -> Result<(), StoreError> {
    let inserted = sqlx::query(
        "INSERT INTO messages
                 (project_id, stream, seq, id, from_id, payload_hash, envelope, hub_at, ack_row)
             VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, $8::timestamptz, $9)",
    )
    .bind(row.project.as_str())
    .bind(row.stream.to_string())
    .bind(i64::try_from(row.seq.0).map_err(stored)?)
    .bind(row.id.as_str())
    .bind(row.from.as_str())
    .bind(row.payload_hash)
    .bind(row.envelope_json)
    .bind(row.hub_at.to_string())
    .bind(row.ack_row)
    .execute(&mut **tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(err) if is_unique_violation(&err) => return Err(StoreError::Duplicate),
        Err(err) => return Err(backend(err)),
    }
    Ok(())
}
pub(super) async fn enqueue_publish(
    tx: &mut Transaction<'_, Postgres>,
    topics: &Topics,
    topic: &str,
    payload: &[u8],
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO publish_outbox (tenant_id, project_id, topic, payload)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(topics.tenant().as_str())
    .bind(topics.project().as_str())
    .bind(topic)
    .bind(payload)
    .execute(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(())
}

fn stored(what: impl std::fmt::Display) -> StoreError {
    StoreError::Stored(format!("a stored value is invalid: {what}"))
}

fn parse_id(id: &str) -> Result<IdentityId, StoreError> {
    id.parse().map_err(stored)
}

fn backend(err: sqlx::Error) -> StoreError {
    StoreError::Backend(Box::new(err))
}
