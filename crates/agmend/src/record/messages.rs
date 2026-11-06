//! Message copies and message-rule lookups on the blocking connection.

use agmen_core::{Member, MemberRole, Parent, Roster, Sender, StoreError, parse_kind};
use agmen_proto::{IdentityId, IdentityKind, ItemKey, MessageId, ProjectId, Seq, Stream, Topics};
use rusqlite::{Connection, OptionalExtension, params};

use super::{OwnedMessageRow, backend, micros};

// Match the in-memory backend's finite read budget. Fail rather than return
// an incomplete roster, reference set or set of private thread streams.
pub(super) const MAX_READ_ROWS: usize = 100_000;

#[cfg(test)]
#[path = "messages_tests.rs"]
mod tests;

// Without the hint SQLite scans every message of the project through the
// covering unique index, because the planner has no statistics.
pub(super) const PARENT_STREAMS_SQL: &str = "SELECT stream FROM messages INDEXED BY messages_by_id
     WHERE project_id = ?1 AND from_id = ?2 AND id = ?3";

pub(super) fn project_exists(
    conn: &Connection,
    _now: i64,
    topics: Topics,
) -> Result<bool, StoreError> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM projects WHERE id = ?1 AND tenant_id = ?2)",
        params![topics.project().as_str(), topics.tenant().as_str()],
        |row| row.get(0),
    )
    .map_err(backend)
}

