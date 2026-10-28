//! Tenant roles against databases created for each test on `AGMEN_IT_PG`.
#![cfg(test)]

mod support;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use agmen_hub::{MIGRATOR, Role};
use reqwest::StatusCode;
use rumqttc::v5::mqttbytes::v5::ConnectReturnCode;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};
use support::{
    Ctx, Project, broker_token, env, login, member_with_token, mint_ok, mqtt, new_id, run,
    seed_human, seed_member, seed_project, topics, unban_username,
};
use url::Url;

async fn another_project(pool: &PgPool, tenant: &str) -> Project {
    let project = new_id();
    sqlx::query("INSERT INTO projects (id, tenant_id, name) VALUES ($1, $2, $1)")
        .bind(&project)
        .bind(tenant)
        .execute(pool)
        .await
        .unwrap();
    Project {
        tenant: tenant.to_owned(),
        project,
    }
}

async fn group_rows(pool: &PgPool, tenant: &str) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT human_id, role FROM tenant_members WHERE tenant_id = $1 ORDER BY human_id",
    )
    .bind(tenant)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn backfill(pool: &PgPool) {
    for migration in MIGRATOR
        .iter()
        .filter(|m| m.version < 9 && m.migration_type.is_up_migration())
    {
        sqlx::raw_sql(migration.sql.clone())
            .execute(pool)
            .await
            .unwrap();
    }
    let first = seed_project(pool).await;
    let second = another_project(pool, &first.tenant).await;
    let a = seed_human(pool, 4242, "alice").await;
    let b = seed_human(pool, 2, "bob").await;
    let c = seed_human(pool, 3, "carol").await;
    for (human, project, role, age) in [
        (&a, &first.project, "admin", 1),
        (&b, &second.project, "admin", 2),
        (&c, &first.project, "coordinator", 0),
        (&c, &second.project, "member", 0),
        (&b, &first.project, "member", 0),
    ] {
        sqlx::query("INSERT INTO memberships (human_id, project_id, role, created_at)
                     VALUES ($1, $2, $3, TIMESTAMPTZ '2020-01-01 00:00:00+00' - $4 * interval '1 second')")
            .bind(human).bind(project).bind(role).bind(age as f64)
            .execute(pool).await.unwrap();
    }
    for (project, human, github_id, age) in [
        (&first.project, &a, "4242", 2),
        (&second.project, &b, "2", 1),
    ] {
        sqlx::query("INSERT INTO bootstrap_admins (project_id, github_id, claimed_by, claimed_at)
                     VALUES ($1, $2, $3, TIMESTAMPTZ '2020-01-01 00:00:00+00' - $4 * interval '1 second')")
            .bind(project).bind(github_id).bind(human).bind(age as f64)
            .execute(pool).await.unwrap();
    }
    let fallback = seed_project(pool).await;
    for (human, age) in [(&a, 1), (&b, 2), (&c, 2)] {
        sqlx::query("INSERT INTO memberships (human_id, project_id, role, created_at)
                     VALUES ($1, $2, 'admin', TIMESTAMPTZ '2020-01-01 00:00:00+00' - $3 * interval '1 second')")
            .bind(human).bind(&fallback.project).bind(age as f64)
            .execute(pool).await.unwrap();
    }
    let migration = MIGRATOR
        .iter()
        .find(|m| m.version == 9 && m.migration_type.is_up_migration())
        .unwrap();
    sqlx::raw_sql(migration.sql.clone())
        .execute(pool)
        .await
        .unwrap();
    let mut expected = vec![
        (a.clone(), "owner".to_owned()),
        (b.clone(), "admin".to_owned()),
        (c.clone(), "member".to_owned()),
    ];
    expected.sort();
    assert_eq!(group_rows(pool, &first.tenant).await, expected);
    let fallback_owner = b.min(c);
    let rows = group_rows(pool, &fallback.tenant).await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows.iter().filter(|(_, role)| role == "owner").count(), 1);
    assert!(rows.contains(&(fallback_owner, "owner".to_owned())));
    // The down migration drops the table and leaves the rest; up again refills it.
    let down = MIGRATOR
        .iter()
        .find(|m| m.version == 9 && m.migration_type.is_down_migration())
        .unwrap();
    sqlx::raw_sql(down.sql.clone()).execute(pool).await.unwrap();
    let gone: Option<String> = sqlx::query_scalar("SELECT to_regclass('tenant_members')::text")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(gone, None);
    let memberships: i64 = sqlx::query_scalar("SELECT count(*) FROM memberships")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(memberships, 8);
    sqlx::raw_sql(migration.sql.clone())
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(group_rows(pool, &first.tenant).await.len(), 3);
}

#[tokio::test]
async fn it_tenant_roles_backfill() {
    // Unlike run(), this database must start without the current migrations.
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&env("AGMEN_IT_PG"))
        .await
        .unwrap();
    let name = format!("agmen_it_{}", new_id());
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&admin)
        .await
        .unwrap();
    let mut url = Url::parse(&env("AGMEN_IT_PG")).unwrap();
    url.set_path(&name);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(url.as_str())
        .await
        .unwrap();
    let test_pool = pool.clone();
    let outcome = tokio::spawn(async move { backfill(&test_pool).await }).await;
    pool.close().await;
    sqlx::query(AssertSqlSafe(format!(
        "DROP DATABASE \"{name}\" WITH (FORCE)"
    )))
    .execute(&admin)
    .await
    .unwrap();
    admin.close().await;
    if let Err(err) = outcome {
        std::panic::resume_unwind(err.into_panic());
    }
}

