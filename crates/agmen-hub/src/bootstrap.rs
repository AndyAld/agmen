//! `agmen-hub bootstrap`: the operator command that makes a tenant, a
//! project in it, and the project's first admin (ruling R37).

use agmen_proto::{ProjectId, TenantId};
use serde::Serialize;
use sqlx::PgPool;

use crate::auth::is_human_handle;
use crate::db::{self, new_id};

/// The longest tenant or project name, in characters.
const MAX_NAME_CHARS: usize = 64;

/// Why bootstrapping failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BootstrapError {
    /// A tenant or project name breaks the naming rule.
    #[error(
        "the {what} name must be 1 to 64 characters, with no control characters and no space at either end"
    )]
    BadName {
        /// `tenant` or `project`.
        what: &'static str,
    },
    /// The admin's GitHub login is not one GitHub allows.
    #[error("{0:?} is not a GitHub login")]
    BadLogin(String),
    /// Connecting, migrating or writing failed.
    #[error("database error")]
    Database(#[from] sqlx::Error),
    /// The operating system's random source failed.
    #[error("minting an id failed")]
    Random(#[from] getrandom::Error),
    /// The database returned an id the schema should have refused.
    #[error("the database holds an invalid id")]
    StoredId(#[from] agmen_proto::ProtoError),
}

/// What to bootstrap: checked names for the tenant, the project and the
/// first admin's GitHub login.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Bootstrap {
    tenant: String,
    project: String,
    admin_github: String,
}

/// What [`Bootstrap::run`] made or found. Serializes as
/// `{"tenant", "project", "admin_github"}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[non_exhaustive]
pub struct Bootstrapped {
    /// The tenant's id.
    pub tenant: TenantId,
    /// The project's id.
    pub project: ProjectId,
    /// The first admin's GitHub login, lowercased.
    pub admin_github: String,
}

fn is_name(name: &str) -> bool {
    (1..=MAX_NAME_CHARS).contains(&name.chars().count())
        && !name.chars().any(char::is_control)
        && name.trim() == name
}

impl Bootstrap {
    /// Checks the names. GitHub logins are case-insensitive, so the login
    /// is kept lowercased.
    ///
    /// # Errors
    ///
    /// [`BootstrapError::BadName`] or [`BootstrapError::BadLogin`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), agmen_hub::BootstrapError> {
    /// let request = agmen_hub::Bootstrap::new("Acme", "agmen", "Andy-A")?;
    /// assert!(agmen_hub::Bootstrap::new("Acme", " ", "andy").is_err());
    /// # Ok(()) }
    /// ```
    pub fn new(
        tenant: impl Into<String>,
        project: impl Into<String>,
        admin_github: &str,
    ) -> Result<Self, BootstrapError> {
        let (tenant, project) = (tenant.into(), project.into());
        if !is_name(&tenant) {
            return Err(BootstrapError::BadName { what: "tenant" });
        }
        if !is_name(&project) {
            return Err(BootstrapError::BadName { what: "project" });
        }
        let admin_github = admin_github.to_ascii_lowercase();
        if !is_human_handle(&admin_github) {
            return Err(BootstrapError::BadLogin(admin_github));
        }
        Ok(Self {
            tenant,
            project,
            admin_github,
        })
    }

    /// Connects to `database_url`, runs the migrations, and makes the
    /// tenant, the project and an unclaimed first-admin row, reusing any
    /// that a run with the same names made before. The admin becomes an
    /// admin member at their next sign-in with that GitHub login.
    ///
    /// # Errors
    ///
    /// [`BootstrapError::Database`] when connecting or a query fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), agmen_hub::BootstrapError> {
    /// let request = agmen_hub::Bootstrap::new("Acme", "agmen", "andy-a")?;
    /// let made = request.run("postgres://localhost/agmen").await?;
    /// println!("project {}", made.project);
    /// # Ok(()) }
    /// ```
    pub async fn run(&self, database_url: &str) -> Result<Bootstrapped, BootstrapError> {
        let pool = db::connect(database_url).await?;
        let made = self.write(&pool).await;
        pool.close().await;
        made
    }

    async fn write(&self, pool: &PgPool) -> Result<Bootstrapped, BootstrapError> {
        let mut tx = pool.begin().await?;
        // The no-op update makes RETURNING give the existing row's id.
        let tenant: String = sqlx::query_scalar(
            "INSERT INTO tenants (id, name) VALUES ($1, $2)
             ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name
             RETURNING id",
        )
        .bind(new_id()?)
        .bind(&self.tenant)
        .fetch_one(&mut *tx)
        .await?;
        let project: String = sqlx::query_scalar(
            "INSERT INTO projects (id, tenant_id, name) VALUES ($1, $2, $3)
             ON CONFLICT (tenant_id, name) DO UPDATE SET name = EXCLUDED.name
             RETURNING id",
        )
        .bind(new_id()?)
        .bind(&tenant)
        .bind(&self.project)
        .fetch_one(&mut *tx)
        .await?;
        // A claimed row stays claimed, so a re-run is a no-op.
        sqlx::query(
            "INSERT INTO bootstrap_admins (project_id, github_login) VALUES ($1, $2)
             ON CONFLICT (project_id, github_login) DO NOTHING",
        )
        .bind(&project)
        .bind(&self.admin_github)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Bootstrapped {
            tenant: tenant.parse()?,
            project: project.parse()?,
            admin_github: self.admin_github.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_logins() {
        let ok = Bootstrap::new("Acme Co", "agmen", "Andy-A").unwrap();
        assert_eq!(ok.admin_github, "andy-a");
        assert!(Bootstrap::new("x".repeat(64), "p", "a").is_ok());
        for (tenant, project) in [("", "p"), (" t", "p"), ("t", "p "), ("t", "a\nb")] {
            assert!(
                matches!(
                    Bootstrap::new(tenant, project, "andy"),
                    Err(BootstrapError::BadName { .. })
                ),
                "{tenant:?} {project:?}"
            );
        }
        assert!(Bootstrap::new("x".repeat(65), "p", "a").is_err());
        for login in ["", "-andy", "andy/x", "andy_a", &"a".repeat(40)] {
            assert!(
                matches!(
                    Bootstrap::new("t", "p", login),
                    Err(BootstrapError::BadLogin(_))
                ),
                "{login:?}"
            );
        }
    }
}
