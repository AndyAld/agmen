//! The hosted plan clock: trial reminders, read-only, and group deletion.
#![cfg(test)]

mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use agmen_hub::{BillingEvent, HUB_IDENTITY, HubMode, Plan, PlanRecord, PlanState};
use agmen_proto::{DeliveryReply, IdentityId, NackReason, StampedEnvelope, TenantId, Timestamp};
use reqwest::StatusCode;
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool};
use support::mqtt::{self, connect_ok};
use support::{
    Ctx, Project, broker_token, envelope, login, mint_ok, run_hosted, run_in_mode, ulid,
};

const DAY: u64 = 86_400;
const MINUTE: u64 = 60;

/// Tables a deleted group's rows must leave, each of which this file fills.
const FILLED_TABLES: [&str; 15] = [
    "bootstrap_admins",
    "claims",
    "identities",
    "invites",
    "item_counters",
    "items",
    "lane_lends",
    "lanes",
    "memberships",
    "messages",
    "projects",
    "publish_outbox",
    "tenant_members",
    "tenant_plans",
    "tenant_usage",
];

/// Whole seconds, so a stored microsecond timestamp reads back the same.
fn now() -> Timestamp {
    Timestamp::from_second(Timestamp::now().as_second()).unwrap()
}

fn ago(now: Timestamp, days: u64) -> Timestamp {
    now - Duration::from_secs(days * DAY + MINUTE)
}

struct Group {
    handle: String,
    token: String,
    owner: String,
    tenant: TenantId,
    projects: Vec<String>,
}

impl Group {
    fn project(&self, index: usize) -> Project {
        Project {
            tenant: self.tenant.as_str().to_owned(),
            project: self.projects[index].clone(),
        }
    }
}

