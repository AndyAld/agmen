//! Membership and rights in a tenant, shared by its projects.

use std::collections::BTreeSet;

use agmen_proto::{Timestamp, Topics};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::{Executor, PgConnection, PgPool, Postgres, Transaction};

use crate::auth::Caller;
use crate::db::TenantRole;
use crate::http::{ApiError, Hub};
use crate::identities::{HumanId, ban_and_kick, ban_until};

/// The human's role in this tenant, if they belong to it.
pub(crate) async fn tenant_role<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    tenant: &str,
    human: &HumanId,
) -> Result<Option<TenantRole>, ApiError> {
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM tenant_members WHERE tenant_id = $1 AND human_id = $2",
    )
    .bind(tenant)
    .bind(human.as_str())
    .fetch_optional(executor)
    .await?;
    role.map(|role| role.parse().map_err(ApiError::stored))
        .transpose()
}

/// Joins a tenant, preserving existing rights except a member-to-admin raise.
pub(crate) async fn join_tenant(
    conn: &mut PgConnection,
    tenant: &str,
    human: &str,
    role: TenantRole,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO tenant_members (tenant_id, human_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (tenant_id, human_id) DO UPDATE SET role = EXCLUDED.role
         WHERE tenant_members.role = 'member' AND EXCLUDED.role = 'admin'",
    )
    .bind(tenant)
    .bind(human)
    .bind(role.as_str())
    .execute(conn)
    .await?;
    Ok(())
}

/// Requires the rights shared by group admins and the owner.
pub(crate) fn require_group_admin(role: Option<TenantRole>) -> Result<TenantRole, ApiError> {
    match role {
        Some(role @ (TenantRole::Owner | TenantRole::Admin)) => Ok(role),
        Some(TenantRole::Member) | None => Err(ApiError::forbidden(
            "group_admin_required",
            "only a group admin or owner can do this",
        )),
    }
}

// shortcut: lists stop at 1000 members and 1000 identities per member; add paging when groups reach either cap.
const LIST_LIMIT: i64 = 1000;

#[derive(Debug, Serialize)]
pub(crate) struct Members {
    members: Vec<Member>,
}

#[derive(Debug, Serialize)]
struct Member {
    human_id: String,
    handle: String,
    role: TenantRole,
    projects: Vec<MemberProject>,
    identities: Vec<MemberIdentity>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct MemberProject {
    project: String,
    name: String,
    role: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct MemberIdentity {
    id: String,
    project: String,
    handle: String,
}

/// `GET /v1/tenants/{tenant}/members`, for any human member of the group.
pub(crate) async fn list(
    State(hub): State<Hub>,
    Path(tenant): Path<String>,
    caller: Caller,
) -> Result<Json<Members>, ApiError> {
    let human = caller.human()?;
    if tenant_role(hub.pool(), &tenant, &human).await?.is_none() {
        return Err(ApiError::forbidden(
            "not_a_group_member",
            "you are not a member of that group",
        ));
    }
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT h.id, h.handle, m.role FROM tenant_members m JOIN humans h ON h.id = m.human_id
         WHERE m.tenant_id = $1 ORDER BY h.handle LIMIT $2",
    )
    .bind(&tenant)
    .bind(LIST_LIMIT)
    .fetch_all(hub.pool())
    .await?;
    let mut members = Vec::with_capacity(rows.len());
    for (human_id, handle, role) in rows.into_iter().take(LIST_LIMIT as usize) {
        let projects = sqlx::query_as(
            "SELECT p.id AS project, p.name, m.role FROM memberships m
             JOIN projects p ON p.id = m.project_id
             WHERE p.tenant_id = $1 AND m.human_id = $2 ORDER BY p.id",
        )
        .bind(&tenant)
        .bind(&human_id)
        .fetch_all(hub.pool())
        .await?;
        let identities = sqlx::query_as(
            "SELECT id, project_id AS project, handle FROM identities
             WHERE tenant_id = $1 AND owner_human_id = $2 AND revoked_at IS NULL
             ORDER BY project_id, handle LIMIT $3",
        )
        .bind(&tenant)
        .bind(&human_id)
        .bind(LIST_LIMIT)
        .fetch_all(hub.pool())
        .await?;
        members.push(Member {
            human_id,
            handle,
            role: role.parse().map_err(ApiError::stored)?,
            projects,
            identities,
        });
    }
    Ok(Json(Members { members }))
}

// Serialize membership mutations before reading permissions. In particular, a
// transfer must not race a removal or role change against the previous owner.
async fn lock_tenant<'a>(
    pool: &'a PgPool,
    tenant: &str,
) -> Result<Transaction<'a, Postgres>, ApiError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM tenants WHERE id = $1 FOR UPDATE")
        .bind(tenant)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

