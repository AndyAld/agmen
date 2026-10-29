//! Items (design 6.5): FOUND mints the next key and the hub answers with
//! ITEM, ASSIGN records the assignee, DONE from the holder closes an item,
//! and `GET /v1/projects/{p}/items` lists them.

use agmen_proto::{Body, IdentityId, ItemKey, LaneName, Payload, Severity, Stream, Topics};
use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;
use sqlx::PgConnection;

use crate::auth::Caller;
use crate::claims::{LedgerError, LedgerMessage};
use crate::http::{ApiError, Hub};
use crate::ingest::{HubPost, post_in};
use crate::lanes::member_of;
use crate::store_pg::PgTx;

/// The prefix of the keys FOUND mints: B for bug, as in `B-412`.
const FOUND_PREFIX: &str = "B";

/// The most items one `GET` lists.
///
/// shortcut: one page of the oldest 1,000; page with `after=` once a
/// project has more.
const MAX_LISTED: i64 = 1000;

/// A severity as the `items` table spells it.
const fn severity_str(severity: Severity) -> &'static str {
    match severity {
        Severity::Low => "low",
        Severity::Med => "med",
        Severity::High => "high",
        Severity::Security => "security",
    }
}

/// FOUND: mints the next `B-<n>` key, stores the item, and answers in `tx`
/// with ITEM `{key}` in the feed and the finder's inbox. The ITEM carries
/// only the key and lane, so a security finding's title stays in the
/// finder's inboxes (protocol 4).
pub(crate) async fn found(
    tx: &mut PgTx,
    topics: &Topics,
    msg: &LedgerMessage,
    title: &str,
    lane: Option<&LaneName>,
    severity: Option<Severity>,
) -> Result<(), LedgerError> {
    let project = topics.project().as_str();
    let n: i64 = sqlx::query_scalar(
        "INSERT INTO item_counters (project_id, prefix, n) VALUES ($1, $2, 1)
         ON CONFLICT (project_id, prefix) DO UPDATE SET n = item_counters.n + 1
         RETURNING n",
    )
    .bind(project)
    .bind(FOUND_PREFIX)
    .fetch_one(&mut **tx.sqlx())
    .await?;
    let key: ItemKey = format!("{FOUND_PREFIX}-{n}").parse()?;
    sqlx::query(
        "INSERT INTO items (project_id, key, title, lane, severity, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(project)
    .bind(key.as_str())
    .bind(title)
    .bind(lane.map(LaneName::as_str))
    .bind(severity.map(severity_str))
    .bind(msg.from.as_str())
    .execute(&mut **tx.sqlx())
    .await?;
    let body = match lane {
        Some(lane) => format!("{key} ({lane})"),
        None => key.to_string(),
    };
    post_in(
        tx,
        topics,
        HubPost {
            payload: Payload::Item { key: key.clone() },
            body: Body::try_from(body)?,
            reply_to: Some(msg.env.id.clone()),
            items: vec![key],
            lane: lane.cloned(),
            mentioned: Vec::new(),
            streams: vec![Stream::Feed, Stream::Inbox(msg.from.clone())],
        },
    )
    .await?;
    Ok(())
}

/// ASSIGN: records `to` as `item`'s assignee, which lets it claim the item
/// in a lane another identity owns. Ingest already routed the ASSIGN to
/// the assignee's inbox; an unknown item, or an assignee that is a human,
/// records nothing.
pub(crate) async fn assign(
    conn: &mut PgConnection,
    project: &str,
    item: &ItemKey,
    to: &IdentityId,
) -> Result<(), LedgerError> {
    let assigned = sqlx::query(
        "UPDATE items SET assigned_to = $3
         WHERE project_id = $1 AND key = $2
           AND EXISTS (SELECT 1 FROM identities WHERE id = $3 AND project_id = $1)",
    )
    .bind(project)
    .bind(item.as_str())
    .bind(to.as_str())
    .execute(conn)
    .await?;
    if assigned.rows_affected() == 0 {
        tracing::debug!(%item, %to, "ASSIGN recorded nothing");
    }
    Ok(())
}

/// Ends `holder`'s live claim on `item` and moves the item to done, which
/// closes it to new claims. A claim is live by the test
/// [`crate::store_pg::lane_holders`] uses: not released and its lease not
/// over. Without one, nothing changes.
pub(crate) async fn close_claimed(
    conn: &mut PgConnection,
    project: &str,
    item: &ItemKey,
    holder: &IdentityId,
) -> Result<(), LedgerError> {
    let released = sqlx::query(
        "UPDATE claims SET released_at = now()
         WHERE project_id = $1 AND item_key = $2 AND identity_id = $3
           AND released_at IS NULL AND lease_until > now()",
    )
    .bind(project)
    .bind(item.as_str())
    .bind(holder.as_str())
    .execute(&mut *conn)
    .await?;
    if released.rows_affected() == 0 {
        tracing::debug!(%item, %holder, "DONE from an identity without a live claim");
        return Ok(());
    }
    sqlx::query("UPDATE items SET state = 'done' WHERE project_id = $1 AND key = $2")
        .bind(project)
        .bind(item.as_str())
        .execute(conn)
        .await?;
    Ok(())
}

/// One item as `GET /v1/projects/{p}/items` lists it. A security item's
/// title is left out.
#[derive(Debug, Serialize)]
pub(crate) struct ItemView {
    key: String,
    title: Option<String>,
    lane: Option<String>,
    severity: Option<String>,
    state: String,
    created_by: String,
    assigned_to: Option<String>,
    /// The identity with the live claim, its handle, and when the lease
    /// ends.
    holder: Option<String>,
    holder_handle: Option<String>,
    lease_until: Option<agmen_proto::Timestamp>,
}

/// The `GET` answer.
#[derive(Debug, Serialize)]
pub(crate) struct ItemList {
    items: Vec<ItemView>,
}

type ItemRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);

/// `GET /v1/projects/{p}/items` (any member): the oldest
/// [`MAX_LISTED`] items, with who holds each.
pub(crate) async fn list(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
) -> Result<Json<ItemList>, ApiError> {
    let (project, _) = member_of(&hub, caller, &project).await?;
    let rows: Vec<ItemRow> = sqlx::query_as(
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
    .fetch_all(hub.pool())
    .await?;
    let items = rows
        .into_iter()
        .map(
            |(
                key,
                title,
                lane,
                severity,
                state,
                created_by,
                assigned_to,
                holder,
                handle,
                until,
            )| {
                let secret = severity.as_deref() == Some(severity_str(Severity::Security));
                Ok(ItemView {
                    key,
                    title: (!secret).then_some(title),
                    lane,
                    severity,
                    state,
                    created_by,
                    assigned_to,
                    holder,
                    holder_handle: handle,
                    lease_until: until
                        .map(agmen_proto::Timestamp::from_millisecond)
                        .transpose()
                        .map_err(ApiError::stored)?,
                })
            },
        )
        .collect::<Result<_, ApiError>>()?;
    Ok(Json(ItemList { items }))
}
