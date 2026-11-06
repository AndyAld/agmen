//! Synchronous ledger SQL. The record wrappers run every call on the blocking pool.

use std::time::Duration;

use agmen_core::{
    ExpiredClaim, ItemLock, ItemRow, LaneHolder, LaneRow, Lapsed, MAX_GLOBS, MAX_GLOBS_PER_LANE,
    MAX_LANES, MAX_LISTED, MAX_LOADED_LENDS, QuotaTaken, ReleaseBy, ResolvedLane, StoreError,
    parse_kind,
};
use agmen_proto::{
    IdentityId, IdentityKind, ItemKey, LaneName, ProjectId, Severity, TenantId, Timestamp,
};
use rusqlite::{Connection, OptionalExtension, Params, Row, params};

use super::{OwnedItem, OwnedLend, backend, from_micros, micros};

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;

/// Matches the Postgres lane-holder cap and bounds stored-row walks.
const HOLDER_ROW_CAP: usize = 100_000;
const MAX_ITEM_NUMBER: i64 = 999_999;
const MICROS_PER_MILLISECOND: i64 = 1_000;

pub(super) fn items(
    conn: &Connection,
    now: i64,
    project: ProjectId,
) -> Result<Vec<ItemRow>, StoreError> {
    let mut listed: Vec<ItemRow> = rows(
        conn,
        "SELECT i.key, i.title, i.lane, i.severity, i.state, i.created_by, i.assigned_to,
                c.identity_id, h.handle, c.lease_until
         FROM items i
         LEFT JOIN claims c ON c.project_id=i.project_id AND c.item_key=i.key
              AND c.released_at IS NULL AND c.lease_until > ?2
         LEFT JOIN identities h ON h.id=c.identity_id
         WHERE i.project_id=?1 ORDER BY i.created_at, i.key LIMIT ?3",
        params![project.as_str(), now, MAX_LISTED],
        MAX_LISTED as usize,
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        },
    )?;
    for row in listed.iter_mut().take(MAX_LISTED as usize) {
        // Postgres casts epoch milliseconds to bigint, rounding half away from zero.
        row.9 = row.9.map(milliseconds);
    }
    Ok(listed)
}

