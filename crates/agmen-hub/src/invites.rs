//! Invite-only onboarding: a project admin makes a one-use code, and the
//! person who accepts it becomes a member with the invite's role.

use agmen_proto::{ProjectId, Timestamp};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::auth::Caller;
use crate::db::{Role, hash_secret, new_secret};
use crate::http::{ApiError, Hub};

/// How long an invite stays open, in seconds (7 days).
const INVITE_TTL_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRequest {
    project: ProjectId,
    role: Role,
}

#[derive(Debug, Serialize)]
pub(crate) struct Created {
    /// The code, shown once; the hub keeps only its hash.
    code: String,
    project: String,
    role: Role,
    expires_at: Timestamp,
}

/// `POST /v1/invites` (human token, project admin).
pub(crate) async fn create(
    State(hub): State<Hub>,
    caller: Caller,
    Json(request): Json<CreateRequest>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    let human = caller.human()?;
    let membership: Option<(String, String)> = sqlx::query_as(
        "SELECT m.role, p.tenant_id FROM memberships m JOIN projects p ON p.id = m.project_id
         WHERE m.human_id = $1 AND m.project_id = $2",
    )
    .bind(human.as_str())
    .bind(request.project.as_str())
    .fetch_optional(hub.pool())
    .await?;
    let tenant = match membership {
        Some((role, tenant)) if role.parse::<Role>().map_err(ApiError::stored)? == Role::Admin => {
            tenant
        }
        _ => {
            return Err(ApiError::forbidden(
                "admin_required",
                "only a project admin can invite",
            ));
        }
    };
    let code = new_secret()?;
    let expires_at: i64 = sqlx::query_scalar(
        "INSERT INTO invites (code_hash, tenant_id, project_id, role, created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6))
         RETURNING extract(epoch FROM expires_at)::bigint",
    )
    .bind(hash_secret(&code))
    .bind(&tenant)
    .bind(request.project.as_str())
    .bind(request.role.as_str())
    .bind(human.as_str())
    .bind(INVITE_TTL_SECS)
    .fetch_one(hub.pool())
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(Created {
            code,
            project: request.project.into(),
            role: request.role,
            expires_at: Timestamp::from_second(expires_at).map_err(ApiError::stored)?,
        }),
    ))
}

#[derive(Debug, Serialize)]
pub(crate) struct Accepted {
    tenant: String,
    project: String,
    role: Role,
}

/// `POST /v1/invites/{code}/accept` (human token): joins the project. An
/// invite works once; a second accept gets 409.
pub(crate) async fn accept(
    State(hub): State<Hub>,
    Path(code): Path<String>,
    caller: Caller,
) -> Result<Json<Accepted>, ApiError> {
    let human = caller.human()?;
    let hash = hash_secret(&code);
    let mut tx = hub.pool().begin().await?;
    let invite: Option<(String, String, String, bool, bool)> = sqlx::query_as(
        "SELECT tenant_id, project_id, role, used_by IS NOT NULL, expires_at <= now()
         FROM invites WHERE code_hash = $1 FOR UPDATE",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((tenant, project, role, used, expired)) = invite else {
        return Err(ApiError::not_found("no such invite"));
    };
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
    .bind(&role)
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
    tx.commit().await?;
    tracing::info!(human = %human, %project, %role, "invite accepted");
    Ok(Json(Accepted {
        tenant,
        project,
        role: role.parse().map_err(ApiError::stored)?,
    }))
}
