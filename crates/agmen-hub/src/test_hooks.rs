//! Hooks for integration tests, built only with the `test-hooks` feature
//! and never in the hub binary: agmen-client's and the CLI's tests run a
//! hub in process and need a project, an identity with its hub token, or a
//! signed-in human (ruling R59). Outside this crate they could only get
//! those through a browser sign-in with GitHub, and a test that wrote the
//! rows itself would have to copy how the hub stores token hashes.

use agmen_proto::{TenantId, Topics};

use crate::billing::PlanRecord;

use crate::auth::{approve_as, issue_human_tokens, issue_identity_token};
use crate::db::{Role, TenantRole, new_id};
use crate::http::{ApiError, Hub};
use crate::identities::{HumanId, Identity};
use crate::tenants::join_tenant;

/// What a hook returns when a query or the random source fails.
pub type HookError = Box<dyn std::error::Error + Send + Sync>;

/// Joins a tenant through the hub's role rules on the test's connection.
/// This hook lets integration support use those rules within its transaction.
///
/// # Errors
///
/// When the tenant or human does not exist, an owner conflicts, or a query fails.
pub async fn seed_tenant_member(
    conn: &mut sqlx::PgConnection,
    tenant: &str,
    human: &str,
    role: TenantRole,
) -> Result<(), sqlx::Error> {
    join_tenant(conn, tenant, human, role).await
}

/// A person [`Hub::seed_human`] made, signed in as `agmen login` would be.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SeededHuman {
    /// Their id, `owner_id` in an identity file.
    pub id: HumanId,
    /// Their handle; their identities are named `<handle>/<name>`.
    pub handle: String,
    /// A human access token (1 hour).
    pub access_token: String,
    /// The refresh token that goes with it (30 days).
    pub refresh_token: String,
}

impl Hub {
    /// Replaces the self-host group's pending claim token, unless it has an owner.
    ///
    /// # Errors
    ///
    /// When the group is missing, a query fails, or the random source fails.
    pub async fn mint_claim_token(&self) -> Result<Option<String>, HookError> {
        Ok(crate::group::mint_claim_token(self.pool())
            .await?
            .map(|claim| claim.token))
    }

    /// Sets the repository URL for the given project.
    ///
    /// # Errors
    ///
    /// When the database query fails or the project does not exist.
    pub async fn set_repo_url(
        &self,
        topics: &Topics,
        repo_url: &agmen_proto::RepoUrl,
    ) -> Result<(), HookError> {
        let updated =
            sqlx::query("UPDATE projects SET repo_url = $1 WHERE tenant_id = $2 AND id = $3")
                .bind(repo_url.as_str())
                .bind(topics.tenant().as_str())
                .bind(topics.project().as_str())
                .execute(self.pool())
                .await?
                .rows_affected();
        if updated == 0 {
            return Err("no such project".into());
        }
        Ok(())
    }

    /// Sets how many events streams the hub holds open in all, and per
    /// person, so a test can reach the caps with a few streams.
    pub fn set_events_limits(&self, total: usize, per_human: usize) {
        self.event_slots().set_limits(total, per_human);
    }

    /// Makes a tenant and a project, each with a fresh id, and returns the
    /// project's topics.
    ///
    /// # Errors
    ///
    /// When a query or the random source fails.
    pub async fn seed_project(&self) -> Result<Topics, HookError> {
        let (tenant, project) = (new_id()?, new_id()?);
        let mut tx = self.pool().begin().await?;
        sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'tenant ' || $1)")
            .bind(&tenant)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO projects (id, tenant_id, name) VALUES ($1, $2, 'project ' || $1)")
            .bind(&project)
            .bind(&tenant)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Topics::new(tenant.parse()?, project.parse()?))
    }

    /// Makes a person who is a member of `topics`' project and an agent
    /// identity they own, named `<person>/agent`, and returns the identity
    /// with its hub token.
    ///
    /// # Errors
    ///
    /// When a query or the random source fails.
    pub async fn seed_identity(&self, topics: &Topics) -> Result<(Identity, String), HookError> {
        let (human, id) = (new_id()?, new_id()?);
        let mut tx = self.pool().begin().await?;
        // The person's id is a valid handle too, and unique.
        sqlx::query(
            "INSERT INTO humans (id, provider, provider_sub, handle) VALUES ($1, 'github', $1, $1)",
        )
        .bind(&human)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO memberships (human_id, project_id, role) VALUES ($1, $2, 'member')",
        )
        .bind(&human)
        .bind(topics.project().as_str())
        .execute(&mut *tx)
        .await?;
        join_tenant(
            &mut tx,
            topics.tenant().as_str(),
            &human,
            TenantRole::Member,
        )
        .await?;
        sqlx::query(
            "INSERT INTO identities (id, tenant_id, project_id, kind, handle, owner_human_id)
             VALUES ($1, $2, $3, 'agent', $4 || '/agent', $4)",
        )
        .bind(&id)
        .bind(topics.tenant().as_str())
        .bind(topics.project().as_str())
        .bind(&human)
        .execute(&mut *tx)
        .await?;
        let token = issue_identity_token(&mut tx, &human, &id)
            .await
            .map_err(hook_error)?;
        tx.commit().await?;
        let identity = Identity::load(self.pool(), &id)
            .await
            .map_err(hook_error)?
            .ok_or("the seeded identity is missing")?;
        Ok((identity, token))
    }

    /// Makes a person with `role` in `topics`' project, and an access and
    /// refresh token pair for them as the device flow issues it. Their
    /// handle is their id, which is unique and a valid handle.
    ///
    /// # Errors
    ///
    /// When a query or the random source fails.
    pub async fn seed_human(&self, topics: &Topics, role: Role) -> Result<SeededHuman, HookError> {
        let human = new_id()?;
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO humans (id, provider, provider_sub, handle) VALUES ($1, 'github', $1, $1)",
        )
        .bind(&human)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO memberships (human_id, project_id, role) VALUES ($1, $2, $3)")
            .bind(&human)
            .bind(topics.project().as_str())
            .bind(role.as_str())
            .execute(&mut *tx)
            .await?;
        join_tenant(
            &mut tx,
            topics.tenant().as_str(),
            &human,
            if role == Role::Admin {
                TenantRole::Admin
            } else {
                TenantRole::Member
            },
        )
        .await?;
        let tokens = issue_human_tokens(&mut tx, &human)
            .await
            .map_err(hook_error)?;
        tx.commit().await?;
        Ok(SeededHuman {
            id: HumanId::from_stored(human.clone()),
            handle: human,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
        })
    }

    /// Overwrites the tenant's plan. Paddle ids already stored stay.
    ///
    /// # Errors
    ///
    /// When a query fails.
    pub async fn set_plan(&self, tenant: &TenantId, record: &PlanRecord) -> Result<(), HookError> {
        let mut tx = self.pool().begin().await?;
        crate::groups::store_plan(&mut tx, tenant, record).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Approves the device login `user_code` (as `agmen login` shows it,
    /// such as `BCDF-GHJK`) for `human`, standing in for the GitHub sign-in
    /// step. The CLI's next poll then gets tokens.
    ///
    /// # Errors
    ///
    /// When no open login has that code (unknown, expired or already
    /// approved), or a query fails.
    pub async fn approve_device(&self, user_code: &str, human: &HumanId) -> Result<(), HookError> {
        if approve_as(self, user_code, human.as_str())
            .await
            .map_err(hook_error)?
        {
            Ok(())
        } else {
            Err(format!("no open device login has the code {user_code:?}").into())
        }
    }
}

fn hook_error(err: ApiError) -> HookError {
    Box::new(err)
}
