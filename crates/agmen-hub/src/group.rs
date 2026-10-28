//! The self-host hub's single group and its initial ownership claim.

use agmen_proto::{TenantId, Timestamp};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::auth::Caller;
use crate::db::{TenantRole, hash_secret, new_id, new_secret};
use crate::http::{ApiError, Hub};
use crate::tenants::join_tenant;

/// Why the self-host group could not be initialized.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GroupError {
    /// A database without a selected group has more than one tenant.
    #[error(
        "a self-host hub has one group, but the database has {0} tenants; set AGMEN_HUB_MODE=hosted"
    )]
    ManyTenants(i64),
    /// A group query failed.
    #[error("accessing the self-host group failed")]
    Database(#[from] sqlx::Error),
    /// The operating system could not mint an id or token.
    #[error("the operating system's random source failed")]
    Random(#[from] getrandom::Error),
}

/// The self-host group, and the name of the tenant if this call adopted it.
pub(crate) struct Group {
    pub(crate) tenant: TenantId,
    pub(crate) adopted: Option<String>,
}

/// Selects the existing group, adopts the only tenant, or creates one.
pub(crate) async fn ensure_group(pool: &PgPool, name: &str) -> Result<Group, GroupError> {
    let mut tx = pool.begin().await?;
    // Serialize initialization even before a singleton row exists.
    sqlx::query("LOCK TABLE hub_group IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let existing: Option<String> = sqlx::query_scalar("SELECT tenant_id FROM hub_group")
        .fetch_optional(&mut *tx)
        .await?;
    let mut adopted = None;
    let tenant = match existing {
        Some(tenant) => tenant,
        None => {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tenants")
                .fetch_one(&mut *tx)
                .await?;
            let tenant = match count {
                0 => {
                    let tenant = new_id()?;
                    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2)")
                        .bind(&tenant)
                        .bind(name)
                        .execute(&mut *tx)
                        .await?;
                    tenant
                }
                1 => {
                    let (id, name): (String, String) =
                        sqlx::query_as("SELECT id, name FROM tenants")
                            .fetch_one(&mut *tx)
                            .await?;
                    adopted = Some(name);
                    id
                }
                count => return Err(GroupError::ManyTenants(count)),
            };
            sqlx::query("INSERT INTO hub_group (tenant_id) VALUES ($1)")
                .bind(&tenant)
                .execute(&mut *tx)
                .await?;
            tenant
        }
    };
    let tenant = tenant
        .parse()
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    tx.commit().await?;
    Ok(Group { tenant, adopted })
}

/// The local, single-use secret and its one-hour deadline.
pub(crate) struct ClaimToken {
    pub(crate) token: String,
    pub(crate) expires_at: Timestamp,
}

/// Replaces any pending claim, unless the group has any member: only a group
/// nobody has used is open to claiming, whatever the members' roles.
pub(crate) async fn mint_claim_token(pool: &PgPool) -> Result<Option<ClaimToken>, GroupError> {
    let mut tx = pool.begin().await?;
    let tenant: String = sqlx::query_scalar("SELECT tenant_id FROM hub_group FOR UPDATE")
        .fetch_one(&mut *tx)
        .await?;
    let used: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tenant_members WHERE tenant_id = $1)")
            .bind(&tenant)
            .fetch_one(&mut *tx)
            .await?;
    if used {
        // A token minted while the group was empty must not outlive its use.
        sqlx::query("UPDATE hub_group SET claim_hash = NULL, claim_expires_at = NULL")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    }
    let token = new_secret()?;
    let expires_at: i64 = sqlx::query_scalar(
        "UPDATE hub_group SET claim_hash = $1, claim_expires_at = now() + interval '1 hour'
         RETURNING floor(extract(epoch FROM claim_expires_at))::bigint",
    )
    .bind(hash_secret(&token))
    .fetch_one(&mut *tx)
    .await?;
    let expires_at =
        Timestamp::from_second(expires_at).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    tx.commit().await?;
    Ok(Some(ClaimToken { token, expires_at }))
}

/// The host CLI reads this one structured log line from local logs.
pub(crate) fn log_claim_token(token: &ClaimToken) {
    tracing::warn!(
        claim_token = %token.token,
        expires_at = %token.expires_at,
        "this hub has no owner yet; agmen host up claims it with this token"
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Claim {
    token: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Claimed {
    tenant: String,
    name: String,
    role: TenantRole,
}

/// `POST /v1/claim`: consume the local token and make this human the owner.
pub(crate) async fn claim(
    State(hub): State<Hub>,
    caller: Caller,
    Json(request): Json<Claim>,
) -> Result<Json<Claimed>, ApiError> {
    let human = caller.human()?;
    let mut tx = hub.pool().begin().await?;
    let (tenant, name, hash, expired): (String, String, Option<Vec<u8>>, bool) = sqlx::query_as(
        "SELECT g.tenant_id, t.name, g.claim_hash,
                coalesce(g.claim_expires_at <= now(), false)
         FROM hub_group g JOIN tenants t ON t.id = g.tenant_id FOR UPDATE OF g",
    )
    .fetch_one(&mut *tx)
    .await?;
    let owned: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tenant_members WHERE tenant_id = $1 AND role = 'owner')",
    )
    .bind(&tenant)
    .fetch_one(&mut *tx)
    .await?;
    if owned {
        return Err(ApiError::conflict(
            "already_claimed",
            "this group already has an owner",
        ));
    }
    if hash.as_deref() != Some(hash_secret(&request.token).as_slice()) {
        return Err(ApiError::forbidden(
            "bad_claim_token",
            "the claim token is invalid",
        ));
    }
    if expired {
        return Err(ApiError::new(
            StatusCode::GONE,
            "claim_expired",
            "the claim token has expired",
        ));
    }
    join_tenant(&mut tx, &tenant, human.as_str(), TenantRole::Admin).await?;
    sqlx::query("UPDATE tenant_members SET role = 'owner' WHERE tenant_id = $1 AND human_id = $2")
        .bind(&tenant)
        .bind(human.as_str())
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE hub_group SET claim_hash = NULL, claim_expires_at = NULL")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(Claimed {
        tenant,
        name,
        role: TenantRole::Owner,
    }))
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn claim_log_line_has_the_token_field() {
        let buffer = Buffer(Arc::new(Mutex::new(Vec::new())));
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let token = ClaimToken {
            token: crate::db::new_secret().unwrap(),
            expires_at: "2026-10-28T12:00:00Z".parse().unwrap(),
        };
        tracing::subscriber::with_default(subscriber, || log_claim_token(&token));
        let line = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert_eq!(token.token.len(), 64);
        assert!(token.token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(
            line.contains(&format!("claim_token={}", token.token)),
            "{line}"
        );
        assert!(
            line.contains(&format!("expires_at={}", token.expires_at)),
            "{line}"
        );
        assert!(
            line.contains("this hub has no owner yet; agmen host up claims it with this token")
        );
        assert_eq!(line.lines().count(), 1);
        assert!(!line.contains('\u{1b}'));
    }
}
