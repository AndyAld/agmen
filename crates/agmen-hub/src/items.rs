//! The bounded HTTP item list, including security-title filtering.

use agmen_proto::Severity;
use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;

use crate::auth::Caller;
use crate::http::{ApiError, Hub};
use crate::lanes::member_of;

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
