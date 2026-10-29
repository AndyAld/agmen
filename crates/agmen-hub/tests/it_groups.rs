//! Hosted group creation, the one trial per GitHub account, and the
//! three-group ownership limit.
#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_hub::{MIGRATOR, SetPlanError, TenantRole};
use reqwest::StatusCode;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use support::{Ctx, Project, TestDb, login, mint_ok, run, run_hosted, seed_project};

const GROUP_LIMIT: &str = "you already own 3 groups, the most one account can own";
const SECS_PER_DAY: u64 = 86_400;

async fn post_group(ctx: &Ctx, token: &str, body: &Value) -> (StatusCode, Value) {
    let response = ctx
        .http
        .post(ctx.hub.url("/v1/groups"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
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

async fn count(pool: &sqlx::PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

fn assert_close(actual: &str, expected: jiff::Timestamp) {
    let parsed: jiff::Timestamp = actual.parse().unwrap();
    let delta = (parsed.as_microsecond() - expected.as_microsecond()).unsigned_abs();
    assert!(delta <= 5_000_000, "{parsed} vs {expected}");
}

async fn create_named(ctx: &Ctx, token: &str, name: &str) -> Value {
    let (status, body) = post_group(ctx, token, &json!({"name": name, "project": "web"})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

#[tokio::test]
async fn it_first_group_starts_trial() {
    run_hosted(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        let now = jiff::Timestamp::now();
        let (status, body) =
            post_group(&ctx, &token, &json!({"name": "acme", "project": "web"})).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["group"], "andy/acme");
        assert_eq!(body["state"], "trial");
        assert!(body["checkout_url"].is_null());
        assert_close(
            body["trial_ends_at"].as_str().unwrap(),
            now + Duration::from_secs(7 * SECS_PER_DAY),
        );
        let human = me(&ctx, &token).await;
        let human_id = human["id"].as_str().unwrap();
        let role: String = sqlx::query_scalar(
            "SELECT role FROM tenant_members WHERE tenant_id = $1 AND human_id = $2",
        )
        .bind(body["tenant"].as_str().unwrap())
        .bind(human_id)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(role, "owner");
        let project_role: String = sqlx::query_scalar(
            "SELECT role FROM memberships WHERE project_id = $1 AND human_id = $2",
        )
        .bind(body["project"].as_str().unwrap())
        .bind(human_id)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(project_role, "admin");
        let sub: String = sqlx::query_scalar("SELECT provider_sub FROM humans WHERE id = $1")
            .bind(human_id)
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        let grant: String = sqlx::query_scalar("SELECT github_id FROM trial_grants")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(grant, sub);
        assert_eq!(sub, "4242");
    })
    .await;
}

#[tokio::test]
async fn it_second_group_gets_no_trial() {
    run_hosted(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        create_named(&ctx, &token, "acme").await;
        let now = jiff::Timestamp::now();
        let body = create_named(&ctx, &token, "beta").await;
        assert_eq!(body["state"], "read_only");
        assert!(body["trial_ends_at"].is_null());
        assert!(body["checkout_url"].is_null());
        let row: (String, String, i64) = sqlx::query_as(
            "SELECT state, read_only_cause,
                    (extract(epoch FROM read_only_since) * 1000000)::bigint
             FROM tenant_plans WHERE tenant_id = $1",
        )
        .bind(body["tenant"].as_str().unwrap())
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "read_only");
        assert_eq!(row.1, "awaiting_payment");
        let since = jiff::Timestamp::from_microsecond(row.2).unwrap();
        let delete_at = agmen_hub::PlanRecord::awaiting_payment(since)
            .delete_at()
            .unwrap();
        assert_close(
            &delete_at.to_string(),
            now + Duration::from_secs(14 * SECS_PER_DAY),
        );
        assert_eq!(
            count(&ctx.pool, "SELECT count(*) FROM trial_grants").await,
            1
        );
    })
    .await;
}

#[tokio::test]
async fn it_fourth_owned_group_refused() {
    run_hosted(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        for name in ["one", "two", "three"] {
            create_named(&ctx, &token, name).await;
        }
        let before = count(&ctx.pool, "SELECT count(*) FROM tenants").await;
        let (status, body) =
            post_group(&ctx, &token, &json!({"name": "four", "project": "web"})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "group_limit");
        assert_eq!(body["error_description"], GROUP_LIMIT);
        assert_eq!(
            count(&ctx.pool, "SELECT count(*) FROM tenants").await,
            before
        );
    })
    .await;
}

#[tokio::test]
async fn it_transfer_refused_when_receiver_owns_three() {
    run_hosted(|ctx| async move {
        let andy = login(&ctx, 4242, "andy").await;
        let blair = login(&ctx, 7777, "blair").await;
        let owned = create_named(&ctx, &andy, "acme").await;
        for name in ["one", "two", "three"] {
            create_named(&ctx, &blair, name).await;
        }
        let blair_me = me(&ctx, &blair).await;
        let tenant = owned["tenant"].as_str().unwrap();
        let mut tx = ctx.pool.begin().await.unwrap();
        agmen_hub::seed_tenant_member(
            &mut tx,
            tenant,
            blair_me["id"].as_str().unwrap(),
            TenantRole::Admin,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let response = ctx
            .http
            .post(ctx.hub.url(&format!("/v1/tenants/{tenant}/transfer")))
            .bearer_auth(&andy)
            .json(&json!({"to": blair_me["handle"].as_str().unwrap()}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: Value = response.json().await.unwrap();
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "group_limit");
        assert_eq!(body["error_description"], GROUP_LIMIT);
        let andy_id = me(&ctx, &andy).await;
        let role: String = sqlx::query_scalar(
            "SELECT role FROM tenant_members WHERE tenant_id = $1 AND human_id = $2",
        )
        .bind(tenant)
        .bind(andy_id["id"].as_str().unwrap())
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(role, "owner");
    })
    .await;
}

#[tokio::test]
async fn it_same_group_name_two_owners() {
    run_hosted(|ctx| async move {
        let andy = login(&ctx, 1, "andy").await;
        let adam = login(&ctx, 2, "adam").await;
        let andy_group = create_named(&ctx, &andy, "acme").await;
        let adam_group = create_named(&ctx, &adam, "acme").await;
        assert_eq!(andy_group["group"], "andy/acme");
        assert_eq!(adam_group["group"], "adam/acme");
    })
    .await;
}

#[tokio::test]
async fn it_group_input_rules() {
    run_hosted(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        let before = (
            count(&ctx.pool, "SELECT count(*) FROM tenants").await,
            count(&ctx.pool, "SELECT count(*) FROM trial_grants").await,
        );
        let (status, body) =
            post_group(&ctx, &token, &json!({"name": "Acme", "project": "web"})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], "bad_group_name");
        let (status, body) =
            post_group(&ctx, &token, &json!({"name": "acme", "project": " x"})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], "invalid_name");
        let bad_repo = "https://u:p@github.com/a/b";
        let (status, body) = post_group(
            &ctx,
            &token,
            &json!({"name": "acme", "project": "web", "repo_url": bad_repo}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], "bad_repo_url");
        let expected = bad_repo
            .parse::<agmen_proto::RepoUrl>()
            .unwrap_err()
            .to_string();
        assert_eq!(body["error_description"], expected);
        assert_eq!(
            (
                count(&ctx.pool, "SELECT count(*) FROM tenants").await,
                count(&ctx.pool, "SELECT count(*) FROM trial_grants").await,
            ),
            before
        );
        let repo = "https://github.com/acme/web";
        let (status, body) = post_group(
            &ctx,
            &token,
            &json!({"name": "acme", "project": "web", "repo_url": repo}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let stored: String = sqlx::query_scalar("SELECT repo_url FROM projects WHERE id = $1")
            .bind(body["project"].as_str().unwrap())
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(stored, repo);
    })
    .await;
}

#[tokio::test]
async fn it_groups_not_on_self_host() {
    run(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        let claim = ctx.hub.hub.mint_claim_token().await.unwrap().unwrap();
        let response = ctx
            .http
            .post(ctx.hub.url("/v1/claim"))
            .bearer_auth(&token)
            .json(&json!({ "token": claim }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let claimed: Value = response.json().await.unwrap();
        let (status, body) =
            post_group(&ctx, &token, &json!({"name": "acme", "project": "web"})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"], "not_hosted");
        // A self-host group has no plan row, so state and trial_ends_at are
        // omitted. The coordinator ruled that out over emitting "self_host".
        let me_body = me(&ctx, &token).await;
        let groups = me_body["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(&groups[0], &claimed);
        assert!(groups[0].get("state").is_none());
        assert!(groups[0].get("trial_ends_at").is_none());
        assert!(me_body.get("notices").is_none());
    })
    .await;
}

#[tokio::test]
async fn it_me_groups_show_state() {
    run_hosted(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        let now = jiff::Timestamp::now();
        create_named(&ctx, &token, "acme").await;
        let body = me(&ctx, &token).await;
        let group = &body["groups"][0];
        assert_eq!(group["state"], "trial");
        assert_eq!(group["name"], "andy/acme");
        assert_close(
            group["trial_ends_at"].as_str().unwrap(),
            now + Duration::from_secs(7 * SECS_PER_DAY),
        );
    })
    .await;
}

#[tokio::test]
async fn it_me_notices_on_day_5() {
    run_hosted(|ctx| async move {
        let andy = login(&ctx, 4242, "andy").await;
        let now = jiff::Timestamp::now();
        let created = create_named(&ctx, &andy, "acme").await;
        let tenant: agmen_proto::TenantId = created["tenant"].as_str().unwrap().parse().unwrap();
        let started = now - Duration::from_secs(4 * SECS_PER_DAY);
        ctx.hub
            .hub
            .set_plan(&tenant, &agmen_hub::PlanRecord::trial(started))
            .await
            .unwrap();
        let owner = me(&ctx, &andy).await;
        let notices = owner["notices"].as_array().unwrap();
        assert_eq!(notices.len(), 1);
        let text = notices[0].as_str().unwrap();
        assert!(text.contains("andy/acme"), "{text}");
        assert!(text.contains("agmen group billing"), "{text}");
        // The date is the trial's end in UTC, the same instant /v1/me states.
        let ends = owner["groups"][0]["trial_ends_at"].as_str().unwrap();
        assert!(ends.ends_with('Z'), "{ends}");
        assert!(text.contains(ends), "{text} does not name {ends}");
        assert_close(ends, started + Duration::from_secs(7 * SECS_PER_DAY));
        let blair = login(&ctx, 7777, "blair").await;
        let blair_id = me(&ctx, &blair).await;
        let mut tx = ctx.pool.begin().await.unwrap();
        agmen_hub::seed_tenant_member(
            &mut tx,
            tenant.as_str(),
            blair_id["id"].as_str().unwrap(),
            TenantRole::Admin,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let admin = me(&ctx, &blair).await;
        assert!(admin.get("notices").is_none(), "{admin}");
    })
    .await;
}

/// Sends every request at once and returns the answers in the order they
/// arrive.
async fn send_at_once(requests: Vec<reqwest::RequestBuilder>) -> Vec<(StatusCode, Value)> {
    let mut sent = tokio::task::JoinSet::new();
    for request in requests {
        sent.spawn(async move {
            let response = request.send().await.unwrap();
            let status = response.status();
            (status, response.json().await.unwrap_or(Value::Null))
        });
    }
    let mut answers = Vec::new();
    while let Some(answer) = sent.join_next().await {
        answers.push(answer.unwrap());
    }
    answers
}

fn group_request(ctx: &Ctx, token: &str, name: &str) -> reqwest::RequestBuilder {
    ctx.http
        .post(ctx.hub.url("/v1/groups"))
        .bearer_auth(token)
        .json(&json!({"name": name, "project": "web"}))
}

/// Opens the hub's database connections ahead of a burst. On a cold pool the
/// requests wait for new connections one by one, so the first create commits
/// before the others count and the race never happens.
async fn warm_hub_pool(ctx: &Ctx, token: &str) {
    let mut sent = tokio::task::JoinSet::new();
    for _ in 0..12 {
        let request = ctx.http.get(ctx.hub.url("/v1/me")).bearer_auth(token);
        sent.spawn(async move { request.send().await.unwrap().status() });
    }
    while let Some(status) = sent.join_next().await {
        assert_eq!(status.unwrap(), StatusCode::OK);
    }
}

#[tokio::test]
async fn it_parallel_creates_respect_the_limit() {
    run_hosted(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        create_named(&ctx, &token, "one").await;
        create_named(&ctx, &token, "two").await;
        // Two creates at once did not hit the window with the lock removed,
        // and neither did a burst on a cold pool. Eight on a warm pool do.
        warm_hub_pool(&ctx, &token).await;
        let requests = (0..8)
            .map(|n| group_request(&ctx, &token, &format!("burst-{n}")))
            .collect();
        let answers = send_at_once(requests).await;
        let created: Vec<_> = answers
            .iter()
            .filter(|(status, _)| *status == StatusCode::CREATED)
            .collect();
        assert_eq!(created.len(), 1, "{answers:?}");
        assert_eq!(created[0].1["state"], "read_only");
        assert!(created[0].1["trial_ends_at"].is_null());
        for (status, body) in answers.iter().filter(|(s, _)| *s != StatusCode::CREATED) {
            assert_eq!(*status, StatusCode::FORBIDDEN, "{body}");
            assert_eq!(body["error"], "group_limit");
            assert_eq!(body["error_description"], GROUP_LIMIT);
        }
        let owners: i64 =
            sqlx::query_scalar("SELECT count(*) FROM tenant_members WHERE role = 'owner'")
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(owners, 3);
        assert_eq!(
            count(&ctx.pool, "SELECT count(*) FROM tenants").await,
            3,
            "a refused create left a tenant behind"
        );
        assert_eq!(
            count(&ctx.pool, "SELECT count(*) FROM trial_grants").await,
            1
        );
    })
    .await;
}

#[tokio::test]
async fn it_parallel_transfer_and_creates_respect_the_limit() {
    run_hosted(|ctx| async move {
        let andy = login(&ctx, 4242, "andy").await;
        let blair = login(&ctx, 7777, "blair").await;
        let acme = create_named(&ctx, &andy, "acme").await;
        create_named(&ctx, &blair, "one").await;
        create_named(&ctx, &blair, "two").await;
        let blair_me = me(&ctx, &blair).await;
        let blair_id = blair_me["id"].as_str().unwrap();
        let tenant = acme["tenant"].as_str().unwrap();
        let mut tx = ctx.pool.begin().await.unwrap();
        agmen_hub::seed_tenant_member(&mut tx, tenant, blair_id, TenantRole::Admin)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        warm_hub_pool(&ctx, &blair).await;
        // Blair owns two groups. Andy's transfer of acme and six creates by
        // Blair each make Blair an owner of one more, so exactly one may win.
        let mut requests = vec![
            ctx.http
                .post(ctx.hub.url(&format!("/v1/tenants/{tenant}/transfer")))
                .bearer_auth(&andy)
                .json(&json!({"to": blair_me["handle"].as_str().unwrap()})),
        ];
        requests.extend((0..6).map(|n| group_request(&ctx, &blair, &format!("race-{n}"))));
        let answers = send_at_once(requests).await;
        let won = answers.iter().filter(|(s, _)| s.is_success()).count();
        assert_eq!(won, 1, "{answers:?}");
        for (status, body) in answers.iter().filter(|(s, _)| !s.is_success()) {
            assert_eq!(*status, StatusCode::FORBIDDEN, "{body}");
            assert_eq!(body["error"], "group_limit");
        }
        let owned: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tenant_members WHERE human_id = $1 AND role = 'owner'",
        )
        .bind(blair_id)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(owned, 3);
    })
    .await;
}

#[tokio::test]
async fn it_group_create_needs_a_human_token() {
    run_hosted(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        let made = create_named(&ctx, &token, "acme").await;
        let project = Project {
            tenant: made["tenant"].as_str().unwrap().to_owned(),
            project: made["project"].as_str().unwrap().to_owned(),
        };
        let agent = mint_ok(&ctx, &token, "andy/bot", &project).await;
        let body = json!({"name": "beta", "project": "web"});
        let (status, refused) = post_group(&ctx, &agent.token, &body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        assert_eq!(refused["error"], "human_token_required");
        let anonymous = ctx
            .http
            .post(ctx.hub.url("/v1/groups"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(count(&ctx.pool, "SELECT count(*) FROM tenants").await, 1);
    })
    .await;
}

#[tokio::test]
async fn it_same_owner_same_group_name_conflicts() {
    run_hosted(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        create_named(&ctx, &token, "acme").await;
        let (status, body) =
            post_group(&ctx, &token, &json!({"name": "acme", "project": "web"})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "group_exists");
        for sql in [
            "SELECT count(*) FROM tenants",
            "SELECT count(*) FROM tenant_plans",
            "SELECT count(*) FROM tenant_members",
            "SELECT count(*) FROM projects",
            "SELECT count(*) FROM trial_grants",
        ] {
            assert_eq!(count(&ctx.pool, sql).await, 1, "{sql}");
        }
        // The refused attempt used up neither the trial nor a place in the limit.
        let beta = create_named(&ctx, &token, "beta").await;
        assert_eq!(beta["state"], "read_only");
    })
    .await;
}

#[tokio::test]
async fn it_recreated_group_gets_no_second_trial() {
    run_hosted(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        let first = create_named(&ctx, &token, "acme").await;
        assert_eq!(first["state"], "trial");
        // What a group delete does (hosted Task 8): every row of the group goes.
        let tenant = first["tenant"].as_str().unwrap();
        for sql in [
            "DELETE FROM memberships
             WHERE project_id IN (SELECT id FROM projects WHERE tenant_id = $1)",
            "DELETE FROM projects WHERE tenant_id = $1",
            "DELETE FROM tenant_members WHERE tenant_id = $1",
            "DELETE FROM tenant_plans WHERE tenant_id = $1",
            "DELETE FROM tenants WHERE id = $1",
        ] {
            sqlx::query(sql)
                .bind(tenant)
                .execute(&ctx.pool)
                .await
                .unwrap();
        }
        let again = create_named(&ctx, &token, "acme").await;
        assert_eq!(again["state"], "read_only");
        assert!(again["trial_ends_at"].is_null(), "{again}");
        assert_eq!(
            count(&ctx.pool, "SELECT count(*) FROM trial_grants").await,
            1
        );
    })
    .await;
}

#[tokio::test]
async fn it_set_plan_sets_an_active_plan_and_refuses_unknown_tenant() {
    run_hosted(|ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        let made = create_named(&ctx, &token, "acme").await;
        let tenant = made["tenant"].as_str().unwrap();
        let url = ctx.db_url.as_str();
        assert!(matches!(
            agmen_hub::apply_operator_plan(url, "nosuchtenant1234", "team").await,
            Err(SetPlanError::NoTenant)
        ));
        assert!(matches!(
            agmen_hub::apply_operator_plan(url, "NOT A TENANT", "team").await,
            Err(SetPlanError::BadTenant)
        ));
        assert!(matches!(
            agmen_hub::apply_operator_plan(url, tenant, "free").await,
            Err(SetPlanError::BadPlan)
        ));
        // Every refusal left the trial and made no row for the unknown tenant.
        assert_eq!(me(&ctx, &token).await["groups"][0]["state"], "trial");
        assert_eq!(
            count(&ctx.pool, "SELECT count(*) FROM tenant_plans").await,
            1
        );
        agmen_hub::apply_operator_plan(url, tenant, "business-25")
            .await
            .unwrap();
        let row: (String, String, i32, i32, bool, bool) = sqlx::query_as(
            "SELECT state, plan, business_step, reminders_sent::int,
                    trial_started_at IS NULL, read_only_cause IS NULL
             FROM tenant_plans WHERE tenant_id = $1",
        )
        .bind(tenant)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(
            row,
            (
                "active".to_owned(),
                "business".to_owned(),
                25,
                0,
                true,
                true
            )
        );
        let shown = me(&ctx, &token).await;
        assert_eq!(shown["groups"][0]["state"], "active");
        assert!(shown["groups"][0].get("trial_ends_at").is_none(), "{shown}");
        assert!(shown.get("notices").is_none(), "{shown}");
        // A second run replaces the plan.
        agmen_hub::apply_operator_plan(url, tenant, "team")
            .await
            .unwrap();
        let plan: (String, Option<i32>) =
            sqlx::query_as("SELECT plan, business_step FROM tenant_plans WHERE tenant_id = $1")
                .bind(tenant)
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(plan, ("team".to_owned(), None));
    })
    .await;
}

#[tokio::test]
async fn it_hosted_groups_migration_keeps_tenants_and_reverses() {
    // Unlike run(), this database starts without the current migrations.
    let db = TestDb::create().await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db.url)
        .await
        .unwrap();
    let test_pool = pool.clone();
    let outcome = tokio::spawn(async move { migration_round_trip(&test_pool).await }).await;
    pool.close().await;
    db.drop_db().await;
    if let Err(err) = outcome {
        std::panic::resume_unwind(err.into_panic());
    }
}

async fn migration_round_trip(pool: &sqlx::PgPool) {
    for migration in MIGRATOR
        .iter()
        .filter(|m| m.version < 20 && m.migration_type.is_up_migration())
    {
        sqlx::raw_sql(migration.sql.clone())
            .execute(pool)
            .await
            .unwrap();
    }
    // A hub that served groups before hosted: the trial database has one tenant.
    let existing: Project = seed_project(pool).await;
    let up = MIGRATOR
        .iter()
        .find(|m| m.version == 20 && m.migration_type.is_up_migration())
        .unwrap();
    let down = MIGRATOR
        .iter()
        .find(|m| m.version == 20 && m.migration_type.is_down_migration())
        .unwrap();
    sqlx::raw_sql(up.sql.clone()).execute(pool).await.unwrap();
    assert_eq!(count(pool, "SELECT count(*) FROM tenants").await, 1);
    assert_eq!(count(pool, "SELECT count(*) FROM tenant_plans").await, 0);
    assert_eq!(count(pool, "SELECT count(*) FROM trial_grants").await, 0);
    // The old tenant takes a plan row, and a grant needs no group.
    sqlx::query("INSERT INTO tenant_plans (tenant_id, state, extra_seats, reminders_sent) VALUES ($1, 'active', 0, 0)")
        .bind(&existing.tenant)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO trial_grants (github_id, granted_at) VALUES ('4242', now())")
        .execute(pool)
        .await
        .unwrap();
    // Down drops both tables and leaves everything else.
    sqlx::raw_sql(down.sql.clone()).execute(pool).await.unwrap();
    for table in ["tenant_plans", "trial_grants"] {
        let found: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
            .bind(table)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(found, None, "{table} survived the down migration");
    }
    assert_eq!(count(pool, "SELECT count(*) FROM tenants").await, 1);
    assert_eq!(count(pool, "SELECT count(*) FROM projects").await, 1);
    // Up again works on the same database.
    sqlx::raw_sql(up.sql.clone()).execute(pool).await.unwrap();
    assert_eq!(count(pool, "SELECT count(*) FROM tenant_plans").await, 0);
    assert_eq!(count(pool, "SELECT count(*) FROM trial_grants").await, 0);
}