fn no_such_member() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "no_such_member",
        "no such group member",
    )
}

/// `DELETE /v1/tenants/{tenant}/members/{handle}`. Commit every database
/// change before trying all bans, kicks and roster notifications.
pub(crate) async fn remove(
    State(hub): State<Hub>,
    Path((tenant, handle)): Path<(String, String)>,
    caller: Caller,
) -> Result<StatusCode, ApiError> {
    let human = caller.human()?;
    let mut tx = lock_tenant(hub.pool(), &tenant).await?;
    let actor = require_group_admin(tenant_role(&mut *tx, &tenant, &human).await?)?;
    let target: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT h.id, m.role FROM humans h LEFT JOIN tenant_members m
         ON m.human_id = h.id AND m.tenant_id = $1 WHERE h.handle = $2",
    )
    .bind(&tenant)
    .bind(&handle)
    .fetch_optional(&mut *tx)
    .await?;
    let (target, role) = target.ok_or_else(no_such_member)?;
    let role = role
        .map(|role| role.parse::<TenantRole>().map_err(ApiError::stored))
        .transpose()?;
    if role == Some(TenantRole::Owner) {
        return Err(ApiError::conflict(
            "owner_cannot_be_removed",
            "transfer ownership before removing the owner",
        ));
    }
    if actor == TenantRole::Admin && role == Some(TenantRole::Admin) && target != human.as_str() {
        return Err(ApiError::forbidden(
            "owner_required",
            "only the owner can remove another admin",
        ));
    }
    // Delete the memberships first: this is the lock that serializes against
    // `identities::mint`, which share-locks the row it checks.
    let mut projects: BTreeSet<String> = sqlx::query_scalar(
        "DELETE FROM memberships m USING projects p
         WHERE m.project_id = p.id AND p.tenant_id = $1 AND m.human_id = $2
         RETURNING m.project_id",
    )
    .bind(&tenant)
    .bind(&target)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    // Revoke only after the membership delete above: a mint in flight holds a
    // share lock on that row, so the delete waits for it to commit and this
    // statement then sees the identity it inserted.
    sqlx::query(
        "UPDATE identities SET revoked_at = now()
         WHERE tenant_id = $1 AND owner_human_id = $2 AND revoked_at IS NULL",
    )
    .bind(&tenant)
    .bind(&target)
    .execute(&mut *tx)
    .await?;
    let mut identities: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT id, project_id, extract(epoch FROM revoked_at)::bigint FROM identities
         WHERE tenant_id = $1 AND owner_human_id = $2 ORDER BY id",
    )
    .bind(&tenant)
    .bind(&target)
    .fetch_all(&mut *tx)
    .await?;
    // Use the same token lifetime and skew margin as single-identity revoke.
    let now = Timestamp::now();
    identities.retain(|(_, _, at)| ban_until(*at) > now);
    if role.is_none() && identities.is_empty() {
        return Err(no_such_member());
    }
    sqlx::query("DELETE FROM invites WHERE created_by = $1 AND tenant_id = $2 AND used_by IS NULL")
        .bind(&target)
        .bind(&tenant)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM tenant_members WHERE tenant_id = $1 AND human_id = $2")
        .bind(&tenant)
        .bind(&target)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let mut failed = None;
    // These snapshots bound both loops; retries never extend them with new rows.
    let identity_count = identities.len();
    for (id, project, revoked_at) in identities.into_iter().take(identity_count) {
        if let Some(err) = ban_and_kick(&hub, &id, ban_until(revoked_at)).await {
            failed.get_or_insert(err);
        }
        projects.insert(project);
    }
    let project_count = projects.len();
    for project in projects.into_iter().take(project_count) {
        match (tenant.parse(), project.parse()) {
            (Ok(tenant), Ok(project)) => {
                hub.presence().roster_changed(&Topics::new(tenant, project))
            }
            _ => {
                tracing::error!(%tenant, %project, "a stored tenant or project id is invalid; the roster was not published")
            }
        }
    }
    match failed {
        Some(err) => Err(err.into()),
        None => Ok(StatusCode::NO_CONTENT),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoleRequest {
    role: TenantRole,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChangedRole {
    handle: String,
    role: TenantRole,
}

/// `PUT /v1/tenants/{tenant}/members/{handle}/role`, for owners and admins.
pub(crate) async fn change_role(
    State(hub): State<Hub>,
    Path((tenant, handle)): Path<(String, String)>,
    caller: Caller,
    Json(request): Json<RoleRequest>,
) -> Result<Json<ChangedRole>, ApiError> {
    let human = caller.human()?;
    let mut tx = lock_tenant(hub.pool(), &tenant).await?;
    require_group_admin(tenant_role(&mut *tx, &tenant, &human).await?)?;
    if request.role == TenantRole::Owner {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "use_transfer",
            "use the transfer route to change ownership",
        ));
    }
    let target: Option<(String, String)> = sqlx::query_as(
        "SELECT m.human_id, m.role FROM tenant_members m JOIN humans h ON h.id = m.human_id
         WHERE m.tenant_id = $1 AND h.handle = $2",
    )
    .bind(&tenant)
    .bind(&handle)
    .fetch_optional(&mut *tx)
    .await?;
    let (target, role) = target.ok_or_else(no_such_member)?;
    if role.parse::<TenantRole>().map_err(ApiError::stored)? == TenantRole::Owner {
        return Err(ApiError::conflict(
            "owner_role_fixed",
            "use the transfer route to change the owner's role",
        ));
    }
    sqlx::query("UPDATE tenant_members SET role = $3 WHERE tenant_id = $1 AND human_id = $2")
        .bind(&tenant)
        .bind(&target)
        .bind(request.role.as_str())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(ChangedRole {
        handle,
        role: request.role,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferRequest {
    to: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Transferred {
    owner: String,
}

/// `POST /v1/tenants/{tenant}/transfer`. Demote before promoting so the
/// partial unique index always permits at most one owner.
pub(crate) async fn transfer(
    State(hub): State<Hub>,
    Path(tenant): Path<String>,
    caller: Caller,
    Json(request): Json<TransferRequest>,
) -> Result<Json<Transferred>, ApiError> {
    let human = caller.human()?;
    let mut tx = lock_tenant(hub.pool(), &tenant).await?;
    if tenant_role(&mut *tx, &tenant, &human).await? != Some(TenantRole::Owner) {
        return Err(ApiError::forbidden(
            "owner_required",
            "only the group owner can transfer ownership",
        ));
    }
    let target: Option<String> = sqlx::query_scalar(
        "SELECT m.human_id FROM tenant_members m JOIN humans h ON h.id = m.human_id
         WHERE m.tenant_id = $1 AND h.handle = $2 AND m.role = 'admin'",
    )
    .bind(&tenant)
    .bind(&request.to)
    .fetch_optional(&mut *tx)
    .await?;
    let target = target.ok_or_else(|| {
        ApiError::conflict("not_a_group_admin", "the new owner must be a group admin")
    })?;
    sqlx::query("UPDATE tenant_members SET role = 'admin' WHERE tenant_id = $1 AND human_id = $2")
        .bind(&tenant)
        .bind(human.as_str())
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE tenant_members SET role = 'owner' WHERE tenant_id = $1 AND human_id = $2")
        .bind(&tenant)
        .bind(&target)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(Transferred { owner: request.to }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn group_admin_is_owner_or_admin() {
        for role in [TenantRole::Owner, TenantRole::Admin] {
            assert_eq!(require_group_admin(Some(role)).unwrap(), role);
        }
        for role in [Some(TenantRole::Member), None] {
            assert!(matches!(
                require_group_admin(role),
                Err(ApiError::Client {
                    status: StatusCode::FORBIDDEN,
                    code: "group_admin_required",
                    ..
                })
            ));
        }
    }
}
