//! Message copies, publish outbox and message-rule lookups, with the original SQL.

use agmen_core::{Member, MessageRow, Parent, QuotaTaken, Roster, Sender, StoreError, parse_kind};
use agmen_proto::{
    IdentityId, IdentityKind, ItemKey, MessageId, ProjectId, Seq, Stream, TenantId, Topics,
};
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

pub(crate) async fn project_exists(
    pool: impl PgExecutor<'_>,
    topics: &Topics,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1 AND tenant_id = $2)")
        .bind(topics.project().as_str())
        .bind(topics.tenant().as_str())
        .fetch_one(pool)
        .await
}

type SenderRow = (String, String, String, bool, Option<Vec<String>>);

pub(super) async fn sender(
    db: &mut sqlx::PgConnection,
    topics: &Topics,
    from: &IdentityId,
) -> Result<Option<Sender>, StoreError> {
    let identity: Option<SenderRow> = sqlx::query_as(
        "SELECT kind, handle, owner_human_id, revoked_at IS NOT NULL, verbs
         FROM identities WHERE id = $1 AND tenant_id = $2 AND project_id = $3",
    )
    .bind(from.as_str())
    .bind(topics.tenant().as_str())
    .bind(topics.project().as_str())
    .fetch_optional(&mut *db)
    .await
    .map_err(backend)?;
    if let Some((kind, handle, owner, revoked, verbs)) = identity {
        if revoked {
            return Ok(None);
        }
        return Ok(Some(Sender {
            id: from.clone(),
            kind: parse_kind(&kind).map_err(stored)?,
            handle,
            owner: Some(parse_id(&owner)?),
            verbs,
        }));
    }
    let human: Option<String> = sqlx::query_scalar(
        "SELECT h.handle FROM humans h
         JOIN memberships m ON m.human_id = h.id
         JOIN projects p ON p.id = m.project_id
         WHERE h.id = $1 AND p.tenant_id = $2 AND p.id = $3",
    )
    .bind(from.as_str())
    .bind(topics.tenant().as_str())
    .bind(topics.project().as_str())
    .fetch_optional(&mut *db)
    .await
    .map_err(backend)?;
    Ok(human.map(|handle| Sender {
        id: from.clone(),
        kind: IdentityKind::Human,
        handle,
        owner: None,
        verbs: None,
    }))
}

pub(super) async fn parent(
    db: &mut sqlx::PgConnection,
    project: &ProjectId,
    sender: &IdentityId,
    reply_to: &MessageId,
) -> Result<Option<Parent>, StoreError> {
    let readable = [
        Stream::Feed.to_string(),
        Stream::Inbox(sender.clone()).to_string(),
    ];
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT from_id, envelope->>'from_kind', envelope->>'verb' FROM messages
         WHERE project_id = $1 AND id = $2 AND stream = ANY($3)
         ORDER BY hub_at, stream, seq LIMIT 1",
    )
    .bind(project.as_str())
    .bind(reply_to.as_str())
    .bind(&readable[..])
    .fetch_optional(&mut *db)
    .await
    .map_err(backend)?;
    let Some((from, kind, verb)) = row else {
        return Ok(None);
    };
    let streams: Vec<String> = sqlx::query_scalar(
        "SELECT stream FROM messages WHERE project_id = $1 AND from_id = $2 AND id = $3",
    )
    .bind(project.as_str())
    .bind(&from)
    .bind(reply_to.as_str())
    .fetch_all(&mut *db)
    .await
    .map_err(backend)?;
    Ok(Some(Parent {
        from: parse_id(&from)?,
        from_kind: parse_kind(&kind).map_err(stored)?,
        verb,
        streams: streams
            .iter()
            .map(|stream| stream.parse().map_err(stored))
            .collect::<Result<_, _>>()?,
    }))
}

pub(super) async fn recorded(
    db: impl PgExecutor<'_>,
    from: &IdentityId,
    id: &MessageId,
) -> Result<Option<(Stream, Seq, Vec<u8>)>, StoreError> {
    let row: Option<(String, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT stream, seq, payload_hash FROM messages
         WHERE from_id = $1 AND id = $2 AND ack_row",
    )
    .bind(from.as_str())
    .bind(id.as_str())
    .fetch_optional(db)
    .await
    .map_err(backend)?;
    let Some((stream, seq, recorded_hash)) = row else {
        return Ok(None);
    };
    Ok(Some((
        stream.parse().map_err(stored)?,
        Seq(u64::try_from(seq).map_err(stored)?),
        recorded_hash,
    )))
}

pub(super) async fn is_delegate(
    db: impl PgExecutor<'_>,
    project: &ProjectId,
    worker: &IdentityId,
    delegator: &IdentityId,
) -> Result<bool, StoreError> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM identities WHERE id=$1 AND project_id=$2
                 AND delegated_by=$3 AND revoked_at IS NULL)",
    )
    .bind(worker.as_str())
    .bind(project.as_str())
    .bind(delegator.as_str())
    .fetch_one(db)
    .await
    .map_err(backend)
}

/// Takes the usage row before stream heads, counting only if `tx` commits.
pub(super) async fn take_quota(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &TenantId,
    limit: u32,
) -> Result<QuotaTaken, StoreError> {
    if limit == 0 {
        return Ok(QuotaTaken::Exhausted);
    }
    let messages: Option<i32> = sqlx::query_scalar(
        "INSERT INTO tenant_usage (tenant_id, day, messages)
         VALUES ($1, (now() AT TIME ZONE 'UTC')::date, 1)
         ON CONFLICT (tenant_id, day) DO UPDATE
         SET messages = tenant_usage.messages + 1
         WHERE tenant_usage.messages < $2
         RETURNING messages",
    )
    .bind(tenant.as_str())
    .bind(i64::from(limit))
    .fetch_optional(&mut **tx)
    .await
    .map_err(backend)?;
    let Some(messages) = messages else {
        return Ok(QuotaTaken::Exhausted);
    };
    let messages =
        u32::try_from(messages).map_err(|err| backend(sqlx::Error::Decode(Box::new(err))))?;
    // The upsert holds this row until commit, including through this update.
    // Bigint arithmetic keeps the threshold safe for every u32 limit.
    let warn: Option<bool> = sqlx::query_scalar(
        "UPDATE tenant_usage SET warned = true
         WHERE tenant_id = $1 AND day = (now() AT TIME ZONE 'UTC')::date
         AND NOT warned AND messages::bigint * 5 >= $2::bigint * 4
         RETURNING warned",
    )
    .bind(tenant.as_str())
    .bind(i64::from(limit))
    .fetch_optional(&mut **tx)
    .await
    .map_err(backend)?;
    Ok(QuotaTaken::Counted {
        messages,
        warn: warn.is_some(),
    })
}
