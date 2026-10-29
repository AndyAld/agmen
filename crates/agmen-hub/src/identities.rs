//! Identities: minting a person's agents and coordinators, their broker
//! tokens, and revocation.

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use agmen_proto::{
    EditScope, IdentityId, IdentityKind, ProjectId, RoleName, RoleSpec, Sha256Hex, TenantId,
    Timestamp, Topics,
};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::auth::{Caller, issue_identity_token};
use crate::billing::PlanState;
use crate::db::{Role, is_unique_violation, new_id};
use crate::emqx::EmqxError;
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
    /// `agent`, `coordinator` or `listener`.
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
            "listener" => IdentityKind::Listener,
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

/// The kinds a person may mint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MintKind {
    /// Any member may mint an agent.
    #[default]
    Agent,
    /// Needs the coordinator or admin role in the project.
    Coordinator,
    /// At most one live listener per owner and project.
    Listener,
}

impl MintKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Coordinator => "coordinator",
            Self::Listener => "listener",
        }
    }
}

/// `POST /v1/identities` body. Unknown fields are refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MintRequest {
    handle: String,
    project: ProjectId,
    #[serde(default)]
    kind: MintKind,
    role: Option<RoleName>,
    verbs: Option<BTreeSet<String>>,
    edits: Option<EditScope>,
    role_sha256: Option<Sha256Hex>,
    agent: Option<String>,
    delegated_by: Option<IdentityId>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    verbs: Option<BTreeSet<String>>,
}

impl MintRequest {
    fn permissions(&self) -> Result<Option<RoleSpec>, ApiError> {
        match (&self.role, &self.verbs, &self.edits, &self.role_sha256) {
            (None, None, None, None) => Ok(None),
            (Some(role), Some(verbs), Some(edits), Some(sha)) => {
                crate::lanes::validate_edits(edits)?;
                if self.kind == MintKind::Listener && *edits != EditScope::None {
                    return Err(bad_role("a listener's edits must be none"));
                }
                let kind = match self.kind {
                    MintKind::Agent => IdentityKind::Agent,
                    MintKind::Coordinator => IdentityKind::Coordinator,
                    MintKind::Listener => IdentityKind::Listener,
                };
                let mut verbs = crate::roles::cap_verbs(kind, verbs);
                if kind == IdentityKind::Listener && role.as_str() != "coordinator" {
                    verbs.remove("ASSIGN");
                }
                Ok(Some(RoleSpec {
                    role: role.clone(),
                    verbs,
                    edits: edits.clone(),
                    role_sha256: sha.clone(),
                }))
            }
            _ => Err(bad_role(
                "role, verbs, edits and role_sha256 must be supplied together",
            )),
        }
    }
}

fn bad_role(message: &str) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_role", message)
}

/// Share-locks the plan row of the group a member mints into and returns its
/// stored state. No row for a non-member or a group without a plan.
const LOCK_PLAN_FOR_MINT: &str = "SELECT tp.state
     FROM memberships m
     JOIN projects p ON p.id = m.project_id
     JOIN tenant_plans tp ON tp.tenant_id = p.tenant_id
     WHERE m.human_id = $1 AND m.project_id = $2
     FOR SHARE OF tp";

/// The refusal for a group whose plan is `deleted`: its deletion is under
/// way, and an identity minted now would only be revoked again.
fn refuse_deleted_group(state: Option<&str>) -> Result<(), ApiError> {
    let state = state
        .map(str::parse::<PlanState>)
        .transpose()
        .map_err(ApiError::stored)?;
    if state == Some(PlanState::Deleted) {
        return Err(ApiError::new(
            StatusCode::GONE,
            "group_deleted",
            "the group is being deleted",
        ));
    }
    Ok(())
}

