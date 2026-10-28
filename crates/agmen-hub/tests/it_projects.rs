//! `POST /v1/projects`: a tenant admin creates a project and becomes its
//! admin, against an in-process hub and a database of the test's own on
//! `AGMEN_IT_PG`.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use agmen_hub::Role;
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{Ctx, count, mint_ok, new_id, run, seed_project, topics};

/// `POST /v1/projects` with `token` and `body`: the status and the answer.
async fn create(ctx: &Ctx, token: &str, body: &Value) -> (StatusCode, Value) {
    let resp = ctx
        .http
        .post(ctx.hub.url("/v1/projects"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn it_projects_admin_creates_and_becomes_admin() {
    run(|ctx| async move {
        let home = seed_project(&ctx.pool).await;
        let andy = ctx
            .hub
            .hub
            .seed_human(&topics(&home), Role::Admin)
            .await
            .unwrap();

        let (status, body) = create(&ctx, &andy.access_token, &json!({ "name": "fresh" })).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["tenant"], home.tenant.as_str());
        assert_eq!(body["name"], "fresh");
        assert_eq!(body["role"], "admin");
        let project = body["project"].as_str().unwrap().to_owned();
        let role: String = sqlx::query_scalar(
            "SELECT role FROM memberships WHERE human_id = $1 AND project_id = $2",
        )
        .bind(andy.id.as_str())
        .bind(&project)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(role, "admin");

        // The same name again in the tenant is a conflict and adds nothing.
        let (status, body) = create(&ctx, &andy.access_token, &json!({ "name": "fresh" })).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "project_exists");
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM projects WHERE tenant_id = $1",
                &home.tenant
            )
            .await,
            2
        );

        let (status, body) = create(&ctx, &andy.access_token, &json!({ "name": " pad" })).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], "invalid_name");
    })
    .await;
}

#[tokio::test]
async fn it_projects_refuses_non_admins_and_other_tenants() {
    run(|ctx| async move {
        let home = seed_project(&ctx.pool).await;
        let elsewhere = seed_project(&ctx.pool).await;
        let coordinator = ctx
            .hub
            .hub
            .seed_human(&topics(&home), Role::Coordinator)
            .await
            .unwrap();
        let (status, body) =
            create(&ctx, &coordinator.access_token, &json!({ "name": "nope" })).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "admin_required");

        // An admin of one tenant cannot create in another by naming it.
        let admin = ctx
            .hub
            .hub
            .seed_human(&topics(&home), Role::Admin)
            .await
            .unwrap();
        let (status, body) = create(
            &ctx,
            &admin.access_token,
            &json!({ "name": "nope", "tenant": elsewhere.tenant }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM projects WHERE tenant_id = $1",
                &elsewhere.tenant
            )
            .await,
            1
        );

        // An identity token is refused even for an admin's identity.
        let sol = mint_ok(
            &ctx,
            &admin.access_token,
            &format!("{}/sol", admin.handle),
            &home,
        )
        .await;
        let (status, body) = create(&ctx, &sol.token, &json!({ "name": "nope" })).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "human_token_required");
    })
    .await;
}

