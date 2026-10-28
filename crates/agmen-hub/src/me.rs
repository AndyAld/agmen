//! `GET /v1/me`: who the signed-in human is, where clients reach the
//! broker, and the projects they belong to (rulings R56 and R57). `agmen
//! join` needs the handle to name `<handle>/<name>` and the id to write as
//! `owner_id`; `agmen whoami` lists the projects.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::auth::Caller;
use crate::db::Role;
use crate::http::{ApiError, Hub};

/// `GET /v1/me`'s answer.
#[derive(Debug, Serialize)]
pub(crate) struct Me {
    id: String,
    handle: String,
    /// `AGMEN_PUBLIC_MQTT_URL`.
    broker_url: String,
    /// By name.
    projects: Vec<Membership>,
}

/// One project the human belongs to.
#[derive(Debug, Serialize)]
struct Membership {
    tenant: String,
    project: String,
    name: String,
    role: Role,
}

/// `GET /v1/me` (human token; an identity token gets 403).
pub(crate) async fn me(State(hub): State<Hub>, caller: Caller) -> Result<Json<Me>, ApiError> {
    let human = caller.human()?;
    let handle: String = sqlx::query_scalar("SELECT handle FROM humans WHERE id = $1")
        .bind(human.as_str())
        .fetch_one(hub.pool())
        .await?;
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT p.tenant_id, p.id, p.name, m.role
         FROM memberships m JOIN projects p ON p.id = m.project_id
         WHERE m.human_id = $1
         ORDER BY p.name, p.tenant_id, p.id",
    )
    .bind(human.as_str())
    .fetch_all(hub.pool())
    .await?;
    let projects = rows
        .into_iter()
        .map(|(tenant, project, name, role)| {
            Ok(Membership {
                tenant,
                project,
                name,
                role: role.parse().map_err(ApiError::stored)?,
            })
        })
        .collect::<Result<_, ApiError>>()?;
    Ok(Json(Me {
        id: human.as_str().to_owned(),
        handle,
        broker_url: hub.public_mqtt_url().as_str().to_owned(),
        projects,
    }))
}
