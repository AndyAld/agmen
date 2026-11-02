//! Presence roster and latest feed HELLOs on the blocking connection.

use agmen_core::{LatestHellos, RosterRow, StoreError};
use agmen_proto::ProjectId;
use rusqlite::Connection;

use super::{
    backend,
    messages::{MAX_READ_ROWS, read_rows, stored},
};

// Keep the partial-index predicates identical to messages_latest_hello.
pub(super) const LATEST_HELLOS_SQL: &str = "
    SELECT from_id, agent, model FROM (
        SELECT from_id, json_extract(envelope,'$.data.agent') AS agent,
               json_extract(envelope,'$.data.model') AS model,
               row_number() OVER (PARTITION BY from_id ORDER BY seq DESC) AS rank
        FROM messages
        WHERE project_id = ?1 AND stream = 'feed' AND json_extract(envelope,'$.verb') = 'HELLO'
    ) WHERE rank = 1";

/// Live identities union human members, ordered by handle, then id as the
/// in-memory backend does.
pub(super) fn roster_rows(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
) -> Result<Vec<RosterRow>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, handle, role, agent, delegated_by FROM identities
         WHERE project_id = ?1 AND revoked_at IS NULL
         UNION ALL
         SELECT h.id, 'human', h.handle, NULL, NULL, NULL FROM memberships m
         JOIN humans h ON h.id = m.human_id WHERE m.project_id = ?1 ORDER BY 3, 1",
        )
        .map_err(backend)?;
    let rows = stmt
        .query_map([project.as_str()], |row| {
            Ok(RosterRow {
                identity_id: row.get(0)?,
                kind: row.get(1)?,
                handle: row.get(2)?,
                role: row.get(3)?,
                agent: row.get(4)?,
                delegated_by: row.get(5)?,
            })
        })
        .map_err(backend)?;
    read_rows(rows)
}

/// The latest agent and model per sender of a feed HELLO, using the partial index.
pub(super) fn latest_hellos(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
) -> Result<LatestHellos, StoreError> {
    let mut stmt = conn.prepare(LATEST_HELLOS_SQL).map_err(backend)?;
    let rows = stmt
        .query_map([project.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(backend)?;
    read_rows(rows)?
        .into_iter()
        .take(MAX_READ_ROWS)
        .map(|(from, agent, model)| Ok((from.parse().map_err(stored)?, (agent, model))))
        .collect()
}
