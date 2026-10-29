//! `agmen-hub bootstrap`: the operator command that makes a tenant, a
//! project in it, and the project's first admin (rulings R37 and R39).

use agmen_proto::{ProjectId, RepoUrl, TenantId};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use url::Url;

use crate::auth::{GithubError, GithubId, account_by_login, is_human_handle};
use crate::config::HubMode;
use crate::db::{self, is_display_name, new_id};
use crate::group::{self, GroupError};
use crate::http::upstream_client;

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
    /// GitHub knows no account with the admin's login.
    #[error("GitHub has no account with the login {0:?}")]
    UnknownLogin(String),
    /// Looking the login up at GitHub failed.
    #[error("looking the login up at GitHub failed")]
    Github(#[source] GithubError),
    /// The HTTP client for GitHub could not be built.
    #[error("building the HTTP client failed")]
    HttpClient(#[source] reqwest::Error),
    /// Connecting, migrating or writing failed.
    #[error("database error")]
    Database(#[from] sqlx::Error),
    /// The operating system's random source failed.
    #[error("minting an id failed")]
    Random(#[from] getrandom::Error),
    /// The database returned an id the schema should have refused.
    #[error("the database holds an invalid id")]
    StoredId(#[from] agmen_proto::ProtoError),
    /// A self-host hub has one group and the requested tenant is another.
    #[error(
        "this self-host hub has one group, {group:?}; use --tenant {group:?} or set AGMEN_HUB_MODE=hosted; \
         hosted mode also requires AGMEN_PADDLE_API_KEY, AGMEN_PADDLE_WEBHOOK_SECRET, \
         AGMEN_PADDLE_PRICE_TEAM_BASE, AGMEN_PADDLE_PRICE_TEAM_SEAT, \
         AGMEN_PADDLE_PRICE_BUSINESS_10, AGMEN_PADDLE_PRICE_BUSINESS_25 and AGMEN_PADDLE_PRICE_BUSINESS_50"
    )]
    OtherGroup {
        /// The hub's group name.
        group: String,
    },
    /// The self-host group could not be selected or created.
    #[error("selecting the self-host group failed")]
    Group(#[from] GroupError),
    /// The database holds a repository URL outside the allowed forms.
    #[error("the database holds an invalid repository URL")]
    StoredRepoUrl(#[from] agmen_proto::BadRepoUrl),
}

/// Who the first admin is. Either way the hub stores the account id, since
/// a login can change hands (ruling R39).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AdminGithub {
    /// A GitHub login, resolved to its account id once, at bootstrap time.
    Login(String),
    /// The account id itself, for when GitHub cannot be reached.
    Id(GithubId),
}

/// What to bootstrap: checked names for the tenant, the project and the
/// first admin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Bootstrap {
    tenant: String,
    project: String,
    admin: AdminGithub,
    repo_url: Option<RepoUrl>,
    mode: HubMode,
}

/// What [`Bootstrap::run`] made or found. Serializes as
/// `{"tenant", "project", "admin_github_id", "admin_github", "repo_url"}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[non_exhaustive]
pub struct Bootstrapped {
    /// The tenant's id.
    pub tenant: TenantId,
    /// The project's id.
    pub project: ProjectId,
    /// The first admin's GitHub account id.
    pub admin_github_id: GithubId,
    /// The first admin's GitHub login as GitHub spells it, lowercased; for
    /// display only. `None` when the admin was named by id.
    pub admin_github: Option<String>,
    /// The project's repository URL, if set.
    pub repo_url: Option<RepoUrl>,
}

impl Bootstrap {
    /// Checks the names. GitHub logins are case-insensitive, so a login is
    /// kept lowercased.
    ///
    /// # Errors
    ///
    /// [`BootstrapError::BadName`] or [`BootstrapError::BadLogin`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use agmen_hub::{AdminGithub, Bootstrap};
    /// let by_login = Bootstrap::new("Acme", "agmen", AdminGithub::Login("Andy-A".into()))?;
    /// let by_id = Bootstrap::new("Acme", "agmen", AdminGithub::Id("583231".parse()?))?;
    /// assert!(Bootstrap::new("Acme", " ", AdminGithub::Login("andy".into())).is_err());
    /// # Ok(()) }
    /// ```
    pub fn new(
        tenant: impl Into<String>,
        project: impl Into<String>,
        admin: AdminGithub,
    ) -> Result<Self, BootstrapError> {
        let (tenant, project) = (tenant.into(), project.into());
        if !is_display_name(&tenant) {
            return Err(BootstrapError::BadName { what: "tenant" });
        }
        if !is_display_name(&project) {
            return Err(BootstrapError::BadName { what: "project" });
        }
        let admin = match admin {
            AdminGithub::Login(login) => AdminGithub::Login(checked_login(&login)?),
            AdminGithub::Id(id) => AdminGithub::Id(id),
        };
        Ok(Self {
            tenant,
            project,
            admin,
            repo_url: None,
            mode: HubMode::default(),
        })
    }

    /// Sets the hub mode, `self-host` unless this says otherwise. In
    /// self-host mode the tenant is the hub's one group: it is created when
    /// the hub has none, and any other tenant name is refused with
    /// [`BootstrapError::OtherGroup`].
    #[must_use]
    pub fn with_mode(mut self, mode: HubMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the project's repository URL. Without this option, a rerun
    /// preserves the stored URL.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use agmen_hub::{AdminGithub, Bootstrap};
    /// let request = Bootstrap::new("Acme", "agmen", AdminGithub::Id("1".parse()?))?
    ///     .with_repo_url("https://github.com/agmen-dev/agmen".parse()?);
    /// # Ok(()) }
    /// ```
    #[must_use]
    pub fn with_repo_url(mut self, repo_url: RepoUrl) -> Self {
        self.repo_url = Some(repo_url);
        self
    }

    /// Resolves a login to its account id through GitHub's users API at
    /// `github_api_url`, then connects to `database_url`, runs the
    /// migrations, and makes the tenant, the project and an unclaimed
    /// first-admin row, reusing any that a run with the same names made
    /// before. The admin becomes an admin member at the account's next
    /// sign-in, whatever its login is by then.
    ///
    /// # Errors
    ///
    /// [`BootstrapError::UnknownLogin`] or [`BootstrapError::Github`] when
    /// the lookup fails, and [`BootstrapError::Database`] when connecting
    /// or a query fails. Nothing is written when the lookup fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use agmen_hub::{AdminGithub, Bootstrap};
    /// let request = Bootstrap::new("Acme", "agmen", AdminGithub::Login("andy-a".into()))?;
    /// let github = url::Url::parse("https://api.github.com")?;
    /// let made = request.run("postgres://localhost/agmen", &github).await?;
    /// println!("project {}", made.project);
    /// # Ok(()) }
    /// ```
    pub async fn run(
        &self,
        database_url: &str,
        github_api_url: &Url,
    ) -> Result<Bootstrapped, BootstrapError> {
        let (id, login) = self.resolve(github_api_url).await?;
        let pool = db::connect(database_url).await?;
        let made = self.write(&pool, id, login).await;
        pool.close().await;
        made
    }

    /// The admin's account id, and their login when it was given.
    async fn resolve(
        &self,
        github_api_url: &Url,
    ) -> Result<(GithubId, Option<String>), BootstrapError> {
        let login = match &self.admin {
            AdminGithub::Id(id) => return Ok((*id, None)),
            AdminGithub::Login(login) => login,
        };
        let http = upstream_client().map_err(BootstrapError::HttpClient)?;
        let account = account_by_login(&http, github_api_url, login)
            .await
            .map_err(BootstrapError::Github)?
            .ok_or_else(|| BootstrapError::UnknownLogin(login.clone()))?;
        Ok((account.id, Some(checked_login(&account.login)?)))
    }

    async fn write(
        &self,
        pool: &PgPool,
        admin_id: GithubId,
        admin_login: Option<String>,
    ) -> Result<Bootstrapped, BootstrapError> {
        if self.mode == HubMode::SelfHost {
            let group = group::ensure_group(pool, &self.tenant).await?.tenant;
            let name: String = sqlx::query_scalar("SELECT name FROM tenants WHERE id = $1")
                .bind(group.as_str())
                .fetch_one(pool)
                .await?;
            if name != self.tenant {
                return Err(BootstrapError::OtherGroup { group: name });
            }
        }
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
        let project = insert_project(&mut tx, &tenant, &self.project, true, None)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
        if let Some(repo_url) = &self.repo_url {
            sqlx::query("UPDATE projects SET repo_url = $1 WHERE id = $2")
                .bind(repo_url.as_str())
                .bind(&project)
                .execute(&mut *tx)
                .await?;
        }
        let repo_url: Option<String> =
            sqlx::query_scalar("SELECT repo_url FROM projects WHERE id = $1")
                .bind(&project)
                .fetch_one(&mut *tx)
                .await?;
        let repo_url = repo_url.map(RepoUrl::try_from).transpose()?;
        // A claimed row stays claimed, so a re-run is a no-op.
        sqlx::query(
            "INSERT INTO bootstrap_admins (project_id, github_id, github_login)
             VALUES ($1, $2, $3)
             ON CONFLICT (project_id, github_id) DO NOTHING",
        )
        .bind(&project)
        .bind(admin_id.to_string())
        .bind(&admin_login)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Bootstrapped {
            tenant: tenant.parse()?,
            project: project.parse()?,
            admin_github_id: admin_id,
            admin_github: admin_login,
            repo_url,
        })
    }
}

/// Shared insert: bootstrap finds an existing project; HTTP creation refuses it.
pub(crate) async fn insert_project(
    conn: &mut PgConnection,
    tenant: &str,
    name: &str,
    reuse: bool,
    repo_url: Option<&RepoUrl>,
) -> Result<Option<String>, BootstrapError> {
    Ok(sqlx::query_scalar(
        "INSERT INTO projects (id, tenant_id, name, repo_url) VALUES ($1, $2, $3, $5)
         ON CONFLICT (tenant_id, name) DO UPDATE SET name = EXCLUDED.name
         WHERE $4
         RETURNING id",
    )
    .bind(new_id()?)
    .bind(tenant)
    .bind(name)
    .bind(reuse)
    .bind(repo_url.map(RepoUrl::as_str))
    .fetch_optional(conn)
    .await?)
}

/// `login` lowercased, if it is one GitHub allows.
fn checked_login(login: &str) -> Result<String, BootstrapError> {
    let login = login.to_ascii_lowercase();
    if is_human_handle(&login) {
        Ok(login)
    } else {
        Err(BootstrapError::BadLogin(login))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(login: &str) -> AdminGithub {
        AdminGithub::Login(login.to_owned())
    }

    #[test]
    fn names_and_logins() {
        let ok = Bootstrap::new("Acme Co", "agmen", login("Andy-A")).unwrap();
        assert_eq!(ok.admin, login("andy-a"));
        let id: GithubId = "42".parse().unwrap();
        assert_eq!(
            Bootstrap::new("t", "p", AdminGithub::Id(id)).unwrap().admin,
            AdminGithub::Id(id)
        );
        assert!(Bootstrap::new("x".repeat(64), "p", login("a")).is_ok());
        for (tenant, project) in [("", "p"), (" t", "p"), ("t", "p "), ("t", "a\nb")] {
            assert!(
                matches!(
                    Bootstrap::new(tenant, project, login("andy")),
                    Err(BootstrapError::BadName { .. })
                ),
                "{tenant:?} {project:?}"
            );
        }
        assert!(Bootstrap::new("x".repeat(65), "p", login("a")).is_err());
        for bad in ["", "-andy", "andy/x", "andy_a", &"a".repeat(40)] {
            assert!(
                matches!(
                    Bootstrap::new("t", "p", login(bad)),
                    Err(BootstrapError::BadLogin(_))
                ),
                "{bad:?}"
            );
        }
    }
}