#[tokio::test]
async fn it_one_owner_per_tenant() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        let first = seed_human(&ctx.pool, 1, "first").await;
        let second = seed_human(&ctx.pool, 2, "second").await;
        sqlx::query("INSERT INTO tenant_members (tenant_id, human_id, role) VALUES ($1, $2, 'owner')")
            .bind(&project.tenant).bind(&first).execute(&ctx.pool).await.unwrap();
        let err = sqlx::query("INSERT INTO tenant_members (tenant_id, human_id, role) VALUES ($1, $2, 'owner')")
            .bind(&project.tenant).bind(&second).execute(&ctx.pool).await.unwrap_err();
        assert!(matches!(err, sqlx::Error::Database(ref db) if db.is_unique_violation() && db.constraint() == Some("tenant_members_one_owner")));
        assert_eq!(group_rows(&ctx.pool, &project.tenant).await, vec![(first, "owner".to_owned())]);
    }).await;
}

async fn accept_invite(ctx: &Ctx, admin: &str, human: &str, project: &Project, role: &str) {
    let response = ctx
        .http
        .post(ctx.hub.url("/v1/invites"))
        .bearer_auth(admin)
        .json(&json!({"project": project.project, "role": role}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let invite: Value = response.json().await.unwrap();
    let code = invite["code"].as_str().unwrap();
    let response = ctx
        .http
        .post(ctx.hub.url(&format!("/v1/invites/{code}/accept")))
        .bearer_auth(human)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["role"], role);
}

#[tokio::test]
async fn it_invite_accept_joins_group() {
    run(|ctx| async move {
        let first = seed_project(&ctx.pool).await;
        let second = another_project(&ctx.pool, &first.tenant).await;
        let admin = ctx
            .hub
            .hub
            .seed_human(&topics(&first), Role::Admin)
            .await
            .unwrap();
        seed_member(&ctx.pool, admin.id.as_str(), &second, "admin").await;
        let token = login(&ctx, 2, "newcomer").await;
        let human: String = sqlx::query_scalar("SELECT id FROM humans WHERE provider_sub = '2'")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        accept_invite(&ctx, &admin.access_token, &token, &first, "admin").await;
        let role: String = sqlx::query_scalar(
            "SELECT role FROM tenant_members WHERE tenant_id = $1 AND human_id = $2",
        )
        .bind(&first.tenant)
        .bind(&human)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(
            role, "member",
            "project admin invitations only join the group as member"
        );
        accept_invite(&ctx, &admin.access_token, &token, &second, "member").await;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tenant_members WHERE tenant_id = $1 AND human_id = $2",
        )
        .bind(&first.tenant)
        .bind(&human)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
        let third = another_project(&ctx.pool, &first.tenant).await;
        seed_member(&ctx.pool, admin.id.as_str(), &third, "admin").await;
        sqlx::query(
            "UPDATE tenant_members SET role = 'admin' WHERE tenant_id = $1 AND human_id = $2",
        )
        .bind(&first.tenant)
        .bind(&human)
        .execute(&ctx.pool)
        .await
        .unwrap();
        accept_invite(&ctx, &admin.access_token, &token, &third, "member").await;
        let role: String = sqlx::query_scalar(
            "SELECT role FROM tenant_members WHERE tenant_id = $1 AND human_id = $2",
        )
        .bind(&first.tenant)
        .bind(&human)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(role, "admin");
    })
    .await;
}

