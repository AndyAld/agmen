//! `GET /v1/projects/{p}/state?key=roster|head` (protocol section 6).
//! `roster` answers the full array in `value` and, for that key only, sets
//! `ETag` to the head's etag. The retained `state/roster` topic holds only
//! that head (`{count, online, etag}`). `head` answers the JSON the hub
//! publishes retained on `state/head`.
//!
//! The answer is `{key, as_of_seq, value}`. `value` is built by the code
//! that publishes the topic: [`Hub::roster`] for `roster`
//! ([`crate::roster`]) and [`state_head`] for `head` (sweeper.rs). For
//! `roster`, `value` is the full array, not the retained head. The topics
//! are published when something changes; this builds the value at request
//! time, so it is at least as fresh as the topic.
//!
//! `as_of_seq` is the feed seq the value reflects. For `head` it is the
//! payload's own `feed_seq`. For `roster` it is the feed's head read just
//! before the roster is built, so every HELLO at or below it is in the
//! roster (a later one may be too). The roster's statuses come from the
//! presence this hub process has read from the broker since it connected,
//! not from the feed, so `as_of_seq` does not order them; after a hub
//! restart they fill in as the broker resends retained presence.
//!
//! Any member may read it: a human with an access token, or an identity
//! with its own token, of the project (protocol section 2 lets both
//! subscribe to `state/`). Any other `key` gets 400 `invalid_request`:
//! `lanes`, `round` and `packs` are not served yet.

use agmen_proto::Topics;
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::Caller;
use crate::http::{ApiError, Hub};
use crate::lanes::member_of;
use crate::roster::{RosterError, roster_head};
use crate::sweeper::state_head;

/// The query string: the `state/` key.
#[derive(Debug, Deserialize)]
pub(crate) struct StateQuery {
    key: Option<String>,
}

/// The answer: the key, the feed seq the value reflects, and the value.
#[derive(Debug, Serialize)]
struct Snapshot {
    key: &'static str,
    as_of_seq: u64,
    value: Value,
}

/// `GET /v1/projects/{p}/state?key=` (any member; see the module docs).
pub(crate) async fn get(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
    query: Result<Query<StateQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let (project, _) = member_of(&hub, caller, &project).await?;
    let invalid = |description: String| {
        ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", description)
    };
    let Query(query) = query.map_err(|err| invalid(err.body_text()))?;
    let key = match query.key.as_deref() {
        Some("roster") => "roster",
        Some("head") => "head",
        _ => return Err(invalid("key must be roster or head".to_owned())),
    };
    let tenant: String = sqlx::query_scalar("SELECT tenant_id FROM projects WHERE id = $1")
        .bind(project.as_str())
        .fetch_optional(hub.pool())
        .await?
        .ok_or_else(|| ApiError::not_found("no such project"))?;
    let topics = Topics::new(
        tenant.parse().map_err(ApiError::stored)?,
        project.as_str().parse().map_err(ApiError::stored)?,
    );
    let (snapshot, etag) = if key == "head" {
        let head = state_head(&hub, &topics).await?;
        (
            Snapshot {
                key,
                as_of_seq: head.feed_seq,
                value: serde_json::to_value(&head).map_err(ApiError::stored)?,
            },
            None,
        )
    } else {
        // The head first: the roster built next reflects at least it.
        let as_of_seq = state_head(&hub, &topics).await?.feed_seq;
        let roster = hub.roster(&topics).await.map_err(|err| match err {
            RosterError::Database(err) => ApiError::Database(err),
            err @ RosterError::Stored(_) => ApiError::stored(err),
        })?;
        // The same array this response returns, so the header matches the
        // retained head once that publish uses this roster.
        let head = roster_head(&roster);
        let etag =
            HeaderValue::from_str(&format!("\"{}\"", head.etag)).map_err(ApiError::stored)?;
        (
            Snapshot {
                key,
                as_of_seq,
                value: serde_json::to_value(&roster).map_err(ApiError::stored)?,
            },
            Some(etag),
        )
    };
    let mut response = Json(snapshot).into_response();
    if let Some(etag) = etag {
        response.headers_mut().insert(header::ETAG, etag);
    }
    Ok(response)
}
