//! Project creation for tenant owners and admins.

use std::str::FromStr;

use agmen_proto::{RepoUrl, TenantId};
use serde::Serialize;
use sqlx::PgConnection;

use crate::bootstrap::{BootstrapError, insert_project};
use crate::db::is_display_name;
use crate::http::ApiError;
use crate::identities::HumanId;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateProject {
    name: String,
    tenant: Option<TenantId>,
    repo_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct NewProject {
    pub(crate) tenant: String,
    pub(crate) project: String,
    pub(crate) name: String,
    pub(crate) repo_url: Option<RepoUrl>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CreatedProject {
    #[serde(flatten)]
    project: NewProject,
    role: crate::db::Role,
}

fn project_tenant(
    admin_tenants: &[String],
    tenant: Option<&str>,
) -> Result<String, crate::http::ApiError> {
    use crate::http::ApiError;
    let forbidden = || {
        ApiError::forbidden(
            "admin_required",
            "only a group owner or admin can create a project in this group",
        )
    };
    if let Some(tenant) = tenant {
        return admin_tenants
            .iter()
            .find(|id| id.as_str() == tenant)
            .cloned()
            .ok_or_else(forbidden);
    }
    match admin_tenants {
        [] => Err(forbidden()),
        [only] => Ok(only.clone()),
        _ => Err(ApiError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "tenant_required",
            "name the tenant when you are an owner or admin of several groups",
        )),
    }
}

fn project_name(name: &str) -> Result<(), crate::http::ApiError> {
    if is_display_name(name) {
        return Ok(());
    }
    Err(crate::http::ApiError::new(
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_name",
        BootstrapError::BadName { what: "project" }.to_string(),
    ))
}

fn duplicate_project() -> crate::http::ApiError {
    crate::http::ApiError::conflict(
        "project_exists",
        "a project with that name already exists in this tenant",
    )
}

/// Creates the project and its admin membership on the caller's connection.
pub(crate) async fn create_project(
    conn: &mut PgConnection,
    tenant: &str,
    name: &str,
    repo_url: Option<RepoUrl>,
    creator: &HumanId,
) -> Result<NewProject, ApiError> {
    project_name(name)?;
    let project = insert_project(&mut *conn, tenant, name, false, repo_url.as_ref())
        .await
        .map_err(|error| match error {
            BootstrapError::Database(error) => ApiError::Database(error),
            BootstrapError::Random(error) => ApiError::Random(error),
            error => ApiError::stored(error),
        })?
        .ok_or_else(duplicate_project)?;
    sqlx::query("INSERT INTO memberships (human_id, project_id, role) VALUES ($1, $2, 'admin')")
        .bind(creator.as_str())
        .bind(&project)
        .execute(conn)
        .await?;
    Ok(NewProject {
        tenant: tenant.to_owned(),
        project,
        name: name.to_owned(),
        repo_url,
    })
}

/// `POST /v1/projects`: atomically create the project and its admin membership.
pub(crate) async fn create(
    axum::extract::State(hub): axum::extract::State<crate::http::Hub>,
    caller: crate::auth::Caller,
    axum::Json(request): axum::Json<CreateProject>,
) -> Result<(axum::http::StatusCode, axum::Json<CreatedProject>), crate::http::ApiError> {
    let human = caller.human()?;
    let repo_url = request
        .repo_url
        .as_deref()
        .map(RepoUrl::from_str)
        .transpose()
        .map_err(|error| {
            ApiError::new(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "bad_repo_url",
                error.to_string(),
            )
        })?;
    let mut tx = hub.pool().begin().await?;
    // Keep the tenant memberships authorizing this write alive until commit.
    let mut tenants: Vec<String> = sqlx::query_scalar(
        "SELECT tenant_id FROM tenant_members
         WHERE human_id = $1 AND role IN ('owner', 'admin') FOR SHARE",
    )
    .bind(human.as_str())
    .fetch_all(&mut *tx)
    .await?;
    tenants.sort();
    tenants.dedup();
    let tenant = project_tenant(&tenants, request.tenant.as_ref().map(TenantId::as_str))?;
    let project = create_project(&mut tx, &tenant, &request.name, repo_url, &human).await?;
    tx.commit().await?;
    Ok((
        axum::http::StatusCode::CREATED,
        axum::Json(CreatedProject {
            project,
            role: crate::db::Role::Admin,
        }),
    ))
}

#[cfg(test)]
mod project_creation_tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse as _;

    #[test]
    fn project_creation_selects_only_admin_tenants() {
        assert_eq!(
            project_tenant(&["tenant01".into()], None).unwrap(),
            "tenant01"
        );
        assert_eq!(
            project_tenant(&["tenant01".into(), "tenant02".into()], Some("tenant02")).unwrap(),
            "tenant02"
        );
        assert_eq!(
            project_tenant(&[], None)
                .unwrap_err()
                .into_response()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            project_tenant(&["tenant01".into()], Some("tenant02"))
                .unwrap_err()
                .into_response()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            project_tenant(&["tenant01".into(), "tenant02".into()], None)
                .unwrap_err()
                .into_response()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn project_creation_reuses_bootstrap_names_and_rejects_duplicates() {
        for name in ["marked", "A project", &"x".repeat(64)] {
            assert!(project_name(name).is_ok());
        }
        for name in ["", " space", "space ", "a\nb", &"x".repeat(65)] {
            assert_eq!(
                project_name(name).unwrap_err().into_response().status(),
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        assert_eq!(
            duplicate_project().into_response().status(),
            StatusCode::CONFLICT
        );
    }
}