pub(super) fn lane_globs(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
    let loaded: Vec<(String, String)> = rows(
        conn,
        "SELECT name, globs FROM lanes WHERE project_id=?1 ORDER BY name LIMIT ?2",
        params![project.as_str(), MAX_LANES as i64 + 1],
        MAX_LANES + 1,
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    check_lane_count(loaded.len())?;
    let mut total = 0_usize;
    let mut result = Vec::with_capacity(loaded.len());
    for (name, globs) in loaded.into_iter().take(MAX_LANES) {
        let globs: Vec<String> = serde_json::from_str(&globs).map_err(stored)?;
        if globs.len() > MAX_GLOBS_PER_LANE || total.saturating_add(globs.len()) > MAX_GLOBS {
            return Err(StoreError::Stored(
                "stored lane globs exceed the glob cap".into(),
            ));
        }
        total += globs.len();
        result.push((name.parse().map_err(stored)?, globs));
    }
    Ok(result)
}

pub(super) fn lane_holders(
    conn: &Connection,
    now: i64,
    project: ProjectId,
) -> Result<Vec<LaneHolder>, StoreError> {
    let loaded: Vec<(String, String, String)> = rows(
        conn,
        "SELECT i.lane, c.identity_id, h.handle FROM claims c
         JOIN items i ON i.project_id=c.project_id AND i.key=c.item_key
         JOIN identities h ON h.id=c.identity_id
         WHERE c.project_id=?1 AND c.released_at IS NULL AND c.lease_until > ?2
           AND i.lane IS NOT NULL
         ORDER BY c.granted_at, c.id LIMIT ?3",
        params![project.as_str(), now, HOLDER_ROW_CAP as i64 + 1],
        HOLDER_ROW_CAP + 1,
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if loaded.len() > HOLDER_ROW_CAP {
        return Err(StoreError::Stored(format!(
            "more than {HOLDER_ROW_CAP} live lane claims"
        )));
    }
    loaded
        .into_iter()
        .take(HOLDER_ROW_CAP)
        .map(|(lane, id, handle)| {
            Ok(LaneHolder {
                lane: lane.parse().map_err(stored)?,
                identity: id.parse().map_err(stored)?,
                handle,
            })
        })
        .collect()
}

pub(super) fn live_edit_scope(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    id: IdentityId,
) -> Result<Option<(IdentityKind, Option<serde_json::Value>)>, StoreError> {
    let row: Option<(String, Option<String>)> = conn.query_row(
        "SELECT kind, edits FROM identities WHERE id=?1 AND project_id=?2 AND revoked_at IS NULL",
        params![id.as_str(), project.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(backend)?;
    row.map(|(kind, edits)| {
        Ok((
            parse_kind(&kind)
                .map_err(|_| StoreError::Stored(format!("unknown identity kind {kind}")))?,
            edits
                .map(|json| serde_json::from_str(&json).map_err(stored))
                .transpose()?,
        ))
    })
    .transpose()
}

pub(super) fn lends_to(
    conn: &Connection,
    now: i64,
    project: ProjectId,
    to: IdentityId,
) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
    let loaded: Vec<(String, String)> = rows(
        conn,
        "SELECT g.lane, g.paths FROM lane_lends g
         JOIN lanes l ON l.project_id=g.project_id AND l.name=g.lane
         JOIN identities i ON i.id=g.from_id AND i.project_id=g.project_id
         WHERE g.project_id=?1 AND g.to_id=?2 AND g.until_at > ?3
           AND i.revoked_at IS NULL
           AND (l.owner_identity_id=g.from_id OR i.kind='coordinator')
         ORDER BY g.lane, g.until_at, g.message_id LIMIT ?4",
        params![project.as_str(), to.as_str(), now, MAX_LOADED_LENDS],
        MAX_LOADED_LENDS as usize,
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    loaded
        .into_iter()
        .take(MAX_LOADED_LENDS as usize)
        .map(|(lane, paths)| {
            Ok((
                lane.parse().map_err(stored)?,
                serde_json::from_str(&paths).map_err(stored)?,
            ))
        })
        .collect()
}

pub(super) fn lane_list(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
) -> Result<Vec<LaneRow>, StoreError> {
    let loaded: Vec<LaneRow> = rows(
        conn,
        "SELECT l.name, l.globs, l.owner_identity_id, i.handle, l.v
         FROM lanes l LEFT JOIN identities i ON i.id=l.owner_identity_id
         WHERE l.project_id=?1 ORDER BY l.name LIMIT ?2",
        params![project.as_str(), MAX_LANES as i64 + 1],
        MAX_LANES + 1,
        |row| {
            let globs: String = row.get(1)?;
            Ok((row.get(0)?, globs, row.get(2)?, row.get(3)?, row.get(4)?))
        },
    )?
    .into_iter()
    .take(MAX_LANES + 1)
    .map(|(name, globs, owner, owner_handle, v)| {
        Ok(LaneRow {
            name,
            globs: serde_json::from_str(&globs).map_err(stored)?,
            owner,
            owner_handle,
            v,
        })
    })
    .collect::<Result<_, StoreError>>()?;
    check_lane_count(loaded.len())?;
    for row in loaded.iter().take(MAX_LANES) {
        row.name.parse::<LaneName>().map_err(stored)?;
        if let Some(owner) = &row.owner {
            owner.parse::<IdentityId>().map_err(stored)?;
        }
    }
    Ok(loaded)
}

pub(super) fn take_quota(
    conn: &Connection,
    now: i64,
    tenant: TenantId,
    limit: u32,
) -> Result<QuotaTaken, StoreError> {
    if limit == 0 {
        return Ok(QuotaTaken::Exhausted);
    }
    let day = from_micros(now)?
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
        .to_string();
    let messages: Option<u32> = conn
        .query_row(
            "INSERT INTO tenant_usage(tenant_id, day, messages) VALUES (?1, ?2, 1)
         ON CONFLICT(tenant_id, day) DO UPDATE SET messages=tenant_usage.messages+1
         WHERE tenant_usage.messages < ?3 RETURNING messages",
            params![tenant.as_str(), day, i64::from(limit)],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend)?;
    let Some(messages) = messages else {
        return Ok(QuotaTaken::Exhausted);
    };
    let warned = conn
        .execute(
            "UPDATE tenant_usage SET warned=1 WHERE tenant_id=?1 AND day=?2
         AND warned=0 AND messages * 5 >= ?3 * 4",
            params![tenant.as_str(), day, i64::from(limit)],
        )
        .map_err(backend)?;
    Ok(QuotaTaken::Counted {
        messages,
        warn: warned != 0,
    })
}

pub(super) fn lock_item(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    item: ItemKey,
) -> Result<Option<ItemLock>, StoreError> {
    conn.query_row(
        "SELECT i.state, i.lane, i.assigned_to, l.owner_identity_id FROM items i
         LEFT JOIN lanes l ON l.project_id=i.project_id AND l.name=i.lane
         WHERE i.project_id=?1 AND i.key=?2",
        params![project.as_str(), item.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )
    .optional()
    .map_err(backend)
}

pub(super) fn release_lapsed(
    conn: &Connection,
    now: i64,
    project: ProjectId,
    item: ItemKey,
) -> Result<Option<Lapsed>, StoreError> {
    // BEGIN IMMEDIATE holds the write lock across this read and update.
    let row: Option<(i64, String, String)> = conn.query_row(
        "SELECT c.id, c.identity_id, h.handle FROM claims c JOIN identities h ON h.id=c.identity_id
         WHERE c.project_id=?1 AND c.item_key=?2 AND c.released_at IS NULL AND c.lease_until <= ?3",
        params![project.as_str(), item.as_str(), now],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional().map_err(backend)?;
    let Some((id, holder, handle)) = row else {
        return Ok(None);
    };
    let holder = holder.parse().map_err(stored)?;
    conn.execute(
        "UPDATE claims SET released_at=?1 WHERE id=?2",
        params![now, id],
    )
    .map_err(backend)?;
    Ok(Some(Lapsed { holder, handle }))
}

pub(super) fn active_claim(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    item: ItemKey,
) -> Result<Option<(IdentityId, String)>, StoreError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT c.identity_id, h.handle FROM claims c JOIN identities h ON h.id=c.identity_id
         WHERE c.project_id=?1 AND c.item_key=?2 AND c.released_at IS NULL",
            params![project.as_str(), item.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(backend)?;
    row.map(|(id, handle)| Ok((id.parse().map_err(stored)?, handle)))
        .transpose()
}

pub(super) fn extend_claim(
    conn: &Connection,
    now: i64,
    project: ProjectId,
    item: ItemKey,
    lease: Duration,
) -> Result<Timestamp, StoreError> {
    let until = lease_until(now, lease)?;
    let reply = millisecond_reply(until)?;
    conn.query_row(
        "UPDATE claims SET lease_until=?3 WHERE project_id=?1 AND item_key=?2
         AND released_at IS NULL RETURNING lease_until",
        params![project.as_str(), item.as_str(), until],
        |row| row.get::<_, i64>(0),
    )
    .map_err(backend)?;
    Ok(reply)
}

pub(super) fn insert_claim(
    conn: &Connection,
    now: i64,
    project: ProjectId,
    item: ItemKey,
    from: IdentityId,
    lease: Duration,
) -> Result<Timestamp, StoreError> {
    let until = lease_until(now, lease)?;
    let reply = millisecond_reply(until)?;
    conn.execute(
        "INSERT INTO claims(project_id, item_key, identity_id, granted_at, lease_until)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![project.as_str(), item.as_str(), from.as_str(), now, until],
    )
    .map_err(backend)?;
    Ok(reply)
}

pub(super) fn renew_claims(
    conn: &Connection,
    now: i64,
    project: ProjectId,
    from: IdentityId,
    items: Vec<ItemKey>,
    lease: Duration,
) -> Result<u64, StoreError> {
    let until = lease_until(now, lease)?;
    // A JSON table preserves SQL set membership, including duplicate keys.
    let keys = serde_json::to_string(&items).map_err(stored)?;
    let count = conn
        .execute(
            "UPDATE claims SET lease_until=?5 WHERE project_id=?1 AND identity_id=?2
         AND item_key IN (SELECT value FROM json_each(?3))
         AND released_at IS NULL AND lease_until > ?4",
            params![project.as_str(), from.as_str(), keys, now, until],
        )
        .map_err(backend)?;
    u64::try_from(count).map_err(stored)
}

pub(super) fn release_claim(
    conn: &Connection,
    now: i64,
    project: ProjectId,
    item: ItemKey,
    from: IdentityId,
    by: ReleaseBy,
) -> Result<u64, StoreError> {
    let count = conn
        .execute(
            "UPDATE claims SET released_at=?5 WHERE project_id=?1 AND item_key=?2
         AND released_at IS NULL AND (identity_id=?3 OR ?4)",
            params![
                project.as_str(),
                item.as_str(),
                from.as_str(),
                by == ReleaseBy::Coordinator,
                now
            ],
        )
        .map_err(backend)?;
    u64::try_from(count).map_err(stored)
}

pub(super) fn next_item_number(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    prefix: String,
) -> Result<u64, StoreError> {
    let n: Option<i64> = conn
        .query_row(
            "INSERT INTO item_counters(project_id, prefix, n) VALUES (?1, ?2, 1)
         ON CONFLICT(project_id, prefix) DO UPDATE SET n=item_counters.n+1
         WHERE item_counters.n < ?3 RETURNING n",
            params![project.as_str(), prefix, MAX_ITEM_NUMBER],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend)?;
    u64::try_from(n.ok_or_else(|| StoreError::Stored("item counter exhausted".into()))?)
        .map_err(stored)
}

pub(super) fn insert_item(conn: &Connection, now: i64, item: OwnedItem) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO items(project_id, key, title, lane, severity, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            item.project.as_str(),
            item.key.as_str(),
            item.title,
            item.lane.as_ref().map(LaneName::as_str),
            item.severity.map(severity_str),
            item.created_by.as_str(),
            now
        ],
    )
    .map_err(backend)?;
    Ok(())
}

pub(super) fn assign_item(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    item: ItemKey,
    to: IdentityId,
) -> Result<u64, StoreError> {
    let count = conn
        .execute(
            "UPDATE items SET assigned_to=?3 WHERE project_id=?1 AND key=?2
         AND EXISTS (SELECT 1 FROM identities WHERE id=?3 AND project_id=?1)",
            params![project.as_str(), item.as_str(), to.as_str()],
        )
        .map_err(backend)?;
    u64::try_from(count).map_err(stored)
}

pub(super) fn close_claim(
    conn: &Connection,
    now: i64,
    project: ProjectId,
    item: ItemKey,
    holder: IdentityId,
) -> Result<u64, StoreError> {
    let count = conn
        .execute(
            "UPDATE claims SET released_at=?4 WHERE project_id=?1 AND item_key=?2
         AND identity_id=?3 AND released_at IS NULL AND lease_until > ?4",
            params![project.as_str(), item.as_str(), holder.as_str(), now],
        )
        .map_err(backend)?;
    u64::try_from(count).map_err(stored)
}

pub(super) fn mark_done(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    item: ItemKey,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE items SET state='done' WHERE project_id=?1 AND key=?2",
        params![project.as_str(), item.as_str()],
    )
    .map_err(backend)?;
    Ok(())
}

pub(super) fn lock_lane_owner(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    lane: LaneName,
) -> Result<Option<Option<IdentityId>>, StoreError> {
    let owner: Option<Option<String>> = conn
        .query_row(
            "SELECT owner_identity_id FROM lanes WHERE project_id=?1 AND name=?2",
            params![project.as_str(), lane.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend)?;
    owner
        .map(|id| id.map(|id| id.parse().map_err(stored)).transpose())
        .transpose()
}

pub(super) fn identity_live(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    id: IdentityId,
) -> Result<bool, StoreError> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM identities WHERE id=?1 AND project_id=?2 AND revoked_at IS NULL)",
        params![id.as_str(), project.as_str()],
        |row| row.get(0),
    ).map_err(backend)
}

pub(super) fn identity_edits(
    conn: &Connection,
    _now: i64,
    id: IdentityId,
) -> Result<Option<serde_json::Value>, StoreError> {
    let edits: Option<Option<String>> = conn
        .query_row(
            "SELECT edits FROM identities WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend)?;
    edits
        .flatten()
        .map(|json| serde_json::from_str(&json).map_err(stored))
        .transpose()
}

pub(super) fn active_lends(
    conn: &Connection,
    now: i64,
    project: ProjectId,
    lane: LaneName,
    to: IdentityId,
) -> Result<u64, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM lane_lends WHERE project_id=?1 AND lane=?2 AND to_id=?3 AND until_at > ?4",
        params![project.as_str(), lane.as_str(), to.as_str(), now],
        |row| row.get(0),
    ).map_err(backend)?;
    u64::try_from(count).map_err(stored)
}

