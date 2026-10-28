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
use support::{Ctx, count, mint_ok, run, seed_project, topics};

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
