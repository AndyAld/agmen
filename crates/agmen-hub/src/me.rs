//! `GET /v1/me`: who the signed-in human is, where clients reach the
//! broker, and the projects they belong to (rulings R56 and R57). `agmen
//! join` needs the handle to name `<handle>/<name>` and the id to write as
//! `owner_id`; `agmen whoami` lists the projects.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::auth::Caller;
use crate::db::{Role, TenantRole};
use crate::groups::{self, owner_notice, shown_plan};
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
    /// By name, then tenant id.
    groups: Vec<Group>,
    /// Owner notices on a hosted hub. Omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    notices: Vec<String>,
}

/// One group the human belongs to.
///
/// `state` and `trial_ends_at` are omitted when the group has no plan row,
/// which is every self-host group. `trial_ends_at` is present only while
/// `state` is `trial`.
#[derive(Debug, Serialize)]
struct Group {
    tenant: String,
    name: String,
    role: TenantRole,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trial_ends_at: Option<agmen_proto::Timestamp>,
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
    let (groups, notices) = listed_groups(&hub, human.as_str()).await?;
    Ok(Json(Me {
        id: human.as_str().to_owned(),
        handle,
        broker_url: hub.public_mqtt_url().as_str().to_owned(),
        projects,
        groups,
        notices,
    }))
}

async fn listed_groups(hub: &Hub, human: &str) -> Result<(Vec<Group>, Vec<String>), ApiError> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT t.id, t.name, m.role
         FROM tenant_members m JOIN tenants t ON t.id = m.tenant_id
         WHERE m.human_id = $1 ORDER BY t.name, t.id",
    )
    .bind(human)
    .fetch_all(hub.pool())
    .await?;
    let ids: Vec<String> = rows.iter().map(|(id, _, _)| id.clone()).collect();
    let plans = groups::load_plans(hub.pool(), &ids).await?;
    let now = agmen_proto::Timestamp::now();
    let hosted = hub.mode() == crate::config::HubMode::Hosted;
    let mut groups = Vec::with_capacity(rows.len());
    let mut notices = Vec::new();
    for (tenant, name, role) in rows {
        let role = role.parse().map_err(ApiError::stored)?;
        let plan = plans.get(&tenant);
        let (state, trial_ends_at) = match plan {
            Some(record) => {
                let (state, ends) = shown_plan(record);
                (Some(state), ends)
            }
            None => (None, None),
        };
        if hosted
            && role == TenantRole::Owner
            && let Some(record) = plan
            && let Some(notice) = owner_notice(&name, record, now)
        {
            notices.push(notice);
        }
        groups.push(Group {
            tenant,
            name,
            role,
            state,
            trial_ends_at,
        });
    }
    Ok((groups, notices))
}