pub(super) fn insert_lend(conn: &Connection, _now: i64, lend: OwnedLend) -> Result<(), StoreError> {
    let paths = serde_json::to_string(&lend.paths).map_err(stored)?;
    conn.execute(
        "INSERT INTO lane_lends(project_id, lane, message_id, from_id, to_id, paths, until_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            lend.project.as_str(),
            lend.lane.as_str(),
            lend.message_id.as_str(),
            lend.from.as_str(),
            lend.to.as_str(),
            paths,
            micros(lend.until)
        ],
    )
    .map_err(backend)?;
    Ok(())
}

pub(super) fn replace_lanes(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    lanes: Vec<ResolvedLane>,
) -> Result<(), StoreError> {
    if lanes.len() > MAX_LANES {
        return Err(StoreError::Stored(format!(
            "a project has at most {MAX_LANES} lanes"
        )));
    }
    // BEGIN IMMEDIATE already serializes replacements before reading the version.
    let v: i64 = conn
        .query_row(
            "SELECT coalesce(max(v), 0)+1 FROM lanes WHERE project_id=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .map_err(backend)?;
    conn.execute("DELETE FROM lanes WHERE project_id=?1", [project.as_str()])
        .map_err(backend)?;
    let mut stmt = conn.prepare(
        "INSERT INTO lanes(project_id, name, globs, owner_identity_id, v) VALUES (?1, ?2, ?3, ?4, ?5)",
    ).map_err(backend)?;
    for lane in lanes.iter().take(MAX_LANES) {
        let globs = serde_json::to_string(&lane.globs).map_err(stored)?;
        stmt.execute(params![
            project.as_str(),
            lane.name.as_str(),
            globs,
            lane.owner.as_ref().map(IdentityId::as_str),
            v
        ])
        .map_err(backend)?;
    }
    Ok(())
}

pub(super) fn resolve_owner(
    conn: &Connection,
    _now: i64,
    project: ProjectId,
    handle: String,
) -> Result<Option<IdentityId>, StoreError> {
    let id: Option<String> = conn.query_row(
        "SELECT id FROM identities WHERE project_id=?1 AND revoked_at IS NULL AND (id=?2 OR handle=?2)",
        params![project.as_str(), handle], |row| row.get(0),
    ).optional().map_err(backend)?;
    id.map(|id| id.parse().map_err(stored)).transpose()
}

pub(super) fn expire_claims(
    conn: &Connection,
    now: i64,
    limit: u32,
) -> Result<Vec<ExpiredClaim>, StoreError> {
    // Keep the limit inside the subquery: a row that cannot join still uses a slot.
    // The writer lock stays held while the selected rows are decoded and released.
    let loaded: Vec<(i64, ExpiredClaim)> = rows(
        conn,
        "SELECT c.id, p.tenant_id, c.project_id, c.item_key, i.lane, c.identity_id, h.handle,
                i.state='open'
         FROM claims c JOIN items i ON i.project_id=c.project_id AND i.key=c.item_key
         JOIN identities h ON h.id=c.identity_id JOIN projects p ON p.id=c.project_id
         WHERE c.id IN (SELECT id FROM claims WHERE released_at IS NULL AND lease_until <= ?1
                        ORDER BY lease_until, id LIMIT ?2)
         ORDER BY c.lease_until, c.id",
        params![now, i64::from(limit)],
        usize::try_from(limit).map_err(stored)?,
        |row| {
            Ok((
                row.get(0)?,
                ExpiredClaim {
                    tenant: row.get(1)?,
                    project: row.get(2)?,
                    item: row.get(3)?,
                    lane: row.get(4)?,
                    holder: row.get(5)?,
                    handle: row.get(6)?,
                    open: row.get(7)?,
                },
            ))
        },
    )?;
    let mut expired = Vec::with_capacity(loaded.len());
    let mut stmt = conn
        .prepare("UPDATE claims SET released_at=?1 WHERE id=?2")
        .map_err(backend)?;
    for (id, row) in loaded.into_iter().take(limit as usize) {
        stmt.execute(params![now, id]).map_err(backend)?;
        expired.push(row);
    }
    Ok(expired)
}

fn rows<T>(
    conn: &Connection,
    sql: &str,
    params: impl Params,
    cap: usize,
    decode: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, StoreError> {
    let mut stmt = conn.prepare(sql).map_err(backend)?;
    stmt.query_map(params, decode)
        .map_err(backend)?
        .take(cap)
        .collect::<rusqlite::Result<_>>()
        .map_err(backend)
}

fn check_lane_count(len: usize) -> Result<(), StoreError> {
    if len > MAX_LANES {
        return Err(StoreError::Stored(format!(
            "stored lanes exceed {MAX_LANES}"
        )));
    }
    Ok(())
}

fn lease_until(now: i64, lease: Duration) -> Result<i64, StoreError> {
    // Matches the Postgres millisecond interval, retaining the clock's microseconds.
    let ms = i64::try_from(lease.as_millis()).unwrap_or(i64::MAX);
    let until = from_micros(now)?
        .checked_add(Duration::from_millis(ms.unsigned_abs()))
        .map_err(stored)?;
    Ok(micros(until))
}

fn milliseconds(at: i64) -> i64 {
    // i128 keeps rounding safe even at the endpoints of the stored integer range.
    let at = i128::from(at);
    ((at + at.signum() * i128::from(MICROS_PER_MILLISECOND / 2))
        / i128::from(MICROS_PER_MILLISECOND)) as i64
}

fn millisecond_reply(until: i64) -> Result<Timestamp, StoreError> {
    Timestamp::from_millisecond(milliseconds(until)).map_err(stored)
}

const fn severity_str(severity: Severity) -> &'static str {
    match severity {
        Severity::Low => "low",
        Severity::Med => "med",
        Severity::High => "high",
        Severity::Security => "security",
    }
}

fn stored(what: impl std::fmt::Display) -> StoreError {
    StoreError::Stored(format!("a stored value is invalid: {what}"))
}
