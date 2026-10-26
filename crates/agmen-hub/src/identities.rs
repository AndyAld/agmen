//! Identities: minting a person's agents and coordinators, their broker
//! tokens, and revocation.

use std::fmt;
use std::time::Duration;

use agmen_proto::{IdentityId, IdentityKind, ProjectId, TenantId, Timestamp, Topics};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::auth::{Caller, issue_identity_token};
use crate::db::{Role, is_unique_violation, new_id};
use crate::http::{ApiError, Hub};

/// How long a broker token lasts (design 6.3). Clients refresh it 5
/// minutes before it expires.
pub const BROKER_TOKEN_TTL: Duration = Duration::from_secs(3600);

/// The client id suffixes an identity may connect with (ADR-0002): the MCP
/// server, `agmen watch`, and one-shot CLI commands.
pub const CLIENT_ID_SUFFIXES: [&str; 3] = ["mcp", "watch", "cli"];

/// How long a revoked identity's username stays banned in EMQX past
/// [`BROKER_TOKEN_TTL`] after revocation (ruling R36). It covers a token
/// minted just as the revocation committed, and clock skew.
const BAN_MARGIN: Duration = Duration::from_secs(300);

/// A person's id, minted by the hub at first sign-in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HumanId(String);

impl HumanId {
    /// An id read back from the database.
    pub(crate) const fn from_stored(id: String) -> Self {
        Self(id)
    }

    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HumanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An identity as the hub stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Identity {
    /// The id: the MQTT username and the base of its client ids.
    pub id: IdentityId,
    /// The tenant its project belongs to.
    pub tenant: TenantId,
    /// The one project it works in.
    pub project: ProjectId,
    /// `agent` or `coordinator`.
    pub kind: IdentityKind,
    /// Its display name, `<owner handle>/<name>`.
    pub handle: String,
    /// The person who owns it.
    pub owner: HumanId,
    /// When it was revoked, if it was.
    pub revoked_at: Option<Timestamp>,
}

type IdentityRow = (String, String, String, String, String, String, Option<i64>);

impl Identity {
    /// Loads identity `id`, revoked or not.
    pub(crate) async fn load(pool: &PgPool, id: &str) -> Result<Option<Self>, ApiError> {
        let row: Option<IdentityRow> = sqlx::query_as(
            "SELECT id, tenant_id, project_id, kind, handle, owner_human_id,
                    extract(epoch FROM revoked_at)::bigint
             FROM identities WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        row.map(Self::from_row).transpose()
    }

    fn from_row(
        (id, tenant, project, kind, handle, owner, revoked_at): IdentityRow,
    ) -> Result<Self, ApiError> {
        let kind = match kind.as_str() {
            "agent" => IdentityKind::Agent,
            "coordinator" => IdentityKind::Coordinator,
            other => {
                return Err(ApiError::stored(UnknownKind(other.to_owned())));
            }
        };
        Ok(Self {
            id: id.parse().map_err(ApiError::stored)?,
            tenant: tenant.parse().map_err(ApiError::stored)?,
            project: project.parse().map_err(ApiError::stored)?,
            kind,
            handle,
            owner: HumanId(owner),
            revoked_at: revoked_at
                .map(Timestamp::from_second)
                .transpose()
                .map_err(ApiError::stored)?,
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown identity kind {0:?} in the database")]
struct UnknownKind(String);

/// The kinds a person may mint in the MVP (no listeners: ruling R10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MintKind {
    /// Any member may mint an agent.
    #[default]
    Agent,
    /// Needs the coordinator or admin role in the project.
    Coordinator,
}

impl MintKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Coordinator => "coordinator",
        }
    }
}

/// `POST /v1/identities` body. Unknown fields such as `delegated_by` are
/// refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MintRequest {
    handle: String,
    project: ProjectId,
    #[serde(default)]
    kind: MintKind,
}

#[derive(Debug, Serialize)]
pub(crate) struct Minted {
    id: String,
    handle: String,
    kind: &'static str,
    tenant: String,
    project: String,
    /// The identity's own hub token, shown once.
    token: String,
}

/// Whether `name` may follow `<owner>/` in a child handle.
fn is_child_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `POST /v1/identities` (human token): mints an identity owned by the
/// caller in a project they belong to. Its handle must be
/// `<caller's handle>/<name>`.
pub(crate) async fn mint(
    State(hub): State<Hub>,
    caller: Caller,
    Json(request): Json<MintRequest>,
) -> Result<(StatusCode, Json<Minted>), ApiError> {
    let human = caller.human()?;
    let membership: Option<(String, String, String)> = sqlx::query_as(
        "SELECT h.handle, m.role, p.tenant_id
         FROM humans h
         JOIN memberships m ON m.human_id = h.id
         JOIN projects p ON p.id = m.project_id
         WHERE h.id = $1 AND m.project_id = $2",
    )
    .bind(human.as_str())
    .bind(request.project.as_str())
    .fetch_optional(hub.pool())
    .await?;
    let Some((owner_handle, role, tenant)) = membership else {
        return Err(ApiError::forbidden(
            "not_a_member",
            "you are not a member of that project",
        ));
    };
    let role: Role = role.parse().map_err(ApiError::stored)?;
    let name = request
        .handle
        .strip_prefix(&owner_handle)
        .and_then(|rest| rest.strip_prefix('/'));
    if !name.is_some_and(is_child_name) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_handle",
            format!(
                "the handle must be {owner_handle}/<name>, where name is 1 to 32 of [a-z0-9-] starting with a letter or digit"
            ),
        ));
    }
    if request.kind == MintKind::Coordinator && role < Role::Coordinator {
        return Err(ApiError::forbidden(
            "coordinator_role_required",
            "minting a coordinator needs the coordinator role in the project",
        ));
    }

    let id = new_id()?;
    let mut tx = hub.pool().begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO identities (id, tenant_id, project_id, kind, handle, owner_human_id)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&id)
    .bind(&tenant)
    .bind(request.project.as_str())
    .bind(request.kind.as_str())
    .bind(&request.handle)
    .bind(human.as_str())
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(err) if is_unique_violation(&err) => {
            return Err(ApiError::conflict(
                "handle_taken",
                format!(
                    "a live identity in this project is already {}",
                    request.handle
                ),
            ));
        }
        Err(err) => return Err(err.into()),
    }
    let token = issue_identity_token(&mut tx, human.as_str(), &id).await?;
    tx.commit().await?;
    tracing::info!(identity = %id, handle = %request.handle, owner = %human, "minted identity");
    // The roster lists the new identity, offline, before it ever connects.
    let topics = Topics::new(
        tenant.parse().map_err(ApiError::stored)?,
        request.project.clone(),
    );
    hub.presence().roster_changed(&topics);
    Ok((
        StatusCode::CREATED,
        Json(Minted {
            id,
            handle: request.handle,
            kind: request.kind.as_str(),
            tenant,
            project: request.project.into(),
            token,
        }),
    ))
}