/// `POST /v1/identities` (human token): mints an identity owned by the
/// caller in a project they belong to. Its handle must be
/// `<caller's handle>/<name>`.
#[expect(
    clippy::too_many_lines,
    reason = "keep mint authorization, validation and its single insert together"
)]
pub(crate) async fn mint(
    State(hub): State<Hub>,
    caller: Caller,
    Json(request): Json<MintRequest>,
) -> Result<(StatusCode, Json<Minted>), ApiError> {
    let human = caller.human()?;
    // Locks run plan, then membership, then project (the foreign key), the
    // order `billing::clock` takes them. The plan row is share-locked first,
    // for a member only, so a stranger learns nothing about the group. The
    // clock stores `deleted` under the row's exclusive lock, so a mint that
    // reads another state commits before the clock's revoke round starts,
    // and a later one is refused here: the deletion cannot be kept waiting
    // by new identities. A group with no plan row (self-host, operator) has
    // nothing to lock.
    let mut tx = hub.pool().begin().await?;
    let plan_state: Option<String> = sqlx::query_scalar(LOCK_PLAN_FOR_MINT)
        .bind(human.as_str())
        .bind(request.project.as_str())
        .fetch_optional(&mut *tx)
        .await?;
    refuse_deleted_group(plan_state.as_deref())?;
    // The membership row stays share-locked until the commit. A member
    // removal deletes that row before it revokes identities, so it waits for
    // this mint and then revokes the identity, or finishes first and this
    // lookup finds no row.
    let membership: Option<(String, String, String)> = sqlx::query_as(
        "SELECT h.handle, m.role, p.tenant_id
         FROM humans h
         JOIN memberships m ON m.human_id = h.id
         JOIN projects p ON p.id = m.project_id
         WHERE h.id = $1 AND m.project_id = $2
         FOR SHARE OF m",
    )
    .bind(human.as_str())
    .bind(request.project.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((owner_handle, role, tenant)) = membership else {
        return Err(ApiError::forbidden(
            "not_a_member",
            "you are not a member of that project",
        ));
    };
    let role: Role = role.parse().map_err(ApiError::stored)?;
    // Parsed before the insert, so nothing after the commit can fail and
    // lose the token shown once.
    let topics = Topics::new(
        tenant.parse().map_err(ApiError::stored)?,
        request.project.clone(),
    );
    let name = request
        .handle
        .strip_prefix(&owner_handle)
        .and_then(|rest| rest.strip_prefix('/'));
    if !name.is_some_and(agmen_core::is_child_name) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_handle",
            format!(
                "the handle must be {owner_handle}/<name>, where name is 1 to 32 of [a-z0-9-] starting with a letter or digit"
            ),
        ));
    }
    let coordinator = request.kind == MintKind::Coordinator
        || (request.kind == MintKind::Listener
            && request
                .role
                .as_ref()
                .is_some_and(|role| role.as_str() == "coordinator"));
    if coordinator && role < Role::Coordinator {
        return Err(ApiError::forbidden(
            "coordinator_role_required",
            "minting a coordinator needs the coordinator role in the project",
        ));
    }

    let permissions = request.permissions()?;
    if request
        .agent
        .as_ref()
        .is_some_and(|agent| agent.len() > agmen_proto::MAX_HELLO_FIELD_BYTES)
    {
        return Err(bad_role("agent is too long"));
    }
    let verbs = permissions.as_ref().map(|spec| spec.verbs.clone());
    let edits = permissions
        .as_ref()
        .map(|spec| serde_json::to_value(&spec.edits))
        .transpose()
        .map_err(ApiError::stored)?;

    let id = new_id()?;
    if let Some(listener) = &request.delegated_by {
        // Hold the listener through the worker insert. A revocation that
        // already holds the row wins, and its committed state is rechecked.
        let valid: Option<String> = sqlx::query_scalar(
            "SELECT id FROM identities WHERE id=$1 AND owner_human_id=$2
             AND project_id=$3 AND kind='listener' AND revoked_at IS NULL FOR UPDATE",
        )
        .bind(listener.as_str())
        .bind(human.as_str())
        .bind(request.project.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        if request.kind != MintKind::Agent || valid.is_none() {
            return Err(bad_role(
                "delegated_by must name a live listener of this owner and project for an agent worker",
            ));
        }
    }
    let inserted = sqlx::query(
        "INSERT INTO identities (id, tenant_id, project_id, kind, handle, owner_human_id,
          role, verbs, edits, role_sha256, agent, delegated_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(&id)
    .bind(&tenant)
    .bind(request.project.as_str())
    .bind(request.kind.as_str())
    .bind(&request.handle)
    .bind(human.as_str())
    .bind(permissions.as_ref().map(|spec| spec.role.as_str()))
    .bind(
        verbs
            .as_ref()
            .map(|set| set.iter().cloned().collect::<Vec<_>>()),
    )
    .bind(edits)
    .bind(permissions.as_ref().map(|spec| spec.role_sha256.as_str()))
    .bind(&request.agent)
    .bind(request.delegated_by.as_ref().map(IdentityId::as_str))
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(_) => {}
        Err(err) if is_unique_violation(&err) => {
            if matches!(&err, sqlx::Error::Database(db) if db.constraint() == Some("identities_one_listener"))
            {
                return Err(ApiError::conflict(
                    "listener_exists",
                    "this owner already has a live listener in this project",
                ));
            }
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
            verbs,
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

/// `DELETE /v1/identities/{id}` (the owner's or a group admin's human token): revokes the
/// identity, bans its username in EMQX until its last broker token has
/// expired, kicks its client ids off the broker, and has its project's
/// roster published without it. Its broker tokens are refused from then
/// on. Repeating the call bans and kicks again, so a failed call can be
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
         WHERE id = $1 AND (owner_human_id = $2 OR EXISTS (
             SELECT 1 FROM tenant_members m WHERE m.tenant_id = identities.tenant_id
             AND m.human_id = $2 AND m.role IN ('owner', 'admin')))
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
    // From here on nothing returns early before the ban and the kick: once
    // revoked_at is set, the identity's connections must go.
    let failed = ban_and_kick(&hub, &id, ban_until(revoked_at)).await;
    match (tenant.parse(), project.parse()) {
        (Ok(tenant), Ok(project)) => hub.presence().roster_changed(&Topics::new(tenant, project)),
        (tenant, project) => tracing::error!(
            identity = %id,
            tenant_ok = tenant.is_ok(),
            project_ok = project.is_ok(),
            "a stored tenant or project id is invalid; the roster was not published"
        ),
    }
    match failed {
        Some(err) => Err(err.into()),
        None => Ok(StatusCode::NO_CONTENT),
    }
}

/// When the ban of an identity revoked at `revoked_at` (Unix seconds) may
/// end: once every broker token minted before the revocation has expired.
/// A stored time that does not convert bans for good, since a revoked id
/// is never used again.
pub(crate) fn ban_until(revoked_at: i64) -> Timestamp {
    Timestamp::from_second(revoked_at)
        .and_then(|at| at.checked_add(BROKER_TOKEN_TTL + BAN_MARGIN))
        .unwrap_or_else(|err| {
            tracing::error!(revoked_at, error = %err, "the revocation time is out of range; banning for good");
            Timestamp::MAX
        })
}

/// Bans `id`'s username until `ban_until` and kicks each of its client
/// ids, trying every step even when one fails, and returns the first
/// failure.
pub(crate) async fn ban_and_kick(hub: &Hub, id: &str, ban_until: Timestamp) -> Option<EmqxError> {
    let mut failed = None;
    // Ban before kicking: a kicked client that reconnects with its
    // unexpired token is refused, since EMQX checks only signature and exp.
    // Past ban_until every token minted before the revocation has expired,
    // so a late retry needs no ban.
    if ban_until > Timestamp::now()
        && let Err(err) = hub.emqx().ban_username(id, ban_until).await
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
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deleted_plan_refuses_a_mint_with_410() {
        let Err(ApiError::Client { status, code, .. }) = refuse_deleted_group(Some("deleted"))
        else {
            panic!("a deleted plan must refuse");
        };
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(code, "group_deleted");
    }

    #[test]
    fn every_other_plan_state_and_no_plan_row_may_mint() {
        for state in ["trial", "active", "past_due", "read_only"] {
            assert!(refuse_deleted_group(Some(state)).is_ok(), "{state}");
        }
        assert!(refuse_deleted_group(None).is_ok());
    }

    #[test]
    fn an_unknown_stored_state_is_a_stored_error() {
        assert!(matches!(
            refuse_deleted_group(Some("paused")),
            Err(ApiError::Stored(_))
        ));
    }
}
