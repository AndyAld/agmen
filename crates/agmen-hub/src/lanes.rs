//! Lanes (design 6.5): `PUT` and `GET /v1/projects/{p}/lanes`, `POST
//! /v1/projects/{p}/check-path`, LEND, and the DONE check that flags changed
//! paths outside the sender's claimed lanes.
//!
//! Glob rules, overlap, and path judgment live in `agmen_core`. This module
//! keeps the HTTP handlers, the lane-file parser, and the ledger verbs that
//! still run in the hub's transaction.

use agmen_core::{
    Core, CoreError, EditRule, LaneList, LaneSpec, Lanes, Lends, MAX_ACTIVE_LENDS, MAX_CHECK_PATHS,
    MAX_LEND_DURATION, NOT_A_REPO_PATH, PathCheck, ReplaceError, StoreError, Stray, judge_scoped,
    repo_path, scope_globs, stray_body, stray_body_with,
};
use agmen_proto::{
    Body, Change, ClientEnvelope, EditScope, IdentityId, IdentityKind, ItemKey, NackReason, Needs,
    Payload, ProjectId, Stream, Timestamp, Topics, Urgency,
};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use serde::Deserialize;
use sqlx::{Postgres, Transaction};

use crate::auth::Caller;
use crate::claims::{LedgerError, LedgerMessage};
use crate::http::{ApiError, Hub};
use crate::identities::Identity;
use crate::ingest::{HubPost, post_alert};
use crate::items::close_claimed;
use crate::store_pg::{PgStore, PgTx};

/// A `PUT` body: the whole lane list, as `{"lanes": [...]}` in JSON or
/// `[[lanes]]` tables in TOML.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneFile {
    lanes: Vec<LaneSpec>,
}

/// Who called a project route, checked to belong to that project.
#[derive(Debug, Clone)]
pub(crate) enum Member {
    /// A human member of the project.
    Human,
    /// A live identity of the project.
    Identity(Identity),
}

/// The project `project` names, when `caller` belongs to it: a human with a
/// membership, or an identity of that project.
///
/// # Errors
///
/// 404 for a malformed project id; 403 `not_a_member` for a project the
/// caller does not belong to, which is also the answer for one that does
/// not exist.
pub(crate) async fn member_of(
    hub: &Hub,
    caller: Caller,
    project: &str,
) -> Result<(ProjectId, Member), ApiError> {
    let project: ProjectId = project
        .parse()
        .map_err(|_| ApiError::not_found("no such project"))?;
    let not_a_member =
        || ApiError::forbidden("not_a_member", "you are not a member of that project");
    match caller {
        Caller::Identity(identity) if identity.project == project => {
            Ok((project, Member::Identity(identity)))
        }
        Caller::Identity(_) => Err(not_a_member()),
        Caller::Human(human) => {
            let member: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM memberships WHERE human_id = $1 AND project_id = $2)",
            )
            .bind(human.as_str())
            .bind(project.as_str())
            .fetch_one(hub.pool())
            .await?;
            if member {
                Ok((project, Member::Human))
            } else {
                Err(not_a_member())
            }
        }
    }
}

fn unprocessable(code: &'static str, description: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, description)
}

fn core(hub: &Hub) -> Core<PgStore> {
    Core::new(PgStore::new(hub.pool().clone()), hub.claim_lease())
}

/// `GET /v1/projects/{p}/lanes` (any member).
pub(crate) async fn list(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
) -> Result<Json<LaneList>, ApiError> {
    let (project, _) = member_of(&hub, caller, &project).await?;
    let list = core(&hub).list_lanes(&project).await.map_err(store_error)?;
    Ok(Json(list))
}

/// `PUT /v1/projects/{p}/lanes` (a human member or a coordinator identity
/// of the project): replaces the project's lanes with the body's, a JSON
/// (`application/json`) or TOML (`application/toml`) lane list. A set whose
/// globs overlap across lanes gets 422 `overlapping_lanes`.
pub(crate) async fn replace(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
    headers: HeaderMap,
    body: String,
) -> Result<Json<LaneList>, ApiError> {
    let (project, member) = member_of(&hub, caller, &project).await?;
    let by = match &member {
        Member::Human => IdentityKind::Human,
        Member::Identity(identity) if identity.kind == IdentityKind::Coordinator => {
            IdentityKind::Coordinator
        }
        Member::Identity(_) => {
            return Err(ApiError::forbidden(
                "coordinator_required",
                "only humans and coordinators change lanes",
            ));
        }
    };
    let tenant: String = sqlx::query_scalar("SELECT tenant_id FROM projects WHERE id = $1")
        .bind(project.as_str())
        .fetch_one(hub.pool())
        .await?;
    let tenant = tenant.parse().map_err(ApiError::stored)?;
    if crate::usage::write_gate(&hub, &tenant).await? == crate::usage::Gate::ReadOnly {
        return Err(crate::usage::refuse_read_only());
    }
    let file = parse_lane_file(&headers, &body)?;
    let count = file.lanes.len();
    let list = core(&hub)
        .replace_lanes(&project, by, file.lanes)
        .await
        .map_err(replace_error)?;
    tracing::info!(%project, v = list.version(), lanes = count, "replaced lanes");
    Ok(Json(list))
}