#[derive(Debug, Serialize)]
pub(crate) struct BrokerToken {
    token: String,
    expires_at: Timestamp,
}

/// `GET /v1/identities/{id}/broker-token` (that identity's token): a
/// broker token valid for [`BROKER_TOKEN_TTL`]. Once the identity is
/// revoked, [`Caller`] refuses its token with 403 `identity_revoked`.
pub(crate) async fn broker_token(
    State(hub): State<Hub>,
    Path(id): Path<String>,
    caller: Caller,
) -> Result<Json<BrokerToken>, ApiError> {
    let identity = caller.identity()?;
    if identity.id.as_str() != id {
        return Err(ApiError::forbidden(
            "not_this_identity",
            "an identity token gets broker tokens only for its own identity",
        ));
    }
    // Read the clock before minting and round down, so the reported expiry
    // is never later than the token's own `exp`.
    let now = Timestamp::now().as_second();
    let token = hub.tokens().mint(&identity, BROKER_TOKEN_TTL)?;
    let ttl = i64::try_from(BROKER_TOKEN_TTL.as_secs()).map_err(ApiError::stored)?;
    let expires_at = Timestamp::from_second(now.saturating_add(ttl)).map_err(ApiError::stored)?;
    Ok(Json(BrokerToken { token, expires_at }))
}

/// `DELETE /v1/identities/{id}` (the owner's human token): revokes the
/// identity, has its project's roster published without it, bans its
/// username in EMQX until its last broker token has expired, and kicks its
/// client ids off the broker. Its broker tokens are refused from then on.
/// Repeating the call bans and kicks again, so a failed call can be
/// retried.
pub(crate) async fn revoke(
    State(hub): State<Hub>,
    Path(id): Path<String>,
    caller: Caller,
) -> Result<StatusCode, ApiError> {
    let human = caller.human()?;
    // Commit the revocation before banning and kicking, so a client that
    // reconnects in between cannot get a fresh broker token.
    let revoked: Option<(String, String, String, i64)> = sqlx::query_as(
        "UPDATE identities SET revoked_at = coalesce(revoked_at, now())
         WHERE id = $1 AND owner_human_id = $2
         RETURNING id, tenant_id, project_id, extract(epoch FROM revoked_at)::bigint",
    )
    .bind(&id)
    .bind(human.as_str())
    .fetch_optional(hub.pool())
    .await?;
    let Some((id, tenant, project, revoked_at)) = revoked else {
        return Err(ApiError::not_found("no such identity of yours"));
    };
    tracing::info!(identity = %id, owner = %human, "revoked identity");
    let topics = Topics::new(
        tenant.parse().map_err(ApiError::stored)?,
        project.parse().map_err(ApiError::stored)?,
    );
    hub.presence().roster_changed(&topics);
    let ban_until = Timestamp::from_second(revoked_at)
        .and_then(|at| at.checked_add(BROKER_TOKEN_TTL + BAN_MARGIN))
        .map_err(ApiError::stored)?;
    let mut failed = None;
    // Ban before kicking: a kicked client that reconnects with its
    // unexpired token is refused, since EMQX checks only signature and exp.
    // Past ban_until every token minted before the revocation has expired,
    // so a late retry needs no ban.
    if ban_until > Timestamp::now()
        && let Err(err) = hub.emqx().ban_username(&id, ban_until).await
    {
        tracing::warn!(identity = %id, error = %err, "ban failed");
        failed = Some(err);
    }
    for suffix in CLIENT_ID_SUFFIXES {
        let client_id = format!("{id}-{suffix}");
        if let Err(err) = hub.emqx().kick(&client_id).await {
            tracing::warn!(%client_id, error = %err, "kick failed");
            failed.get_or_insert(err);
        }
    }
    match failed {
        Some(err) => Err(err.into()),
        None => Ok(StatusCode::NO_CONTENT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_names() {
        for good in ["sol", "a", "w3f", "coord-2", &"x".repeat(32)] {
            assert!(is_child_name(good), "{good}");
        }
        for bad in ["", "-x", "Sol", "a/b", "a b", &"x".repeat(33)] {
            assert!(!is_child_name(bad), "{bad}");
        }
    }
}