pub(super) fn sender(
    conn: &Connection,
    _now: i64,
    topics: Topics,
    from: IdentityId,
) -> Result<Option<Sender>, StoreError> {
    let identity = conn
        .query_row(
            "SELECT kind, handle, owner_human_id, revoked_at IS NOT NULL, verbs
             FROM identities WHERE id = ?1 AND tenant_id = ?2 AND project_id = ?3",
            params![
                from.as_str(),
                topics.tenant().as_str(),
                topics.project().as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(backend)?;
    if let Some((kind, handle, owner, revoked, verbs)) = identity {
        if revoked {
            return Ok(None);
        }
        return Ok(Some(Sender {
            id: from,
            kind: parse_kind(&kind)?,
            handle,
            owner: Some(owner.parse().map_err(stored)?),
            verbs: verbs
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(stored)?,
        }));
    }
    let handle = conn
        .query_row(
            "SELECT h.handle FROM humans h
             JOIN memberships m ON m.human_id = h.id
             JOIN projects p ON p.id = m.project_id
             WHERE h.id = ?1 AND p.tenant_id = ?2 AND p.id = ?3",
            params![
                from.as_str(),
                topics.tenant().as_str(),
                topics.project().as_str()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(backend)?;
    Ok(handle.map(|handle| Sender {
        id: from,
        kind: IdentityKind::Human,
        handle,
        owner: None,
        verbs: None,
    }))
}

pub(super) fn parent(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    sender: IdentityId,
    reply_to: MessageId,
) -> Result<Option<Parent>, StoreError> {
    let row = conn
        .query_row(
            "SELECT from_id, json_extract(envelope,'$.from_kind'), json_extract(envelope,'$.verb')
             FROM messages WHERE project_id = ?1 AND id = ?2 AND stream IN ('feed', ?3)
             ORDER BY hub_at, stream, seq LIMIT 1",
            params![
                project.as_str(),
                reply_to.as_str(),
                Stream::Inbox(sender).to_string()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(backend)?;
    let Some((from, kind, verb)) = row else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(PARENT_STREAMS_SQL).map_err(backend)?;
    let rows = stmt
        .query_map(params![project.as_str(), from, reply_to.as_str()], |row| {
            row.get::<_, String>(0)
        })
        .map_err(backend)?;
    let streams = read_rows(rows)?
        .into_iter()
        .take(MAX_READ_ROWS)
        .map(|stream| stream.parse().map_err(stored))
        .collect::<Result<_, _>>()?;
    Ok(Some(Parent {
        from: from.parse().map_err(stored)?,
        from_kind: parse_kind(&kind)?,
        verb,
        streams,
    }))
}

pub(super) fn is_delegate(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    worker: IdentityId,
    delegator: IdentityId,
) -> Result<bool, StoreError> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM identities WHERE id=?1 AND project_id=?2
         AND delegated_by=?3 AND revoked_at IS NULL)",
        params![worker.as_str(), project.as_str(), delegator.as_str()],
        |row| row.get(0),
    )
    .map_err(backend)
}

pub(super) fn recorded(
    conn: &Connection,
    _now: i64,
    from: IdentityId,
    id: MessageId,
) -> Result<Option<(Stream, Seq, Vec<u8>)>, StoreError> {
    let row = conn
        .query_row(
            "SELECT stream, seq, payload_hash FROM messages
         WHERE from_id = ?1 AND id = ?2 AND ack_row=1",
            params![from.as_str(), id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(backend)?;
    row.map(|(stream, seq, hash)| {
        Ok((
            stream.parse().map_err(stored)?,
            Seq(u64::try_from(seq).map_err(stored)?),
            hash,
        ))
    })
    .transpose()
}

pub(super) fn roster(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
) -> Result<Roster, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, handle, owner_human_id, NULL, role FROM identities
         WHERE project_id = ?1 AND revoked_at IS NULL
         UNION ALL
         SELECT m.human_id, 'human', h.handle, NULL, m.role, NULL
         FROM memberships m JOIN humans h ON h.id=m.human_id WHERE m.project_id = ?1",
        )
        .map_err(backend)?;
    let rows = stmt
        .query_map([project.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(backend)?;
    read_rows(rows)?
        .into_iter()
        .take(MAX_READ_ROWS)
        .map(|(id, kind, handle, owner, role, identity_role)| {
            let role = role
                .as_deref()
                .map(|role| match role {
                    "admin" => Ok(MemberRole::Admin),
                    "coordinator" => Ok(MemberRole::Coordinator),
                    "member" => Ok(MemberRole::Member),
                    other => Err(stored(format!("unknown membership role {other:?}"))),
                })
                .transpose()?;
            Ok(Member {
                id: id.parse().map_err(stored)?,
                handle,
                kind: parse_kind(&kind)?,
                owner: owner
                    .as_deref()
                    .map(str::parse)
                    .transpose()
                    .map_err(stored)?,
                role,
                identity_role,
            })
        })
        .collect::<Result<_, _>>()
        .map(Roster)
}

pub(super) fn ref_targets(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    candidates: Vec<String>,
) -> Result<(Vec<String>, Vec<ItemKey>), StoreError> {
    if candidates.len() > MAX_READ_ROWS {
        return Err(stored("too many reference candidates"));
    }
    let mut lanes = conn
        .prepare("SELECT name FROM lanes WHERE project_id=?1 ORDER BY name")
        .map_err(backend)?;
    let rows = lanes
        .query_map([project.as_str()], |row| row.get::<_, String>(0))
        .map_err(backend)?;
    let lanes = read_rows(rows)?;
    // One JSON parameter keeps the query within SQLite's bind parameter limit.
    let candidates = serde_json::to_string(&candidates).map_err(stored)?;
    let mut items = conn
        .prepare(
            "SELECT key FROM items WHERE project_id=?1
         AND key IN (SELECT value FROM json_each(?2)) ORDER BY key",
        )
        .map_err(backend)?;
    let rows = items
        .query_map(params![project.as_str(), candidates], |row| {
            row.get::<_, String>(0)
        })
        .map_err(backend)?;
    let keys = read_rows(rows)?
        .into_iter()
        .take(MAX_READ_ROWS)
        .map(|key| key.parse().map_err(stored))
        .collect::<Result<_, _>>()?;
    Ok((lanes, keys))
}

pub(super) fn insert_message(
    conn: &Connection,
    _now: i64,
    row: OwnedMessageRow,
) -> Result<(), StoreError> {
    let inserted = conn
        .execute(
            "INSERT INTO messages
         (project_id, stream, seq, id, from_id, payload_hash, envelope, hub_at, ack_row)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(project_id, stream, from_id, id) DO NOTHING
         ON CONFLICT(from_id, id) WHERE ack_row=1 DO NOTHING",
            params![
                row.project.as_str(),
                row.stream.to_string(),
                i64::try_from(row.seq.0).map_err(stored)?,
                row.id.as_str(),
                row.from.as_str(),
                row.payload_hash,
                row.envelope_json,
                micros(row.hub_at),
                row.ack_row
            ],
        )
        .map_err(backend)?;
    if inserted == 0 {
        return Err(StoreError::Duplicate);
    }
    Ok(())
}

/// Solo mode has no broker. Replies go to the caller; copies are read back by page.
pub(super) fn enqueue_publish(
    _conn: &Connection,
    _now: i64,
    _topics: Topics,
    _topic: String,
    _payload: Vec<u8>,
) -> Result<(), StoreError> {
    Ok(())
}

pub(super) fn stored(what: impl std::fmt::Display) -> StoreError {
    StoreError::Stored(format!("a stored value is invalid: {what}"))
}

pub(super) fn read_rows<T>(
    rows: impl Iterator<Item = rusqlite::Result<T>>,
) -> Result<Vec<T>, StoreError> {
    let rows = rows
        .take(MAX_READ_ROWS + 1)
        .collect::<Result<Vec<_>, _>>()
        .map_err(backend)?;
    if rows.len() > MAX_READ_ROWS {
        return Err(stored(format!("record read exceeds {MAX_READ_ROWS} rows")));
    }
    Ok(rows)
}