/// The body as a lane list, by its content type.
fn parse_lane_file(headers: &HeaderMap, body: &str) -> Result<LaneFile, ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase());
    let bad = |err: &dyn std::fmt::Display| unprocessable("bad_lanes", err.to_string());
    match content_type.as_deref() {
        Some("application/json") => serde_json::from_str(body).map_err(|err| bad(&err)),
        Some("application/toml") => toml::from_str(body).map_err(|err| bad(&err)),
        _ => Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "send the lanes as application/json or application/toml",
        )),
    }
}

/// `POST /v1/projects/{p}/check-path` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckPath {
    identity: IdentityId,
    paths: Vec<String>,
}

/// `POST /v1/projects/{p}/check-path` (any member): whether `identity` may
/// edit each path. A path in no lane is allowed (`unowned`); a path in a
/// lane is allowed when `identity` holds a claim on an item of that lane.
pub(crate) async fn check_path(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
    Json(request): Json<CheckPath>,
) -> Result<Json<Vec<PathCheck>>, ApiError> {
    let (project, _) = member_of(&hub, caller, &project).await?;
    if request.paths.len() > MAX_CHECK_PATHS {
        return Err(unprocessable(
            "too_many_paths",
            format!("check at most {MAX_CHECK_PATHS} paths at once"),
        ));
    }
    match core(&hub)
        .check_path(&project, &request.identity, request.paths)
        .await
        .map_err(core_error)?
    {
        Some(checks) => Ok(Json(checks)),
        None => Err(ApiError::not_found(
            "no live identity with that id in this project",
        )),
    }
}

/// A LEND refusal is final; database errors remain retryable.
#[derive(Debug)]
pub(crate) enum LendError {
    Refused(NackReason),
    Database(sqlx::Error),
}

impl From<sqlx::Error> for LendError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

