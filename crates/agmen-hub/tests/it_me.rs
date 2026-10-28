//! `GET /v1/me` (ruling R56) and the `test-hooks` that give tests a
//! signed-in human without GitHub (ruling R59), against an in-process hub
//! and a database of the test's own on `AGMEN_IT_PG`.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use agmen_hub::Role;
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{
    Ctx, PUBLIC_MQTT_URL, Project, mint_ok, new_id, poll_device_token, run, seed_member,
    start_device_login, topics,
};

/// `GET /v1/me` with `token`: the status and the JSON answer.
async fn me(ctx: &Ctx, token: Option<&str>) -> (StatusCode, Value) {
    let mut req = ctx.http.get(ctx.hub.url("/v1/me"));
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// A tenant with a project named `name`.
async fn named_project(ctx: &Ctx, name: &str) -> Project {
    let (tenant, project) = (new_id(), new_id());
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'tenant ' || $1)")
        .bind(&tenant)
        .execute(&ctx.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO projects (id, tenant_id, name) VALUES ($1, $2, $3)")
        .bind(&project)
        .bind(&tenant)
        .bind(name)
        .execute(&ctx.pool)
        .await
        .unwrap();
    Project { tenant, project }
}

#[tokio::test]
async fn it_me_answers_the_signed_in_human() {
    run(|ctx| async move {
        let zeta = named_project(&ctx, "zeta").await;
        let alpha = named_project(&ctx, "alpha").await;
        let other = named_project(&ctx, "not mine").await;
        let andy = ctx
            .hub
            .hub
            .seed_human(&topics(&zeta), Role::Coordinator)
            .await
            .unwrap();
        seed_member(&ctx.pool, andy.id.as_str(), &alpha, "admin").await;
        // Someone else's membership does not show.
        let adam = ctx
            .hub
            .hub
            .seed_human(&topics(&other), Role::Member)
            .await
            .unwrap();
        assert_ne!(adam.id, andy.id);

        let (status, body) = me(&ctx, Some(&andy.access_token)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            json!({
                "id": andy.id.as_str(),
                "handle": andy.handle,
                "broker_url": PUBLIC_MQTT_URL,
                "projects": [
                    { "tenant": alpha.tenant, "project": alpha.project, "name": "alpha", "role": "admin" },
                    { "tenant": zeta.tenant, "project": zeta.project, "name": "zeta", "role": "coordinator" },
                ],
            })
        );

        // A human in no project gets an empty list.
        sqlx::query("DELETE FROM memberships WHERE human_id = $1")
            .bind(adam.id.as_str())
            .execute(&ctx.pool)
            .await
            .unwrap();
        let (status, body) = me(&ctx, Some(&adam.access_token)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["projects"], json!([]));
        assert_eq!(body["handle"], adam.handle.as_str());
    })
    .await;
}

#[tokio::test]
async fn it_me_refuses_identity_tokens() {
    run(|ctx| async move {
        let project = named_project(&ctx, "work").await;
        let andy = ctx
            .hub
            .hub
            .seed_human(&topics(&project), Role::Member)
            .await
            .unwrap();
        let sol = mint_ok(
            &ctx,
            &andy.access_token,
            &format!("{}/sol", andy.handle),
            &project,
        )
        .await;

        let (status, body) = me(&ctx, Some(&sol.token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "human_token_required");

        let (status, _) = me(&ctx, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = me(&ctx, Some(&andy.refresh_token)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a refresh token is no bearer token"
        );
    })
    .await;
}

/// A test completes `agmen login`'s device flow with the hooks: the human
/// comes from `seed_human`, and `approve_device` stands in for the browser.
#[tokio::test]
async fn it_device_flow_approved_by_test_hook() {
    run(|ctx| async move {
        let project = named_project(&ctx, "work").await;
        let hub = &ctx.hub.hub;
        let andy = hub
            .seed_human(&topics(&project), Role::Admin)
            .await
            .unwrap();

        let start = start_device_login(&ctx).await;
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "authorization_pending");

        // The code as the CLI shows it (`BCDF-GHJK`), in any case.
        hub.approve_device(&start.user_code.to_lowercase(), &andy.id)
            .await
            .unwrap();
        assert!(
            hub.approve_device(&start.user_code, &andy.id)
                .await
                .is_err(),
            "a login is approved once"
        );
        assert!(
            hub.approve_device("BCDF-GHJK", &andy.id).await.is_err(),
            "an unknown code is refused"
        );

        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let access = body["access_token"].as_str().unwrap();
        let (status, who) = me(&ctx, Some(access)).await;
        assert_eq!(status, StatusCode::OK, "{who}");
        assert_eq!(who["id"], andy.id.as_str());
        assert_eq!(who["projects"][0]["role"], "admin");

        // The seeded refresh token is a real one: the refresh grant takes it.
        let resp = ctx
            .http
            .post(ctx.hub.url("/v1/auth/token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", andy.refresh_token.as_str()),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    })
    .await;
}
