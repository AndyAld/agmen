//! Replay (protocol 5.3 and 6): `GET /v1/projects/{p}/streams/{s}`, how a
//! reader fills a gap in a stream or catches up after a reconnect.
//!
//! An identity reads its project's feed and its own inbox. A human member
//! reads the feed, its own inbox and the inboxes of the identities it owns
//! in that project. Everything else is 403, whether the stream exists or
//! not. The events route (`events.rs`) reads with the same rule
//! ([`human_streams`]) and the same page query ([`fetch_page`]).

use std::collections::BTreeSet;

use agmen_proto::{ProjectId, StampedEnvelope, Stream};
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::auth::Caller;
use crate::http::{ApiError, Hub};
use crate::identities::HumanId;
use crate::ingest::stream_key;

/// The most messages one page holds, and the page size when the reader
/// names none (protocol 5.3).
pub(crate) const MAX_PAGE: u32 = 500;

/// The query string: messages after seq `after` (default 0), at most
/// `limit` of them (default and cap [`MAX_PAGE`]).
#[derive(Debug, Deserialize)]
pub(crate) struct ReplayQuery {
    #[serde(default)]
    after: u64,
    limit: Option<u32>,
}

/// One page of a stream.
#[derive(Debug, Serialize)]
pub(crate) struct Page {
    /// The messages after `after`, in seq order.
    messages: Vec<StampedEnvelope>,
    /// The `after` for the next page; `None` once this page reaches `head`.
    next_after: Option<u64>,
    /// The stream's last seq; 0 for a stream with no messages.
    head: u64,
    /// The highest seq removed from this stream; 0 when none was pruned.
    pruned_through: u64,
}

fn invalid(description: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", description)
}

/// `GET /v1/projects/{p}/streams/{s}?after=&limit=` (any token): the
/// messages of stream `s` (`feed` or `inbox:<identity id>`) after `after`.
pub(crate) async fn stream(
    State(hub): State<Hub>,
    Path((project, stream)): Path<(String, String)>,
    caller: Caller,
    query: Result<Query<ReplayQuery>, QueryRejection>,
) -> Result<Json<Page>, ApiError> {
    let Query(query) = query.map_err(|err| invalid(err.body_text()))?;
    let project: ProjectId = project
        .parse()
        .map_err(|_| invalid("the project id is malformed"))?;
    let stream: Stream = stream
        .parse()
        .map_err(|_| invalid("the stream is neither feed nor inbox:<identity id>"))?;
    let limit = match query.limit {
        Some(0) => return Err(invalid("limit must be at least 1")),
        Some(limit) => limit.min(MAX_PAGE),
        None => MAX_PAGE,
    };
    let after = i64::try_from(query.after).map_err(|_| invalid("after is out of range"))?;
    if !may_read(&hub, &caller, &project, &stream).await? {
        return Err(ApiError::forbidden(
            "forbidden_stream",
            "you may not read that stream",
        ));
    }
    let messages = fetch_page(hub.pool(), &project, &stream, after, limit).await?;
    // Read after the messages: the head moves in the same transaction as
    // each insert, so it is never behind the page.
    let head: Option<(i64, i64)> =
        sqlx::query_as("SELECT seq, pruned_through FROM stream_heads WHERE stream = $1")
            .bind(stream_key(&project, &stream))
            .fetch_optional(hub.pool())
            .await?;
    let (head, pruned_through) = head.unwrap_or((0, 0));
    let head = u64::try_from(head).map_err(ApiError::stored)?;
    let pruned_through = u64::try_from(pruned_through).map_err(ApiError::stored)?;
    let next_after = messages
        .last()
        .map(|message| message.seq.0)
        .filter(|last| *last < head);
    Ok(Json(Page {
        messages,
        next_after,
        head,
        pruned_through,
    }))
}

/// The messages of `stream` in `project` after seq `after`, at most
/// `limit` of them, in seq order. Seqs commit in order with no gaps
/// (protocol 5.2), so the page is always the next run of the stream.
pub(crate) async fn fetch_page(
    pool: &sqlx::PgPool,
    project: &ProjectId,
    stream: &Stream,
    after: i64,
    limit: u32,
) -> Result<Vec<StampedEnvelope>, ApiError> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT envelope::text FROM messages
         WHERE project_id = $1 AND stream = $2 AND seq > $3
         ORDER BY seq LIMIT $4",
    )
    .bind(project.as_str())
    .bind(stream.to_string())
    .bind(after)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|json| serde_json::from_str::<StampedEnvelope>(json))
        .collect::<Result<Vec<_>, _>>()
        .map_err(ApiError::stored)
}

/// Whether `caller` may read `stream` in `project` (see the module docs).
async fn may_read(
    hub: &Hub,
    caller: &Caller,
    project: &ProjectId,
    stream: &Stream,
) -> Result<bool, ApiError> {
    match caller {
        Caller::Identity(identity) => Ok(&identity.project == project
            && match stream {
                Stream::Feed => true,
                Stream::Inbox(id) => *id == identity.id,
            }),
        Caller::Human(human) => Ok(human_streams(hub.pool(), human, project)
            .await?
            .is_some_and(|streams| streams.contains(stream))),
    }
}

/// The streams `human` may read in `project`: the feed, their own inbox and
/// the inbox of every identity they own there, revoked or not, so an owner
/// can still read what a revoked identity received. `None` when `human` is
/// not a member.
pub(crate) async fn human_streams(
    pool: &sqlx::PgPool,
    human: &HumanId,
    project: &ProjectId,
) -> Result<Option<BTreeSet<Stream>>, ApiError> {
    // One row when `human` is a member, holding the identities they own.
    let owned: Option<Vec<String>> = sqlx::query_scalar(
        "SELECT ARRAY(SELECT id FROM identities
                      WHERE project_id = $2 AND owner_human_id = $1 ORDER BY id)
         FROM memberships WHERE human_id = $1 AND project_id = $2",
    )
    .bind(human.as_str())
    .bind(project.as_str())
    .fetch_optional(pool)
    .await?;
    let Some(owned) = owned else {
        return Ok(None);
    };
    let mut streams = BTreeSet::from([Stream::Feed]);
    for id in std::iter::once(human.as_str()).chain(owned.iter().map(String::as_str)) {
        streams.insert(Stream::Inbox(id.parse().map_err(ApiError::stored)?));
    }
    Ok(Some(streams))
}
