//! Replay and item-list reads with the original SQL.

use agmen_core::{ItemRow, MAX_LISTED, Page, StoreError};
use agmen_proto::{ProjectId, StampedEnvelope, Stream};
use sqlx::{PgConnection, PgExecutor};

use crate::ingest::stream_key;

pub(super) async fn page(
    db: &mut PgConnection,
    project: &ProjectId,
    stream: &Stream,
    after: u64,
    limit: u32,
) -> Result<Page, StoreError> {
    let after = i64::try_from(after).map_err(stored)?;
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT envelope::text FROM messages
         WHERE project_id = $1 AND stream = $2 AND seq > $3
         ORDER BY seq LIMIT $4",
    )
    .bind(project.as_str())
    .bind(stream.to_string())
    .bind(after)
    .bind(i64::from(limit))
    .fetch_all(&mut *db)
    .await
    .map_err(backend)?;
    let messages = rows
        .iter()
        .map(|json| serde_json::from_str::<StampedEnvelope>(json))
        .collect::<Result<Vec<_>, _>>()
        .map_err(stored)?;
    // Read after the messages: the head moves in the same transaction as
    // each insert, so it is never behind the page.
    let head: Option<(i64, i64)> =
        sqlx::query_as("SELECT seq, pruned_through FROM stream_heads WHERE stream = $1")
            .bind(stream_key(project, stream))
            .fetch_optional(db)
            .await
            .map_err(backend)?;
    let (head, pruned_through) = head.unwrap_or((0, 0));
    let head = u64::try_from(head).map_err(stored)?;
    let pruned_through = u64::try_from(pruned_through).map_err(stored)?;
    Ok(Page::new(messages, head, pruned_through))
}

pub(super) async fn items(
    db: impl PgExecutor<'_>,
    project: &ProjectId,
) -> Result<Vec<ItemRow>, StoreError> {
    sqlx::query_as(
        "SELECT i.key, i.title, i.lane, i.severity, i.state, i.created_by, i.assigned_to,
                c.identity_id, h.handle, (extract(epoch FROM c.lease_until) * 1000)::bigint
         FROM items i
         LEFT JOIN claims c ON c.project_id = i.project_id AND c.item_key = i.key
              AND c.released_at IS NULL AND c.lease_until > now()
         LEFT JOIN identities h ON h.id = c.identity_id
         WHERE i.project_id = $1
         ORDER BY i.created_at, i.key
         LIMIT $2",
    )
    .bind(project.as_str())
    .bind(MAX_LISTED)
    .fetch_all(db)
    .await
    .map_err(backend)
}

fn backend(err: sqlx::Error) -> StoreError {
    StoreError::Backend(Box::new(err))
}

fn stored(err: impl std::error::Error + Send + Sync + 'static) -> StoreError {
    StoreError::Backend(Box::new(err))
}
