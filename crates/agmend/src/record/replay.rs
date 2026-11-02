//! Replay pages read on the blocking connection.

use agmen_core::{MAX_PAGE, Page, StoreError};
use agmen_proto::{ProjectId, StampedEnvelope, Stream};
use rusqlite::{Connection, OptionalExtension, params};

use super::{backend, messages::stored};

pub(super) fn page(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    stream: Stream,
    after: u64,
    limit: u32,
) -> Result<Page, StoreError> {
    let after = i64::try_from(after).map_err(stored)?;
    let mut stmt = conn
        .prepare(
            "SELECT envelope FROM messages
         WHERE project_id = ?1 AND stream = ?2 AND seq > ?3 ORDER BY seq LIMIT ?4",
        )
        .map_err(backend)?;
    let rows = stmt
        .query_map(
            params![
                project.as_str(),
                stream.to_string(),
                after,
                limit.min(MAX_PAGE)
            ],
            |row| row.get::<_, String>(0),
        )
        .map_err(backend)?;
    let messages = rows
        .take(MAX_PAGE as usize)
        .map(|row| serde_json::from_str::<StampedEnvelope>(&row.map_err(backend)?).map_err(stored))
        .collect::<Result<_, _>>()?;
    // Read the head after the messages so it cannot be behind the page.
    let (head, pruned_through) = conn
        .query_row(
            "SELECT seq, pruned_through FROM stream_heads WHERE stream = ?1",
            [format!("{project}/{stream}")],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(backend)?
        .unwrap_or((0, 0));
    Ok(Page::new(
        messages,
        u64::try_from(head).map_err(stored)?,
        u64::try_from(pruned_through).map_err(stored)?,
    ))
}