async fn me(ctx: &Ctx, token: &str) -> Value {
    ctx.http
        .get(ctx.hub.url("/v1/me"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn create_group(ctx: &Ctx, token: &str, name: &str) -> (StatusCode, Value) {
    let response = ctx
        .http
        .post(ctx.hub.url("/v1/groups"))
        .bearer_auth(token)
        .json(&json!({"name": name, "project": "web"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// A trial group made through the API, with `projects` projects in all.
async fn group(ctx: &Ctx, sub: u64, handle: &str, projects: usize) -> Group {
    let token = login(ctx, sub, handle).await;
    let (status, body) = create_group(ctx, &token, "acme").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut ids = vec![body["project"].as_str().unwrap().to_owned()];
    for index in 1..projects {
        let response = ctx
            .http
            .post(ctx.hub.url("/v1/projects"))
            .bearer_auth(&token)
            .json(&json!({"name": format!("extra{index}")}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: Value = response.json().await.unwrap();
        ids.push(body["project"].as_str().unwrap().to_owned());
    }
    let owner = me(ctx, &token).await["id"].as_str().unwrap().to_owned();
    Group {
        handle: handle.to_owned(),
        token,
        owner,
        tenant: body["tenant"].as_str().unwrap().parse().unwrap(),
        projects: ids,
    }
}

async fn plan_state(ctx: &Ctx, tenant: &TenantId) -> Option<String> {
    sqlx::query_scalar("SELECT state FROM tenant_plans WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .fetch_optional(&ctx.pool)
        .await
        .unwrap()
}

async fn single_count(pool: &PgPool, sql: &'static str, bind: &str) -> i64 {
    support::count(pool, sql, bind).await
}

async fn feed_alerts(ctx: &Ctx, project: &str) -> Vec<StampedEnvelope> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT envelope::text FROM messages WHERE project_id = $1 AND stream = 'feed'
         AND from_id = $2 AND envelope->>'verb' = 'ALERT' ORDER BY seq LIMIT 51",
    )
    .bind(project)
    .bind(HUB_IDENTITY)
    .fetch_all(&ctx.pool)
    .await
    .unwrap();
    rows.iter()
        .map(|raw| serde_json::from_str(raw).unwrap())
        .collect()
}

fn active_team(at: Timestamp) -> PlanRecord {
    PlanRecord::trial(at).apply(BillingEvent::Active(Plan::Team { extra_seats: 0 }), at)
}

fn trial_notice(name: &str, ends: Timestamp) -> String {
    format!(
        "Your agmen hosted trial of {name} ends on {ends}. Run agmen group billing to keep going."
    )
}

#[tokio::test]
async fn it_day_5_and_7_reminders() {
    run_hosted(|ctx| async move {
        let group = group(&ctx, 4242, "andy", 2).await;
        let now = now();
        let hub = &ctx.hub.hub;
        let started = ago(now, 4);
        hub.set_plan(&group.tenant, &PlanRecord::trial(started))
            .await
            .unwrap();
        assert_eq!(hub.tick_plans_at(now).await.unwrap(), 1);
        let ends = started + Duration::from_secs(7 * DAY);
        for project in &group.projects {
            let alerts = feed_alerts(&ctx, project).await;
            assert_eq!(alerts.len(), 1, "{project}");
            assert_eq!(
                alerts[0].client.body.as_str(),
                trial_notice("andy/acme", ends)
            );
            assert!(
                alerts[0]
                    .mentioned
                    .contains(&group.owner.parse::<IdentityId>().unwrap())
            );
        }
        assert_eq!(hub.tick_plans_at(now).await.unwrap(), 0);
        for project in &group.projects {
            assert_eq!(feed_alerts(&ctx, project).await.len(), 1);
        }
        let started = ago(now, 6);
        let mut record = PlanRecord::trial(started);
        record.reminders_sent = 1;
        hub.set_plan(&group.tenant, &record).await.unwrap();
        assert_eq!(hub.tick_plans_at(now).await.unwrap(), 1);
        for project in &group.projects {
            let alerts = feed_alerts(&ctx, project).await;
            assert_eq!(alerts.len(), 2, "{project}");
            assert!(alerts[1].client.body.as_str().contains(&format!(
                "ends on {}",
                started + Duration::from_secs(7 * DAY)
            )));
        }
    })
    .await;
}

#[tokio::test]
async fn it_trial_end_goes_read_only() {
    run_hosted(|ctx| async move {
        let group = group(&ctx, 4242, "andy", 1).await;
        let project = group.project(0);
        let agent = mint_ok(
            &ctx,
            &group.token,
            &format!("{}/sol", group.handle),
            &project,
        )
        .await;
        let hub = &ctx.hub.hub;
        let now = now();
        hub.set_plan(&group.tenant, &PlanRecord::trial(ago(now, 7)))
            .await
            .unwrap();
        assert_eq!(hub.tick_plans_at(now).await.unwrap(), 1);
        let body = me(&ctx, &group.token).await;
        assert_eq!(body["groups"][0]["state"], "read_only", "{body}");
        let cause: String =
            sqlx::query_scalar("SELECT read_only_cause FROM tenant_plans WHERE tenant_id = $1")
                .bind(group.tenant.as_str())
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(cause, "trial_ended");
        let topics = support::topics(&project);
        let raw = envelope(&ulid(), "FYI", json!({}), json!({}));
        let reply = hub
            .ingest(&topics, &agent.id.parse().unwrap(), &raw)
            .await
            .unwrap();
        assert!(
            matches!(&reply, DeliveryReply::Nack { reason, .. } if *reason == NackReason::ReadOnly),
            "{reply:?}"
        );
        assert!(feed_alerts(&ctx, &group.projects[0]).await.is_empty());
    })
    .await;
}

#[tokio::test]
async fn it_unpaid_trial_deleted_after_14_days() {
    run_hosted(|ctx| async move {
        let group = group(&ctx, 4242, "andy", 1).await;
        let now = now();
        let hub = &ctx.hub.hub;
        hub.set_plan(&group.tenant, &PlanRecord::trial(ago(now, 21)))
            .await
            .unwrap();
        assert_eq!(hub.tick_plans_at(now).await.unwrap(), 1);
        assert_eq!(
            single_count(
                &ctx.pool,
                "SELECT count(*) FROM tenants WHERE id = $1",
                group.tenant.as_str()
            )
            .await,
            0
        );
        let body = me(&ctx, &group.token).await;
        assert_eq!(body["groups"].as_array().unwrap().len(), 0, "{body}");
        let grants: i64 = sqlx::query_scalar("SELECT count(*) FROM trial_grants")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(grants, 1);
    })
    .await;
}

#[tokio::test]
async fn it_past_due_read_only_after_14_days() {
    run_hosted(|ctx| async move {
        let group = group(&ctx, 4242, "andy", 1).await;
        let now = now();
        let hub = &ctx.hub.hub;
        let past_due = |days: u64| {
            active_team(now - Duration::from_secs(300 * DAY))
                .apply(BillingEvent::PastDue, ago(now, days))
        };
        hub.set_plan(&group.tenant, &past_due(14)).await.unwrap();
        assert_eq!(hub.tick_plans_at(now).await.unwrap(), 1);
        assert_eq!(
            plan_state(&ctx, &group.tenant).await.as_deref(),
            Some("read_only")
        );
        let cause: String =
            sqlx::query_scalar("SELECT read_only_cause FROM tenant_plans WHERE tenant_id = $1")
                .bind(group.tenant.as_str())
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(cause, "payment_failed");
        hub.set_plan(&group.tenant, &past_due(104)).await.unwrap();
        for _ in 0..3 {
            if hub.tick_plans_at(now).await.unwrap() == 0 {
                break;
            }
        }
        assert_eq!(
            single_count(
                &ctx.pool,
                "SELECT count(*) FROM tenants WHERE id = $1",
                group.tenant.as_str()
            )
            .await,
            0
        );
    })
    .await;
}

async fn row_text(ctx: &Ctx, tenant: &TenantId) -> Option<String> {
    sqlx::query_scalar("SELECT tenant_plans::text FROM tenant_plans WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .fetch_optional(&ctx.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn it_tick_skips_active_and_operator_groups() {
    run_hosted(|ctx| async move {
        let group = group(&ctx, 4242, "andy", 1).await;
        let now = now();
        let hub = &ctx.hub.hub;
        hub.set_plan(&group.tenant, &active_team(ago(now, 400)))
            .await
            .unwrap();
        let operator = support::seed_project(&ctx.pool).await;
        let before = row_text(&ctx, &group.tenant).await;
        assert!(before.is_some());
        assert_eq!(hub.tick_plans_at(now).await.unwrap(), 0);
        assert_eq!(row_text(&ctx, &group.tenant).await, before);
        for (sql, bind) in [
            (
                "SELECT count(*) FROM tenants WHERE id = $1",
                operator.tenant.as_str(),
            ),
            (
                "SELECT count(*) FROM projects WHERE tenant_id = $1",
                operator.tenant.as_str(),
            ),
            (
                "SELECT count(*) FROM tenants WHERE id = $1",
                group.tenant.as_str(),
            ),
            (
                "SELECT count(*) FROM projects WHERE tenant_id = $1",
                group.tenant.as_str(),
            ),
        ] {
            assert_eq!(single_count(&ctx.pool, sql, bind).await, 1, "{sql} {bind}");
        }
        let plans = single_count(
            &ctx.pool,
            "SELECT count(*) FROM tenant_plans WHERE tenant_id = $1",
            &operator.tenant,
        )
        .await;
        assert_eq!(plans, 0);
    })
    .await;
}

/// What the walk of one group found: rows per table naming the tenant or
/// its projects.
type Walk = BTreeMap<String, i64>;

/// Counts rows in every `public` table with a `tenant_id` or `project_id`
/// column, plus `tenants`, `stream_heads` and the identity tokens.
async fn walk(
    pool: &PgPool,
    tenant: &TenantId,
    projects: &[String],
    identities: &[String],
) -> Walk {
    let columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.table_name::text, c.column_name::text
         FROM information_schema.columns c
         JOIN information_schema.tables t
           ON t.table_schema = c.table_schema AND t.table_name = c.table_name
         WHERE c.table_schema = 'public' AND t.table_type = 'BASE TABLE'
           AND c.column_name IN ('tenant_id', 'project_id')
         ORDER BY 1, 2",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut found = Walk::new();
    for (table, column) in columns {
        let sql = if column == "tenant_id" {
            format!("SELECT count(*) FROM \"{table}\" WHERE tenant_id = $1")
        } else {
            format!("SELECT count(*) FROM \"{table}\" WHERE project_id = ANY($1)")
        };
        let rows: i64 = if column == "tenant_id" {
            sqlx::query_scalar(AssertSqlSafe(sql))
                .bind(tenant.as_str())
                .fetch_one(pool)
                .await
                .unwrap()
        } else {
            sqlx::query_scalar(AssertSqlSafe(sql))
                .bind(projects)
                .fetch_one(pool)
                .await
                .unwrap()
        };
        *found.entry(table).or_default() += rows;
    }
    let tenants: i64 = sqlx::query_scalar("SELECT count(*) FROM tenants WHERE id = $1")
        .bind(tenant.as_str())
        .fetch_one(pool)
        .await
        .unwrap();
    found.insert("tenants".into(), tenants);
    let heads: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stream_heads s JOIN unnest($1::text[]) p(id)
         ON left(s.stream, length(p.id) + 1) = p.id || '/'",
    )
    .bind(projects)
    .fetch_one(pool)
    .await
    .unwrap();
    found.insert("stream_heads".into(), heads);
    let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM tokens WHERE identity_id = ANY($1)")
        .bind(identities)
        .fetch_one(pool)
        .await
        .unwrap();
    found.insert("tokens".into(), tokens);
    found
}

struct Filled {
    identities: Vec<String>,
    tokens: Vec<(String, String)>,
}

/// Fills every table the delete must clear for `group`. Two agents in the
/// first project and one in the second; `tokens` is `(id, hub token)`.
async fn fill(ctx: &Ctx, group: &Group) -> Filled {
    let hub = &ctx.hub.hub;
    let (first, second) = (group.project(0), group.project(1));
    let sol = mint_ok(ctx, &group.token, &format!("{}/sol", group.handle), &first).await;
    let dee = mint_ok(ctx, &group.token, &format!("{}/dee", group.handle), &first).await;
    let eli = mint_ok(ctx, &group.token, &format!("{}/eli", group.handle), &second).await;
    for (project, from) in [(&first, &sol), (&second, &eli)] {
        let raw = envelope(&ulid(), "FYI", json!({}), json!({}));
        let reply = hub
            .ingest(&support::topics(project), &from.id.parse().unwrap(), &raw)
            .await
            .unwrap();
        assert!(matches!(reply, DeliveryReply::Ack { .. }), "{reply:?}");
    }
    hub.set_usage(&group.tenant, 0, 3).await.unwrap();
    let project = &group.projects[0];
    sqlx::query("UPDATE identities SET delegated_by = $1 WHERE id = $2")
        .bind(&sol.id)
        .bind(&dee.id)
        .execute(&ctx.pool)
        .await
        .unwrap();
    for sql in [
        "INSERT INTO lanes (project_id, name, globs, owner_identity_id, v)
         VALUES ($1, 'L-UI', ARRAY['src/**'], $2, 1)",
        "INSERT INTO items (project_id, key, title, created_by, assigned_to)
         VALUES ($1, 'B-1', 'work', $2, $2)",
        "INSERT INTO item_counters (project_id, prefix, n) VALUES ($1, 'B', 1)",
        "INSERT INTO claims (project_id, item_key, identity_id, lease_until)
         VALUES ($1, 'B-1', $2, now() + interval '1 hour')",
        "INSERT INTO lane_lends (project_id, lane, message_id, from_id, to_id, paths, until_at)
         VALUES ($1, 'L-UI', 'm1', $2, $2, ARRAY['src/**'], now() + interval '1 hour')",
        "INSERT INTO bootstrap_admins (project_id, github_id) VALUES ($1, '99')",
    ] {
        sqlx::query(sql)
            .bind(project)
            .bind(&sol.id)
            .execute(&ctx.pool)
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO invites (code_hash, tenant_id, project_id, role, created_by, expires_at)
         VALUES ($1, $2, $3, 'member', $4, now() + interval '1 day')",
    )
    .bind(ulid().into_bytes())
    .bind(group.tenant.as_str())
    .bind(project)
    .bind(&group.owner)
    .execute(&ctx.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO publish_outbox (tenant_id, project_id, topic, payload)
         VALUES ($1, $2, 'agmen/v1/t/p/state/x', '\\x00')",
    )
    .bind(group.tenant.as_str())
    .bind(project)
    .execute(&ctx.pool)
    .await
    .unwrap();
    Filled {
        identities: vec![sol.id.clone(), dee.id.clone(), eli.id.clone()],
        tokens: vec![
            (sol.id, sol.token),
            (dee.id, dee.token),
            (eli.id, eli.token),
        ],
    }
}

/// Lifts the bans a deletion left on the shared dev broker.
async fn unban(ctx: &Ctx, filled: &Filled) {
    for id in &filled.identities {
        support::unban_username(&ctx.http, id).await;
    }
}

fn walk_report(walk: &Walk) -> String {
    format!("{walk:?}")
}

#[tokio::test]
async fn it_delete_group_leaves_no_rows() {
    run_hosted(|ctx| async move {
        let group = group(&ctx, 4242, "andy", 2).await;
        let filled = fill(&ctx, &group).await;
        let hub = &ctx.hub.hub;
        let before = walk(
            &ctx.pool,
            &group.tenant,
            &group.projects,
            &filled.identities,
        )
        .await;
        for table in FILLED_TABLES
            .iter()
            .copied()
            .chain(["tenants", "stream_heads", "tokens"])
        {
            assert!(
                before[table] > 0,
                "{table} was not filled: {}",
                walk_report(&before)
            );
        }
        let humans: i64 = sqlx::query_scalar("SELECT count(*) FROM humans")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        let (id, token) = filled.tokens[0].clone();
        let (status, body) = broker_token(&ctx.http, &ctx.hub, &id, &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let mut client = connect_ok(mqtt::options(
            &format!("{id}-mcp"),
            &id,
            body["token"].as_str().unwrap(),
        ))
        .await;

        hub.delete_group(&group.tenant).await.unwrap();

        let after = walk(
            &ctx.pool,
            &group.tenant,
            &group.projects,
            &filled.identities,
        )
        .await;
        assert!(
            after.values().all(|rows| *rows == 0),
            "rows left after delete_group: {}",
            walk_report(&after)
        );
        assert!(
            client.closed_within(Duration::from_secs(2)).await.is_ok(),
            "the connected client stayed connected"
        );
        let (status, body) = broker_token(&ctx.http, &ctx.hub, &id, &token).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM humans")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(kept, humans, "humans must stay");
        // A second delete of a group that is gone is fine.
        hub.delete_group(&group.tenant).await.unwrap();
        unban(&ctx, &filled).await;
    })
    .await;
}

#[tokio::test]
async fn it_delete_retries_after_a_broker_failure() {
    run_hosted(|ctx| async move {
        let group = group(&ctx, 4242, "andy", 2).await;
        let filled = fill(&ctx, &group).await;
        let hub = &ctx.hub.hub;
        let mut record = PlanRecord::trial(ago(now(), 30));
        record.state = PlanState::Deleted;
        hub.set_plan(&group.tenant, &record).await.unwrap();
        let before = walk(
            &ctx.pool,
            &group.tenant,
            &group.projects,
            &filled.identities,
        )
        .await;
        assert!(before["lanes"] > 0, "{}", walk_report(&before));
        assert_eq!(hub.tick_plans_at(now()).await.unwrap(), 1);
        let after = walk(
            &ctx.pool,
            &group.tenant,
            &group.projects,
            &filled.identities,
        )
        .await;
        assert!(
            after.values().all(|rows| *rows == 0),
            "{}",
            walk_report(&after)
        );
        assert_eq!(hub.tick_plans_at(now()).await.unwrap(), 0);
        unban(&ctx, &filled).await;
    })
    .await;
}

#[tokio::test]
async fn it_other_groups_untouched() {
    run_hosted(|ctx| async move {
        let doomed = group(&ctx, 4242, "andy", 2).await;
        let kept = group(&ctx, 7777, "blair", 2).await;
        let doomed_filled = fill(&ctx, &doomed).await;
        let kept_filled = fill(&ctx, &kept).await;
        let before = walk(
            &ctx.pool,
            &kept.tenant,
            &kept.projects,
            &kept_filled.identities,
        )
        .await;
        ctx.hub.hub.delete_group(&doomed.tenant).await.unwrap();
        let gone = walk(
            &ctx.pool,
            &doomed.tenant,
            &doomed.projects,
            &doomed_filled.identities,
        )
        .await;
        assert!(
            gone.values().all(|rows| *rows == 0),
            "{}",
            walk_report(&gone)
        );
        let after = walk(
            &ctx.pool,
            &kept.tenant,
            &kept.projects,
            &kept_filled.identities,
        )
        .await;
        assert_eq!(before, after);
        assert!(after["messages"] > 0, "{}", walk_report(&after));
        unban(&ctx, &doomed_filled).await;
    })
    .await;
}

#[tokio::test]
async fn it_trial_grant_survives_deletion() {
    run_hosted(|ctx| async move {
        let group = group(&ctx, 42, "andy", 1).await;
        let now = now();
        let hub = &ctx.hub.hub;
        hub.set_plan(&group.tenant, &PlanRecord::trial(ago(now, 21)))
            .await
            .unwrap();
        assert_eq!(hub.tick_plans_at(now).await.unwrap(), 1);
        let (status, body) = create_group(&ctx, &group.token, "acme").await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["state"], "read_only", "{body}");
        let grants: i64 = sqlx::query_scalar("SELECT count(*) FROM trial_grants")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(grants, 1);
    })
    .await;
}

#[tokio::test]
async fn it_self_host_clock_does_nothing() {
    run_in_mode(HubMode::SelfHost, |ctx| async move {
        let hub = &ctx.hub.hub;
        let stale = hub.seed_project().await.unwrap();
        hub.set_plan(stale.tenant(), &PlanRecord::trial(ago(now(), 30)))
            .await
            .unwrap();
        assert_eq!(hub.tick_plans_at(now()).await.unwrap(), 0);
        assert_eq!(
            single_count(
                &ctx.pool,
                "SELECT count(*) FROM tenants WHERE id = $1",
                stale.tenant().as_str()
            )
            .await,
            1
        );
        assert_eq!(
            single_count(
                &ctx.pool,
                "SELECT count(*) FROM tenant_plans WHERE state = 'trial' AND tenant_id = $1",
                stale.tenant().as_str()
            )
            .await,
            1
        );
    })
    .await;
}