/// Validates and stores a LEND in the message transaction. Locking its lane
/// keeps replacement or ownership changes from racing the authorization.
pub(crate) async fn lend(
    tx: &mut Transaction<'_, Postgres>,
    topics: &Topics,
    sender: &IdentityId,
    kind: IdentityKind,
    env: &ClientEnvelope,
) -> Result<(), LendError> {
    let Payload::Lend {
        lane,
        paths,
        to,
        until,
    } = &env.payload
    else {
        return Err(LendError::Refused(NackReason::Schema));
    };
    let owner: Option<Option<String>> = sqlx::query_scalar(
        "SELECT owner_identity_id FROM lanes WHERE project_id=$1 AND name=$2 FOR UPDATE",
    )
    .bind(topics.project().as_str())
    .bind(lane.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let Some(owner) = owner else {
        return Err(LendError::Refused(NackReason::Schema));
    };
    if kind != IdentityKind::Coordinator && owner.as_deref() != Some(sender.as_str()) {
        return Err(LendError::Refused(NackReason::NotLaneOwner));
    }
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM identities WHERE id=$1 AND project_id=$2 AND revoked_at IS NULL)",
    ).bind(to.as_str()).bind(topics.project().as_str()).fetch_one(&mut **tx).await?;
    let now = Timestamp::now();
    if !live || *until <= now || *until > now + MAX_LEND_DURATION || scope_globs(paths).is_err() {
        return Err(LendError::Refused(NackReason::Schema));
    }
    // The lane row lock serializes this count with concurrent grants.
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lane_lends WHERE project_id=$1 AND lane=$2 AND to_id=$3 AND until_at > now()",
    ).bind(topics.project().as_str()).bind(lane.as_str()).bind(to.as_str())
        .fetch_one(&mut **tx).await?;
    if active >= MAX_ACTIVE_LENDS {
        return Err(LendError::Refused(NackReason::Schema));
    }
    sqlx::query(
        "INSERT INTO lane_lends (project_id, lane, message_id, from_id, to_id, paths, until_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7::timestamptz)",
    )
    .bind(topics.project().as_str())
    .bind(lane.as_str())
    .bind(env.id.as_str())
    .bind(sender.as_str())
    .bind(to.as_str())
    .bind(paths)
    .bind(until.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Checks a DONE ingest has just written in `tx`: closes the item and ends
/// the sender's claim on it when the sender holds it, and when a changed path lies in a lane the sender holds
/// no claim in (or is no repository path), posts an ALERT naming those
/// paths to the coordinators, or to the sender's owner when the project has
/// none (ruling R35).
pub(crate) async fn check_done(
    tx: &mut PgTx,
    topics: &Topics,
    msg: &LedgerMessage,
    item: &ItemKey,
    changes: &[Change],
) -> Result<(), LedgerError> {
    let project = topics.project().as_str();
    // The lanes held before this DONE ends its claim, which counts.
    let holders = crate::store_pg::lane_holders(&mut **tx.sqlx(), project).await?;
    close_claimed(tx.sqlx(), project, item, &msg.from).await?;
    let lanes = Lanes::compile(crate::store_pg::load_lanes(&mut **tx.sqlx(), project).await?)
        .map_err(|err| LedgerError::Stored(err.to_string()))?;
    let edits = load_edits(tx.sqlx(), msg).await?;
    let lends =
        Lends::compile(crate::store_pg::load_lends(&mut **tx.sqlx(), project, &msg.from).await?)
            .map_err(|err| LedgerError::Stored(err.to_string()))?;
    let strays = stray_changes(&lanes, &holders, msg, &edits, &lends, changes);
    if strays.is_empty() {
        return Ok(());
    }
    let body = stray_text(&msg.from_handle, item, &edits, &strays);
    let alert = HubPost {
        payload: Payload::Alert {
            need: format!(
                "{item}: DONE changed {} outside the claimed lanes",
                plural(strays.len(), "path")
            ),
            urgency: Urgency::Normal,
            needs: Needs::Anyone,
            actions: Vec::new(),
        },
        body: Body::try_from(body)?,
        reply_to: Some(msg.env.id.clone()),
        items: vec![item.clone()],
        lane: None,
        mentioned: Vec::new(),
        streams: vec![Stream::Feed],
    };
    let fallback = msg.from_owner.as_ref().unwrap_or(&msg.from);
    post_alert(tx, topics, alert, fallback).await?;
    Ok(())
}

async fn load_edits(
    tx: &mut Transaction<'_, Postgres>,
    msg: &LedgerMessage,
) -> Result<EditRule, LedgerError> {
    let edits: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT edits FROM identities WHERE id=$1")
            .bind(msg.from.as_str())
            .fetch_optional(&mut **tx)
            .await?
            .flatten();
    if msg.from_kind == IdentityKind::Listener {
        return Ok(EditRule::None);
    }
    EditRule::from_stored(edits).map_err(|err| LedgerError::Stored(err.to_string()))
}

fn stray_changes(
    lanes: &Lanes,
    holders: &[agmen_core::LaneHolder],
    msg: &LedgerMessage,
    edits: &EditRule,
    lends: &Lends,
    changes: &[Change],
) -> Vec<Stray> {
    changes
        .iter()
        .filter_map(|change| {
            let check = judge_scoped(lanes, holders, &msg.from, edits, lends, change.path.clone());
            (!check.allowed).then_some(Stray {
                path: check.path,
                lane: check.lane,
            })
        })
        .collect()
}

fn stray_text(handle: &str, item: &ItemKey, edits: &EditRule, strays: &[Stray]) -> String {
    if matches!(edits, EditRule::Lanes) {
        return stray_body(handle, item, strays);
    }
    stray_body_with(handle, item, strays, |stray| match &stray.lane {
        Some(lane) => lane.to_string(),
        None if repo_path(&stray.path).is_some() => "outside edit scope".to_owned(),
        None => NOT_A_REPO_PATH.to_owned(),
    })
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

pub(crate) fn validate_edits(edits: &EditScope) -> Result<(), ApiError> {
    agmen_core::validate_edits(edits).map_err(|err| unprocessable("bad_edits", err))
}

fn store_error(err: StoreError) -> ApiError {
    match err {
        StoreError::Backend(source) => match source.downcast::<sqlx::Error>() {
            Ok(err) => ApiError::Database(*err),
            Err(other) => ApiError::stored(std::io::Error::other(other.to_string())),
        },
        StoreError::Stored(message) => ApiError::stored(std::io::Error::other(message)),
        StoreError::Duplicate => ApiError::stored(std::io::Error::other("duplicate record")),
        other => ApiError::stored(std::io::Error::other(other.to_string())),
    }
}

fn core_error(err: CoreError) -> ApiError {
    match err {
        CoreError::Store(err) => store_error(err),
        CoreError::Stored(message) => ApiError::stored(std::io::Error::other(message)),
        other => ApiError::stored(std::io::Error::other(other.to_string())),
    }
}

fn replace_error(err: ReplaceError) -> ApiError {
    match err {
        ReplaceError::Spec(err) => unprocessable(err.code, err.description),
        ReplaceError::Overlapping(description) => unprocessable("overlapping_lanes", description),
        ReplaceError::TooComplex(description) => unprocessable("lanes_too_complex", description),
        ReplaceError::UnknownOwner(description) => unprocessable("unknown_owner", description),
        ReplaceError::Forbidden => ApiError::forbidden(
            "coordinator_required",
            "only humans and coordinators change lanes",
        ),
        ReplaceError::Store(err) => store_error(err),
        other => ApiError::stored(std::io::Error::other(other.to_string())),
    }
}

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod tests;