#[tokio::test]
async fn it_group_admin_creates_project() {
    run(|ctx| async move {
        let home = seed_project(&ctx.pool).await;
        let id_pattern = regex::Regex::new("^[a-km-np-z2-9]{16}$").unwrap();
        let repo_url = "https://github.com/acme/widgets.git";
        for (tenant_role, name) in [("owner", "widgets"), ("admin", "admin-widgets")] {
            let human = ctx
                .hub
                .hub
                .seed_human(&topics(&home), Role::Member)
                .await
                .unwrap();
            sqlx::query(
                "UPDATE tenant_members SET role = $1 WHERE tenant_id = $2 AND human_id = $3",
            )
            .bind(tenant_role)
            .bind(&home.tenant)
            .bind(human.id.as_str())
            .execute(&ctx.pool)
            .await
            .unwrap();
            // Group rights authorize creation without a project membership.
            sqlx::query("DELETE FROM memberships WHERE human_id = $1")
                .bind(human.id.as_str())
                .execute(&ctx.pool)
                .await
                .unwrap();

            let (status, body) = create(
                &ctx,
                &human.access_token,
                &json!({ "tenant": home.tenant, "name": name, "repo_url": repo_url }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
            assert_eq!(body["tenant"], home.tenant);
            assert_eq!(body["name"], name);
            let project = body["project"].as_str().unwrap();
            assert!(id_pattern.is_match(project), "{project}");
            assert_eq!(body["role"], "admin");
            assert_eq!(body["repo_url"], repo_url);
            let stored_url: Option<String> =
                sqlx::query_scalar("SELECT repo_url FROM projects WHERE id = $1")
                    .bind(project)
                    .fetch_one(&ctx.pool)
                    .await
                    .unwrap();
            assert_eq!(stored_url.as_deref(), Some(repo_url));

            let response = ctx
                .http
                .get(ctx.hub.url("/v1/me"))
                .bearer_auth(&human.access_token)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let me: Value = response.json().await.unwrap();
            assert!(me["projects"].as_array().unwrap().iter().any(|membership| {
                membership["tenant"] == home.tenant
                    && membership["project"] == project
                    && membership["name"] == name
                    && membership["role"] == "admin"
            }));
        }
    })
    .await;
}

#[tokio::test]
async fn it_group_member_cannot_create_project() {
    run(|ctx| async move {
        let home = seed_project(&ctx.pool).await;
        for tenant_role in [None, Some("member")] {
            let human = ctx
                .hub
                .hub
                .seed_human(&topics(&home), Role::Admin)
                .await
                .unwrap();
            if let Some(role) = tenant_role {
                sqlx::query(
                    "UPDATE tenant_members SET role = $1 WHERE tenant_id = $2 AND human_id = $3",
                )
                .bind(role)
                .bind(&home.tenant)
                .bind(human.id.as_str())
                .execute(&ctx.pool)
                .await
                .unwrap();
            } else {
                sqlx::query("DELETE FROM tenant_members WHERE tenant_id = $1 AND human_id = $2")
                    .bind(&home.tenant)
                    .bind(human.id.as_str())
                    .execute(&ctx.pool)
                    .await
                    .unwrap();
            }
            // Project admin rights alone must not authorize group actions.
            let (status, body) = create(
                &ctx,
                &human.access_token,
                &json!({ "tenant": home.tenant, "name": "nope" }),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert_eq!(body["error"], "admin_required");
            assert_eq!(
                count(
                    &ctx.pool,
                    "SELECT count(*) FROM projects WHERE tenant_id = $1",
                    &home.tenant,
                )
                .await,
                1
            );
        }
    })
    .await;
}

#[tokio::test]
async fn it_project_repo_url_is_optional() {
    run(|ctx| async move {
        let home = seed_project(&ctx.pool).await;
        let admin = ctx
            .hub
            .hub
            .seed_human(&topics(&home), Role::Admin)
            .await
            .unwrap();
        let (status, body) = create(&ctx, &admin.access_token, &json!({ "name": "widgets" })).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body.get("repo_url"), Some(&Value::Null));
        let stored_url: Option<String> =
            sqlx::query_scalar("SELECT repo_url FROM projects WHERE id = $1")
                .bind(body["project"].as_str().unwrap())
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(stored_url, None);
    })
    .await;
}

#[tokio::test]
async fn it_project_refuses_credentials_in_repo_url() {
    run(|ctx| async move {
        let home = seed_project(&ctx.pool).await;
        let admin = ctx
            .hub
            .hub
            .seed_human(&topics(&home), Role::Admin)
            .await
            .unwrap();
        for repo_url in [
            format!("https://{}:{}@github.com/acme/w.git", "user", "pw"),
            format!("https://{}@github.com/acme/w.git", "user"),
            "-oProxyCommand=x".to_owned(),
        ] {
            let (status, body) = create(
                &ctx,
                &admin.access_token,
                &json!({ "tenant": home.tenant, "name": "widgets", "repo_url": repo_url }),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            assert_eq!(body["error"], "bad_repo_url");
            assert_eq!(
                body["error_description"],
                agmen_proto::BadRepoUrl.to_string()
            );
        }
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM projects WHERE tenant_id = $1",
                &home.tenant,
            )
            .await,
            1
        );
        let credential_urls: i64 =
            sqlx::query_scalar("SELECT count(*) FROM projects WHERE repo_url LIKE '%@github.com%'")
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(credential_urls, 0);
    })
    .await;
}

#[tokio::test]
async fn it_project_create_answer_keeps_its_shape() {
    run(|ctx| async move {
        let home = seed_project(&ctx.pool).await;
        let admin = ctx
            .hub
            .hub
            .seed_human(&topics(&home), Role::Admin)
            .await
            .unwrap();
        let (status, body) = create(&ctx, &admin.access_token, &json!({ "name": "widgets" })).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let mut keys: Vec<_> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["name", "project", "repo_url", "role", "tenant"]);
        assert_eq!(body["role"], "admin");
    })
    .await;
}

#[tokio::test]
async fn it_project_concurrent_same_name_creates_one() {
    run(|ctx| async move {
        let home = seed_project(&ctx.pool).await;
        let admin = ctx
            .hub
            .hub
            .seed_human(&topics(&home), Role::Admin)
            .await
            .unwrap();
        let body = json!({ "name": "twin" });
        let (first, second) = tokio::join!(
            create(&ctx, &admin.access_token, &body),
            create(&ctx, &admin.access_token, &body),
        );
        let mut statuses = [first.0, second.0];
        statuses.sort_unstable();
        assert_eq!(statuses, [StatusCode::CREATED, StatusCode::CONFLICT]);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM projects WHERE tenant_id = $1 AND name = 'twin'",
                &home.tenant,
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM memberships m JOIN projects p ON p.id = m.project_id
                 WHERE p.tenant_id = $1 AND p.name = 'twin'",
                &home.tenant,
            )
            .await,
            1
        );

        // A tenant id that does not exist answers like one the caller is not in.
        let (missing_status, missing) = create(
            &ctx,
            &admin.access_token,
            &json!({ "name": "nope", "tenant": new_id() }),
        )
        .await;
        let elsewhere = seed_project(&ctx.pool).await;
        let (other_status, other) = create(
            &ctx,
            &admin.access_token,
            &json!({ "name": "nope", "tenant": elsewhere.tenant }),
        )
        .await;
        assert_eq!(missing_status, StatusCode::FORBIDDEN, "{missing}");
        assert_eq!((missing_status, missing), (other_status, other));
    })
    .await;
}
