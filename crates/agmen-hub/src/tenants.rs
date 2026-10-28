//! Membership and rights in a tenant, shared by its projects.

use sqlx::{PgConnection, PgPool};

use crate::db::TenantRole;
use crate::http::ApiError;
use crate::identities::HumanId;

/// The human's role in this tenant, if they belong to it.
// shortcut: self-host Task 3 wires this lookup and removes the dead-code expectation.
#[expect(dead_code, reason = "used by self-host Task 3")]
pub(crate) async fn tenant_role(
    pool: &PgPool,
    tenant: &str,
    human: &HumanId,
) -> Result<Option<TenantRole>, ApiError> {
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM tenant_members WHERE tenant_id = $1 AND human_id = $2",
    )
    .bind(tenant)
    .bind(human.as_str())
    .fetch_optional(pool)
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
// shortcut: self-host Task 3 wires this guard and removes the dead-code expectation.
#[cfg_attr(not(test), expect(dead_code, reason = "used by self-host Task 3"))]
pub(crate) fn require_group_admin(role: Option<TenantRole>) -> Result<TenantRole, ApiError> {
    match role {
        Some(role @ (TenantRole::Owner | TenantRole::Admin)) => Ok(role),
        Some(TenantRole::Member) | None => Err(ApiError::forbidden(
            "group_admin_required",
            "only a group admin or owner can do this",
        )),
    }
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
