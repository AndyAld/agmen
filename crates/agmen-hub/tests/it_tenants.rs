//! Tenant roles against databases created for each test on `AGMEN_IT_PG`.
#![cfg(test)]

mod support;

use agmen_hub::{MIGRATOR, Role};
use reqwest::StatusCode;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};
use support::{
    Ctx, Project, env, login, new_id, run, seed_human, seed_member, seed_project, topics,
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
