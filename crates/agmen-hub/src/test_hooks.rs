//! Hooks for other crates' integration tests, built only with the
//! `test-hooks` feature: agmen-client's tests run a hub in process and need
//! a project and an identity with its hub token. Outside this crate they
//! could only get those through a browser sign-in, and a test that wrote
//! the rows itself would have to copy how the hub stores token hashes.

use agmen_proto::Topics;

use crate::auth::issue_identity_token;
use crate::db::new_id;
use crate::http::{ApiError, Hub};
use crate::identities::Identity;

/// What a hook returns when a query or the random source fails.
pub type HookError = Box<dyn std::error::Error + Send + Sync>;

impl Hub {
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
}

fn hook_error(err: ApiError) -> HookError {
    Box::new(err)
}
