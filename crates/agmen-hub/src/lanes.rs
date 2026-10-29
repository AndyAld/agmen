//! Lanes (design 6.5): `PUT` and `GET /v1/projects/{p}/lanes`, `POST
//! /v1/projects/{p}/check-path`, LEND, and the DONE check that flags changed
//! paths outside the sender's claimed lanes.
//!
//! Glob rules, overlap, and path judgment live in `agmen_core`. This module
//! keeps the HTTP handlers and lane-file parser.

use agmen_core::{
    Core, CoreError, LaneList, LaneSpec, MAX_CHECK_PATHS, PathCheck, ReplaceError, StoreError,
};
use agmen_proto::{EditScope, IdentityId, IdentityKind, ProjectId};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use serde::Deserialize;

use crate::auth::Caller;
use crate::http::{ApiError, Hub};
use crate::identities::Identity;
use crate::store_pg::PgStore;

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
