//! Invite-only onboarding: a project admin or coordinator makes a one-use
//! code, and the person who accepts it becomes a member with the invite's
//! role. Admins invite any role, coordinators members only (ruling R58).

use std::ops::RangeInclusive;

use agmen_proto::{ProjectId, Timestamp, Topics};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::auth::Caller;
use crate::db::{Role, hash_secret, new_secret};
use crate::http::{ApiError, Hub};

/// How long an invite stays open unless the request says, in seconds (7
/// days).
const DEFAULT_TTL_SECS: i64 = 7 * 24 * 3600;
/// The shortest and longest `expires_in_secs`: 1 hour and 30 days.
const TTL_RANGE_SECS: RangeInclusive<i64> = 3600..=30 * 24 * 3600;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRequest {
    project: ProjectId,
    role: Role,
    /// How long the invite stays open, in seconds; [`DEFAULT_TTL_SECS`]
    /// when absent.
    expires_in_secs: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Created {
    /// The code, shown once; the hub keeps only its hash.
    code: String,
    link: url::Url,
    project: String,
    role: Role,
    expires_at: Timestamp,
}

/// `POST /v1/invites` (human token, project admin or coordinator).
pub(crate) async fn create(
    State(hub): State<Hub>,
    caller: Caller,
    Json(request): Json<CreateRequest>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    let human = caller.human()?;
    let ttl = request.expires_in_secs.unwrap_or(DEFAULT_TTL_SECS);
    if !TTL_RANGE_SECS.contains(&ttl) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_expiry",
            format!(
                "expires_in_secs must be from {} to {}",
                TTL_RANGE_SECS.start(),
                TTL_RANGE_SECS.end()
            ),
        ));
    }
    // The membership row stays share-locked until the commit. A member
    // removal deletes that row before it deletes the member's unused invites,
    // so it waits for this insert and then deletes the invite, or finishes
    // first and this lookup finds no row.
    let mut tx = hub.pool().begin().await?;
    let membership: Option<(String, String)> = sqlx::query_as(
        "SELECT m.role, p.tenant_id FROM memberships m JOIN projects p ON p.id = m.project_id
         WHERE m.human_id = $1 AND m.project_id = $2
         FOR SHARE OF m",
    )
    .bind(human.as_str())
    .bind(request.project.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((role, tenant)) = membership else {
        return Err(may_not_invite());
    };
    match (
        role.parse::<Role>().map_err(ApiError::stored)?,
        request.role,
    ) {
        (Role::Admin, _) | (Role::Coordinator, Role::Member) => {}
        (Role::Coordinator, _) => {
            return Err(ApiError::forbidden(
                "admin_required",
                "only a project admin can invite a coordinator or an admin",
            ));
        }
        (Role::Member, _) => return Err(may_not_invite()),
    }
    let code = new_secret()?;
    let expires_at: i64 = sqlx::query_scalar(
        "INSERT INTO invites (code_hash, tenant_id, project_id, role, created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6))
         RETURNING floor(extract(epoch FROM expires_at))::bigint",
    )
    .bind(hash_secret(&code))
    .bind(&tenant)
    .bind(request.project.as_str())
    .bind(request.role.as_str())
    .bind(human.as_str())
    .bind(ttl)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut link = hub.public_link("i");
    link.set_fragment(Some(&code));
    Ok((
        StatusCode::CREATED,
        Json(Created {
            code,
            link,
            project: request.project.into(),
            role: request.role,
            // Rounded down: the reported expiry is never past the stored one.
            expires_at: Timestamp::from_second(expires_at).map_err(ApiError::stored)?,
        }),
    ))
}

/// The answer for a caller who is not an admin or coordinator of the
/// project.
fn may_not_invite() -> ApiError {
    ApiError::forbidden(
        "coordinator_required",
        "only a project admin or coordinator can invite",
    )
}

#[derive(Debug, Serialize)]
pub(crate) struct Accepted {
    tenant: String,
    project: String,
    role: Role,
    name: String,
    repo_url: Option<agmen_proto::RepoUrl>,
}

/// `POST /v1/invites/{code}/accept` (human token): joins the project and
/// has its roster, which lists human members, published again. An invite
/// works once; a second accept gets 409.
pub(crate) async fn accept(
    State(hub): State<Hub>,
    Path(code): Path<String>,
    caller: Caller,
) -> Result<Json<Accepted>, ApiError> {
    #[derive(sqlx::FromRow)]
    struct Invite {
        tenant: String,
        project: String,
        role: String,
        used: bool,
        expired: bool,
        name: String,
        repo_url: Option<String>,
    }

    let human = caller.human()?;
    let hash = hash_secret(&code);
    let mut tx = hub.pool().begin().await?;
    let invite: Option<Invite> = sqlx::query_as(
        "SELECT i.tenant_id AS tenant, i.project_id AS project, i.role,
                i.used_by IS NOT NULL AS used, i.expires_at <= now() AS expired,
                p.name, p.repo_url
         FROM invites i JOIN projects p ON p.id = i.project_id
         WHERE i.code_hash = $1 FOR UPDATE OF i",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(Invite {
        tenant,
        project,
        role,
        used,
        expired,
        name,
        repo_url,
    }) = invite
    else {
        return Err(ApiError::not_found("no such invite"));
    };
    // Parsed before anything changes, so nothing after the commit can fail.
    let role: Role = role.parse().map_err(ApiError::stored)?;
    let repo_url = repo_url
        .map(agmen_proto::RepoUrl::try_from)
        .transpose()
        .map_err(ApiError::stored)?;
    let topics = Topics::new(
        tenant.parse().map_err(ApiError::stored)?,
        project.parse().map_err(ApiError::stored)?,
    );
    if used {
        return Err(ApiError::conflict(
            "invite_used",
            "the invite was already used",
        ));
    }
    if expired {
        return Err(ApiError::new(
            StatusCode::GONE,
            "invite_expired",
            "the invite has expired",
        ));
    }
    let joined = sqlx::query(
        "INSERT INTO memberships (human_id, project_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (human_id, project_id) DO NOTHING",
    )
    .bind(human.as_str())
    .bind(&project)
    .bind(role.as_str())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if joined == 0 {
        return Err(ApiError::conflict(
            "already_member",
            "you are already a member of that project",
        ));
    }
    sqlx::query("UPDATE invites SET used_by = $1, used_at = now() WHERE code_hash = $2")
        .bind(human.as_str())
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    crate::tenants::join_tenant(
        &mut tx,
        &tenant,
        human.as_str(),
        crate::db::TenantRole::Member,
    )
    .await?;
    tx.commit().await?;
    tracing::info!(human = %human, %project, role = role.as_str(), "invite accepted");
    hub.presence().roster_changed(&topics);
    Ok(Json(Accepted {
        tenant,
        project,
        role,
        name,
        repo_url,
    }))
}