async fn bootstrap_row(pool: &PgPool, project: &Project, github_id: &str) {
    sqlx::query("INSERT INTO bootstrap_admins (project_id, github_id) VALUES ($1, $2)")
        .bind(&project.project)
        .bind(github_id)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn it_bootstrap_claim_makes_owner() {
    run(|ctx| async move {
        let first = seed_project(&ctx.pool).await;
        let second = another_project(&ctx.pool, &first.tenant).await;
        bootstrap_row(&ctx.pool, &first, "4242").await;
        bootstrap_row(&ctx.pool, &second, "2").await;
        login(&ctx, 4242, "owner").await;
        login(&ctx, 2, "admin").await;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT h.provider_sub, m.role FROM tenant_members m JOIN humans h ON h.id = m.human_id
             WHERE m.tenant_id = $1 ORDER BY h.provider_sub",
        )
        .bind(&first.tenant)
        .fetch_all(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                ("2".to_owned(), "admin".to_owned()),
                ("4242".to_owned(), "owner".to_owned())
            ]
        );
        let claimed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM bootstrap_admins WHERE claimed_by IS NOT NULL",
        )
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(claimed, 2);
        login(&ctx, 4242, "owner").await;
        assert_eq!(group_rows(&ctx.pool, &first.tenant).await.len(), 2);
        // Existing members and admins can claim an ownerless tenant too.
        for (github_id, handle, role) in
            [(11, "member-owner", "member"), (12, "admin-owner", "admin")]
        {
            let project = seed_project(&ctx.pool).await;
            let human = seed_human(&ctx.pool, github_id, handle).await;
            seed_member(&ctx.pool, &human, &project, role).await;
            bootstrap_row(&ctx.pool, &project, &github_id.to_string()).await;
            login(&ctx, github_id, handle).await;
            assert_eq!(
                group_rows(&ctx.pool, &project.tenant).await,
                vec![(human, "owner".to_owned())]
            );
        }
    })
    .await;
}

#[tokio::test]
async fn it_concurrent_bootstrap_claims_leave_one_owner() {
    run(|ctx| async move {
        for round in 0..4_u64 {
            let first = seed_project(&ctx.pool).await;
            let second = another_project(&ctx.pool, &first.tenant).await;
            let (a, b) = (100 + round * 2, 101 + round * 2);
            bootstrap_row(&ctx.pool, &first, &a.to_string()).await;
            bootstrap_row(&ctx.pool, &second, &b.to_string()).await;
            let (name_a, name_b) = (format!("racer-a{round}"), format!("racer-b{round}"));
            tokio::join!(login(&ctx, a, &name_a), login(&ctx, b, &name_b));
            let roles: Vec<String> = sqlx::query_scalar(
                "SELECT role FROM tenant_members WHERE tenant_id = $1 ORDER BY role",
            )
            .bind(&first.tenant)
            .fetch_all(&ctx.pool)
            .await
            .unwrap();
            assert_eq!(roles, ["admin", "owner"], "round {round}");
            let memberships: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM memberships m JOIN projects p ON p.id = m.project_id
                 WHERE p.tenant_id = $1 AND m.role = 'admin'",
            )
            .bind(&first.tenant)
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
            assert_eq!(memberships, 2, "round {round}");
        }
    })
    .await;
}

#[tokio::test]
async fn it_me_lists_groups() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        sqlx::query("UPDATE tenants SET name = 'zeta' WHERE id = $1")
            .bind(&project.tenant)
            .execute(&ctx.pool)
            .await
            .unwrap();
        bootstrap_row(&ctx.pool, &project, "4242").await;
        let token = login(&ctx, 4242, "owner").await;
        let response = ctx
            .http
            .get(ctx.hub.url("/v1/me"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body["groups"],
            json!([{"tenant": project.tenant, "name": "zeta", "role": "owner"}])
        );
        let other = seed_project(&ctx.pool).await;
        sqlx::query("UPDATE tenants SET name = 'alpha' WHERE id = $1")
            .bind(&other.tenant)
            .execute(&ctx.pool)
            .await
            .unwrap();
        let human = body["id"].as_str().unwrap();
        seed_member(&ctx.pool, human, &other, "admin").await;
        let body: Value = ctx
            .http
            .get(ctx.hub.url("/v1/me"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            body["groups"],
            json!([
                {"tenant": other.tenant, "name": "alpha", "role": "admin"},
                {"tenant": project.tenant, "name": "zeta", "role": "owner"},
            ])
        );
        let stranger = login(&ctx, 99, "stranger").await;
        let body: Value = ctx
            .http
            .get(ctx.hub.url("/v1/me"))
            .bearer_auth(stranger)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["groups"], json!([]));
    })
    .await;
}

struct Group {
    first: Project,
    second: Project,
    owner: String,
    admin: String,
    member: String,
    member_id: String,
}

async fn group(ctx: &Ctx) -> Group {
    let first = seed_project(&ctx.pool).await;
    let second = another_project(&ctx.pool, &first.tenant).await;
    let owner_id = seed_human(&ctx.pool, 1, "owner").await;
    sqlx::query("INSERT INTO tenant_members (tenant_id, human_id, role) VALUES ($1, $2, 'owner')")
        .bind(&first.tenant)
        .bind(&owner_id)
        .execute(&ctx.pool)
        .await
        .unwrap();
    seed_member(&ctx.pool, &owner_id, &first, "admin").await;
    let owner = login(ctx, 1, "owner").await;
    let admin = member_with_token(ctx, &first, 2, "admin", "admin").await;
    let member = member_with_token(ctx, &first, 3, "member", "member").await;
    let member_id: String = sqlx::query_scalar("SELECT id FROM humans WHERE handle = 'member'")
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
    seed_member(&ctx.pool, &member_id, &second, "member").await;
    Group {
        first,
        second,
        owner,
        admin,
        member,
        member_id,
    }
}

async fn request(
    ctx: &Ctx,
    method: reqwest::Method,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = ctx
        .http
        .request(method, ctx.hub.url(path))
        .bearer_auth(token)
        // A two-identity removal tries eight upstream calls of at most 10 s
        // each. Wait for those attempts before the wrapper lifts the bans.
        .timeout(Duration::from_secs(90));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let bytes = response.bytes().await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn member_path(group: &Group, handle: &str) -> String {
    format!("/v1/tenants/{}/members/{handle}", group.first.tenant)
}

fn assert_error(response: (StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(response.0, status, "{}", response.1);
    assert_eq!(response.1["error"], code);
}

// The local wrapper extends run's panic cleanup to every ban these tests cause.
async fn lifting_bans<F, Fut>(ctx: Ctx, ids: Vec<String>, body: F)
where
    F: FnOnce(Arc<Ctx>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    assert!(ids.len() <= 2, "these tests clean up at most two bans");
    let ctx = Arc::new(ctx);
    let mut task = tokio::spawn(body(Arc::clone(&ctx)));
    // Reserve 20 s for both cleanup calls within the profile's 120 s cap.
    let outcome = tokio::time::timeout(Duration::from_secs(95), &mut task).await;
    if outcome.is_err() {
        task.abort();
        // Aborting the test future releases its clients before cleanup.
        let _cancelled = task.await;
    }
    let mut cleanup_failed = false;
    for id in ids.iter().take(2) {
        let http = ctx.http.clone();
        let id = id.clone();
        let mut cleanup = tokio::spawn(async move { unban_username(&http, &id).await });
        match tokio::time::timeout(Duration::from_secs(10), &mut cleanup).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => cleanup_failed = true,
            Err(_) => {
                cleanup.abort();
                let _cancelled = cleanup.await;
                cleanup_failed = true;
            }
        }
    }
    match outcome {
        Ok(Ok(())) => assert!(!cleanup_failed, "ban cleanup failed"),
        Ok(Err(err)) => std::panic::resume_unwind(err.into_panic()),
        Err(_) => panic!("member test exceeded 95 s; ban cleanup attempted"),
    }
}

#[tokio::test]
async fn it_members_lists_handles_roles_projects() {
    run(|ctx| async move {
        let group = group(&ctx).await;
        let z = mint_ok(&ctx, &group.member, "member/z", &group.first).await;
        let a = mint_ok(&ctx, &group.member, "member/a", &group.first).await;
        let b = mint_ok(&ctx, &group.member, "member/b", &group.second).await;
        let revoked = mint_ok(&ctx, &group.member, "member/revoked", &group.first).await;
        sqlx::query("UPDATE identities SET revoked_at = now() WHERE id = $1")
            .bind(&revoked.id).execute(&ctx.pool).await.unwrap();
        let other = seed_project(&ctx.pool).await;
        seed_member(&ctx.pool, &group.member_id, &other, "admin").await;
        mint_ok(&ctx, &group.member, "member/elsewhere", &other).await;
        let path = format!("/v1/tenants/{}/members", group.first.tenant);
        for token in [&group.owner, &group.admin, &group.member] {
            let (status, body) = request(&ctx, reqwest::Method::GET, &path, token, None).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let members = body["members"].as_array().unwrap();
            assert_eq!(members.len(), 3);
            assert_eq!(members.iter().map(|m| m["handle"].as_str().unwrap()).collect::<Vec<_>>(), ["admin", "member", "owner"]);
            assert_eq!(members[0]["role"], "admin");
            assert_eq!(members[2]["role"], "owner");
            let mut projects = vec![
                json!({"project": group.first.project, "name": format!("project {}", group.first.project), "role": "member"}),
                json!({"project": group.second.project, "name": group.second.project, "role": "member"}),
            ];
            projects.sort_by_key(|p| p["project"].as_str().unwrap().to_owned());
            let mut identities = vec![
                json!({"id": z.id, "project": group.first.project, "handle": "member/z"}),
                json!({"id": a.id, "project": group.first.project, "handle": "member/a"}),
                json!({"id": b.id, "project": group.second.project, "handle": "member/b"}),
            ];
            identities.sort_by_key(|i| (i["project"].as_str().unwrap().to_owned(), i["handle"].as_str().unwrap().to_owned()));
            assert_eq!(members[1], json!({"human_id": group.member_id, "handle": "member", "role": "member", "projects": projects, "identities": identities}));
            assert_eq!(members[0]["identities"], json!([]));
        }
    }).await;
}

#[tokio::test]
async fn it_non_member_cannot_list_members() {
    run(|ctx| async move {
        let group = group(&ctx).await;
        let stranger = login(&ctx, 9, "stranger").await;
        let path = format!("/v1/tenants/{}/members", group.first.tenant);
        assert_error(
            request(&ctx, reqwest::Method::GET, &path, &stranger, None).await,
            StatusCode::FORBIDDEN,
            "not_a_group_member",
        );
    })
    .await;
}

#[tokio::test]
async fn it_remove_member_revokes_everywhere() {
    run(|ctx| async move {
        let group = group(&ctx).await;
        let a = mint_ok(&ctx, &group.member, "member/a", &group.first).await;
        let b = mint_ok(&ctx, &group.member, "member/b", &group.second).await;
        lifting_bans(ctx, vec![a.id.clone(), b.id.clone()], |ctx| async move {
            let other = seed_project(&ctx.pool).await;
            seed_member(&ctx.pool, &group.member_id, &other, "admin").await;
            let untouched = mint_ok(&ctx, &group.member, "member/untouched", &other).await;
            sqlx::query("UPDATE memberships SET role = 'admin' WHERE human_id = $1 AND project_id = $2")
                .bind(&group.member_id).bind(&group.first.project).execute(&ctx.pool).await.unwrap();
            let (status, invite) = request(&ctx, reqwest::Method::POST, "/v1/invites", &group.member,
                Some(json!({"project": group.first.project, "role": "member"}))).await;
            assert_eq!(status, StatusCode::CREATED, "{invite}");
            let (status, used_invite) = request(&ctx, reqwest::Method::POST, "/v1/invites", &group.member,
                Some(json!({"project": group.first.project, "role": "member"}))).await;
            assert_eq!(status, StatusCode::CREATED, "{used_invite}");
            let newcomer = login(&ctx, 9, "newcomer").await;
            let used_path = format!("/v1/invites/{}/accept", used_invite["code"].as_str().unwrap());
            assert_eq!(request(&ctx, reqwest::Method::POST, &used_path, &newcomer, None).await.0, StatusCode::OK);
            let (status, outside_invite) = request(&ctx, reqwest::Method::POST, "/v1/invites", &group.member,
                Some(json!({"project": other.project, "role": "member"}))).await;
            assert_eq!(status, StatusCode::CREATED, "{outside_invite}");
            let mut clients = Vec::new();
            for identity in [&a, &b] {
                let (status, body) = broker_token(&ctx.http, &ctx.hub, &identity.id, &identity.token).await;
                assert_eq!(status, StatusCode::OK, "{body}");
                let broker = body["token"].as_str().unwrap().to_owned();
                let client = tokio::time::timeout(Duration::from_secs(10), mqtt::connect_ok(mqtt::options(
                    &format!("{}-mcp", identity.id), &identity.id, &broker))).await.unwrap();
                clients.push((identity.clone(), broker, client));
            }
            let started = tokio::time::Instant::now();
            let response = request(&ctx, reqwest::Method::DELETE, &member_path(&group, "member"), &group.owner, None).await;
            assert_eq!(response.0, StatusCode::NO_CONTENT, "{}", response.1);
            let deadline = started + Duration::from_secs(2);
            for (_, _, client) in clients.iter_mut().take(2) {
                tokio::time::timeout_at(deadline, client.closed_within(Duration::from_secs(2))).await
                    .expect("both clients kicked within 2 s").unwrap();
            }
            assert!(started.elapsed() <= Duration::from_secs(2), "both clients kicked within 2 s");
            for (identity, broker, _) in clients.iter().take(2) {
                let revoked: bool = sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM identities WHERE id = $1")
                    .bind(&identity.id).fetch_one(&ctx.pool).await.unwrap();
                assert!(revoked);
                let code = tokio::time::timeout(Duration::from_secs(10), mqtt::connect_refused(mqtt::options(
                    &format!("{}-mcp", identity.id), &identity.id, broker))).await.unwrap();
                assert_eq!(code, ConnectReturnCode::Banned, "CONNACK 0x8A");
                assert_error(broker_token(&ctx.http, &ctx.hub, &identity.id, &identity.token).await,
                    StatusCode::FORBIDDEN, "identity_revoked");
            }
            let invite_path = format!("/v1/invites/{}/accept", invite["code"].as_str().unwrap());
            assert_eq!(request(&ctx, reqwest::Method::POST, &invite_path, &newcomer, None).await.0, StatusCode::NOT_FOUND);
            assert_error(request(&ctx, reqwest::Method::POST, &used_path, &group.member, None).await,
                StatusCode::CONFLICT, "invite_used");
            let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM memberships m JOIN projects p ON p.id = m.project_id WHERE m.human_id = $1 AND p.tenant_id = $2")
                .bind(&group.member_id).bind(&group.first.tenant).fetch_one(&ctx.pool).await.unwrap();
            assert_eq!(rows, 0);
            assert!(!group_rows(&ctx.pool, &group.first.tenant).await.iter().any(|(id, _)| id == &group.member_id));
            assert_eq!(request(&ctx, reqwest::Method::POST, &format!("/v1/invites/{}/accept", outside_invite["code"].as_str().unwrap()), &newcomer, None).await.0, StatusCode::OK);
            assert_eq!(broker_token(&ctx.http, &ctx.hub, &untouched.id, &untouched.token).await.0, StatusCode::OK);
            let outside_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM memberships WHERE human_id = $1 AND project_id = $2")
                .bind(&group.member_id).bind(&other.project).fetch_one(&ctx.pool).await.unwrap();
            assert_eq!(outside_rows, 1);
            assert!(group_rows(&ctx.pool, &other.tenant).await.iter().any(|(id, _)| id == &group.member_id));
        }).await;
    }).await;
}

#[tokio::test]
async fn it_remove_member_is_repeatable() {
    run(|ctx| async move {
        let group = group(&ctx).await;
        let identity = mint_ok(&ctx, &group.member, "member/a", &group.first).await;
        lifting_bans(ctx, vec![identity.id.clone()], |ctx| async move {
            let (status, broker) =
                broker_token(&ctx.http, &ctx.hub, &identity.id, &identity.token).await;
            assert_eq!(status, StatusCode::OK);
            let path = member_path(&group, "member");
            assert_eq!(
                request(&ctx, reqwest::Method::DELETE, &path, &group.owner, None)
                    .await
                    .0,
                StatusCode::NO_CONTENT
            );
            let revoked: String =
                sqlx::query_scalar("SELECT revoked_at::text FROM identities WHERE id = $1")
                    .bind(&identity.id)
                    .fetch_one(&ctx.pool)
                    .await
                    .unwrap();
            unban_username(&ctx.http, &identity.id).await;
            assert_eq!(
                request(&ctx, reqwest::Method::DELETE, &path, &group.owner, None)
                    .await
                    .0,
                StatusCode::NO_CONTENT
            );
            let again: String =
                sqlx::query_scalar("SELECT revoked_at::text FROM identities WHERE id = $1")
                    .bind(&identity.id)
                    .fetch_one(&ctx.pool)
                    .await
                    .unwrap();
            assert_eq!(again, revoked);
            assert_eq!(
                tokio::time::timeout(
                    Duration::from_secs(10),
                    mqtt::connect_refused(mqtt::options(
                        &format!("{}-mcp", identity.id),
                        &identity.id,
                        broker["token"].as_str().unwrap()
                    ))
                )
                .await
                .unwrap(),
                ConnectReturnCode::Banned
            );
            sqlx::query(
                "UPDATE identities SET revoked_at = now() - interval '2 hours' WHERE id = $1",
            )
            .bind(&identity.id)
            .execute(&ctx.pool)
            .await
            .unwrap();
            assert_error(
                request(&ctx, reqwest::Method::DELETE, &path, &group.owner, None).await,
                StatusCode::NOT_FOUND,
                "no_such_member",
            );
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn it_remove_rules() {
    run(|ctx| async move {
        let group = group(&ctx).await;
        let second_admin = member_with_token(&ctx, &group.first, 4, "second-admin", "admin").await;
        for token in [&group.owner, &group.admin] {
            assert_error(
                request(
                    &ctx,
                    reqwest::Method::DELETE,
                    &member_path(&group, "owner"),
                    token,
                    None,
                )
                .await,
                StatusCode::CONFLICT,
                "owner_cannot_be_removed",
            );
        }
        assert_error(
            request(
                &ctx,
                reqwest::Method::DELETE,
                &member_path(&group, "second-admin"),
                &group.admin,
                None,
            )
            .await,
            StatusCode::FORBIDDEN,
            "owner_required",
        );
        assert_error(
            request(
                &ctx,
                reqwest::Method::DELETE,
                &member_path(&group, "admin"),
                &group.member,
                None,
            )
            .await,
            StatusCode::FORBIDDEN,
            "group_admin_required",
        );
        assert_error(
            request(
                &ctx,
                reqwest::Method::DELETE,
                &member_path(&group, "absent"),
                &group.owner,
                None,
            )
            .await,
            StatusCode::NOT_FOUND,
            "no_such_member",
        );
        assert_eq!(
            request(
                &ctx,
                reqwest::Method::DELETE,
                &member_path(&group, "member"),
                &group.admin,
                None
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            request(
                &ctx,
                reqwest::Method::DELETE,
                &member_path(&group, "admin"),
                &group.admin,
                None
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            request(
                &ctx,
                reqwest::Method::DELETE,
                &member_path(&group, "second-admin"),
                &second_admin,
                None
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
    })
    .await;
}

#[tokio::test]
async fn it_role_changes() {
    run(|ctx| async move {
        let group = group(&ctx).await;
        let path = format!("{}/role", member_path(&group, "member"));
        for (token, role) in [(&group.owner, "admin"), (&group.admin, "member")] {
            let (status, body) = request(
                &ctx,
                reqwest::Method::PUT,
                &path,
                token,
                Some(json!({"role": role})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body, json!({"handle": "member", "role": role}));
            assert!(
                group_rows(&ctx.pool, &group.first.tenant)
                    .await
                    .contains(&(group.member_id.clone(), role.to_owned()))
            );
        }
        assert_error(
            request(
                &ctx,
                reqwest::Method::PUT,
                &format!("{}/role", member_path(&group, "owner")),
                &group.owner,
                Some(json!({"role": "member"})),
            )
            .await,
            StatusCode::CONFLICT,
            "owner_role_fixed",
        );
        assert_error(
            request(
                &ctx,
                reqwest::Method::PUT,
                &path,
                &group.owner,
                Some(json!({"role": "owner"})),
            )
            .await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "use_transfer",
        );
        assert_error(
            request(
                &ctx,
                reqwest::Method::PUT,
                &format!("{}/role", member_path(&group, "absent")),
                &group.admin,
                Some(json!({"role": "admin"})),
            )
            .await,
            StatusCode::NOT_FOUND,
            "no_such_member",
        );
        assert_error(
            request(
                &ctx,
                reqwest::Method::PUT,
                &path,
                &group.member,
                Some(json!({"role": "admin"})),
            )
            .await,
            StatusCode::FORBIDDEN,
            "group_admin_required",
        );
        for body in [
            json!({"role": "member", "extra": true}),
            json!({"role": "coordinator"}),
            json!({}),
        ] {
            assert_eq!(
                request(&ctx, reqwest::Method::PUT, &path, &group.owner, Some(body))
                    .await
                    .0,
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
    })
    .await;
}

#[tokio::test]
async fn it_transfer_to_admin_only() {
    run(|ctx| async move {
        let group = group(&ctx).await;
        let path = format!("/v1/tenants/{}/transfer", group.first.tenant);
        for target in ["member", "owner", "absent"] {
            assert_error(request(&ctx, reqwest::Method::POST, &path, &group.owner, Some(json!({"to": target}))).await,
                StatusCode::CONFLICT, "not_a_group_admin");
        }
        for token in [&group.admin, &group.member] {
            assert_error(request(&ctx, reqwest::Method::POST, &path, token, Some(json!({"to": "owner"}))).await,
                StatusCode::FORBIDDEN, "owner_required");
        }
        assert_eq!(request(&ctx, reqwest::Method::POST, &path, &group.owner, Some(json!({"to": "admin", "extra": true}))).await.0, StatusCode::UNPROCESSABLE_ENTITY);
        let (status, body) = request(&ctx, reqwest::Method::POST, &path, &group.owner, Some(json!({"to": "admin"}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"owner": "admin"}));
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT h.handle, m.role FROM tenant_members m JOIN humans h ON h.id = m.human_id WHERE tenant_id = $1 ORDER BY h.handle")
            .bind(&group.first.tenant).fetch_all(&ctx.pool).await.unwrap();
        assert_eq!(rows, [("admin".to_owned(), "owner".to_owned()), ("member".to_owned(), "member".to_owned()), ("owner".to_owned(), "admin".to_owned())]);
        assert_error(request(&ctx, reqwest::Method::POST, &path, &group.owner, Some(json!({"to": "admin"}))).await,
            StatusCode::FORBIDDEN, "owner_required");
    }).await;
}

#[tokio::test]
async fn it_group_admin_revokes_member_identity() {
    run(|ctx| async move {
        let group = group(&ctx).await;
        let a = mint_ok(&ctx, &group.member, "member/a", &group.first).await;
        let b = mint_ok(&ctx, &group.member, "member/b", &group.second).await;
        lifting_bans(ctx, vec![a.id.clone(), b.id.clone()], |ctx| async move {
            let ordinary = member_with_token(&ctx, &group.first, 4, "ordinary", "member").await;
            let other = seed_project(&ctx.pool).await;
            seed_member(&ctx.pool, &group.member_id, &other, "member").await;
            let outside = mint_ok(&ctx, &group.member, "member/outside", &other).await;
            for token in [&ordinary, &group.admin] {
                let id = if token == &ordinary {
                    &a.id
                } else {
                    &outside.id
                };
                assert_eq!(
                    request(
                        &ctx,
                        reqwest::Method::DELETE,
                        &format!("/v1/identities/{id}"),
                        token,
                        None
                    )
                    .await
                    .0,
                    StatusCode::NOT_FOUND
                );
            }
            for (token, identity) in [(&group.admin, &a), (&group.owner, &b)] {
                let (status, body) = request(
                    &ctx,
                    reqwest::Method::DELETE,
                    &format!("/v1/identities/{}", identity.id),
                    token,
                    None,
                )
                .await;
                assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
                assert_eq!(
                    request(
                        &ctx,
                        reqwest::Method::DELETE,
                        &format!("/v1/identities/{}", identity.id),
                        &ordinary,
                        None
                    )
                    .await
                    .0,
                    StatusCode::NOT_FOUND
                );
                assert_error(
                    broker_token(&ctx.http, &ctx.hub, &identity.id, &identity.token).await,
                    StatusCode::FORBIDDEN,
                    "identity_revoked",
                );
            }
        })
        .await;
    })
    .await;
}

// Waits (at most 10 s) until a statement starting with `prefix` is blocked
// on a lock, so the test knows how far a concurrent request has got.
async fn wait_lock_blocked(pool: &PgPool, prefix: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
             WHERE wait_event_type = 'Lock' AND query LIKE $1 AND pid <> pg_backend_pid())",
        )
        .bind(format!("{prefix}%"))
        .fetch_one(pool)
        .await
        .unwrap();
        if blocked {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no statement starting {prefix:?} blocked on a lock within 10 s"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// A mint that passed its membership check and then waits on a unique-index
// conflict must not leave a live identity once the member is removed.
#[tokio::test]
async fn it_mint_racing_member_removal_leaves_no_live_identity() {
    run(|ctx| async move {
        let group = group(&ctx).await;
        let ctx = Arc::new(ctx);
        // A second connection holds an uncommitted live identity with the
        // handle the mint will insert, so that insert blocks until rollback.
        let other = PgPoolOptions::new()
            .max_connections(1)
            .connect(&ctx.db_url)
            .await
            .unwrap();
        let owner_id: String = sqlx::query_scalar("SELECT id FROM humans WHERE handle = 'owner'")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        let mut blocker = other.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO identities (id, tenant_id, project_id, kind, handle, owner_human_id)
             VALUES ($1, $2, $3, 'agent', 'member/race', $4)",
        )
        .bind(new_id())
        .bind(&group.first.tenant)
        .bind(&group.first.project)
        .bind(&owner_id)
        .execute(&mut *blocker)
        .await
        .unwrap();

        let mint = tokio::spawn({
            let ctx = Arc::clone(&ctx);
            let token = group.member.clone();
            let project = group.first.project.clone();
            async move {
                request(
                    &ctx,
                    reqwest::Method::POST,
                    "/v1/identities",
                    &token,
                    Some(json!({"handle": "member/race", "project": project, "kind": "agent"})),
                )
                .await
            }
        });
        wait_lock_blocked(&ctx.pool, "INSERT INTO identities").await;
        let removal = tokio::spawn({
            let ctx = Arc::clone(&ctx);
            let token = group.owner.clone();
            let path = member_path(&group, "member");
            async move { request(&ctx, reqwest::Method::DELETE, &path, &token, None).await }
        });
        // Whether the removal commits now (the bug) or waits for the mint
        // (the fix), the blocker's rollback lets the mint finish.
        tokio::time::sleep(Duration::from_secs(1)).await;
        blocker.rollback().await.unwrap();
        let mint = tokio::time::timeout(Duration::from_secs(20), mint)
            .await
            .unwrap()
            .unwrap();
        let removal = tokio::time::timeout(Duration::from_secs(90), removal)
            .await
            .unwrap()
            .unwrap();

        // Lift any ban the removal caused before asserting.
        let minted: Vec<String> =
            sqlx::query_scalar("SELECT id FROM identities WHERE owner_human_id = $1")
                .bind(&group.member_id)
                .fetch_all(&ctx.pool)
                .await
                .unwrap();
        for id in minted.iter().take(4) {
            unban_username(&ctx.http, id).await;
        }
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM identities WHERE owner_human_id = $1 AND revoked_at IS NULL",
        )
        .bind(&group.member_id)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        let memberships: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM memberships m JOIN projects p ON p.id = m.project_id
             WHERE m.human_id = $1 AND p.tenant_id = $2",
        )
        .bind(&group.member_id)
        .bind(&group.first.tenant)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(removal.0, StatusCode::NO_CONTENT, "{}", removal.1);
        // The mint passed its membership check before the removal began.
        assert_eq!(mint.0, StatusCode::CREATED, "{}", mint.1);
        assert_eq!(live, 0, "a removed member kept a live identity");
        assert_eq!(memberships, 0);
        assert!(
            !group_rows(&ctx.pool, &group.first.tenant)
                .await
                .iter()
                .any(|(id, _)| id == &group.member_id)
        );
    })
    .await;
}
