//! Self-host group claims against each test's own database.
#![cfg(test)]

mod support;

use agmen_hub::{Config, GroupError, Hub, HubError, HubMode, Role};
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{Ctx, DEV_KEY_1, TestHub, env, run, seed_human, seed_member, seed_project, topics};

async fn claim(ctx: &Ctx, access: &str, token: &str) -> (StatusCode, Value) {
    let response = ctx
        .http
        .post(ctx.hub.url("/v1/claim"))
        .bearer_auth(access)
        .json(&json!({ "token": token }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}

async fn claimant(ctx: &Ctx) -> agmen_hub::SeededHuman {
    let project = seed_project(&ctx.pool).await;
    ctx.hub
        .hub
        .seed_human(&topics(&project), Role::Member)
        .await
        .unwrap()
}

#[tokio::test]
async fn it_fresh_hub_creates_one_group() {
    run(|ctx| async move {
        let group: (String, String) = sqlx::query_as(
            "SELECT t.id, t.name FROM hub_group g JOIN tenants t ON t.id = g.tenant_id",
        )
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(group.1, "agmen");
        let second = TestHub::start(&ctx.db_url, &ctx.github, DEV_KEY_1, &[]).await;
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM tenants), (SELECT count(*) FROM hub_group)",
        )
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(counts, (1, 1));
        let adopted: String = sqlx::query_scalar("SELECT tenant_id FROM hub_group")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(adopted, group.0);
        drop(second);
    })
    .await;
}

#[tokio::test]
async fn it_claim_makes_owner() {
    run(|ctx| async move {
        let human = claimant(&ctx).await;
        let token = ctx.hub.hub.mint_claim_token().await.unwrap().unwrap();
        let (status, body) = claim(&ctx, &human.access_token, &token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["role"], "owner");
        assert_eq!(body["name"], "agmen");
        assert_eq!(body.as_object().unwrap().len(), 3);
        let role: String = sqlx::query_scalar(
            "SELECT role FROM tenant_members WHERE human_id = $1 AND tenant_id = $2",
        )
        .bind(human.id.as_str())
        .bind(body["tenant"].as_str().unwrap())
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(role, "owner");
        let cleared: bool = sqlx::query_scalar(
            "SELECT claim_hash IS NULL AND claim_expires_at IS NULL FROM hub_group",
        )
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert!(cleared);
        let me: Value = ctx
            .http
            .get(ctx.hub.url("/v1/me"))
            .bearer_auth(&human.access_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(me["groups"].as_array().unwrap().contains(&body));
        let (status, body) = claim(&ctx, &human.access_token, &token).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "already_claimed");
    })
    .await;
}

#[tokio::test]
async fn it_claim_wrong_token_refused() {
    run(|ctx| async move {
        let human = claimant(&ctx).await;
        let token = ctx.hub.hub.mint_claim_token().await.unwrap().unwrap();
        let (status, body) = claim(&ctx, &human.access_token, &"0".repeat(64)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "bad_claim_token");
        let response = ctx
            .http
            .post(ctx.hub.url("/v1/claim"))
            .bearer_auth(&human.access_token)
            .json(&json!({ "token": token, "extra": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            claim(&ctx, &human.access_token, &token).await.0,
            StatusCode::OK
        );
    })
    .await;
}

#[tokio::test]
async fn it_claim_expired_is_refused() {
    run(|ctx| async move {
        let human = claimant(&ctx).await;
        let token = ctx.hub.hub.mint_claim_token().await.unwrap().unwrap();
        sqlx::query("UPDATE hub_group SET claim_expires_at = now() - interval '1 second'")
            .execute(&ctx.pool)
            .await
            .unwrap();
        // A wrong hash wins over expiry.
        assert_eq!(
            claim(&ctx, &human.access_token, &"0".repeat(64)).await.0,
            StatusCode::FORBIDDEN
        );
        let (status, body) = claim(&ctx, &human.access_token, &token).await;
        assert_eq!(status, StatusCode::GONE);
        assert_eq!(body["error"], "claim_expired");
    })
    .await;
}

#[tokio::test]
async fn it_claim_new_start_replaces_token() {
    run(|ctx| async move {
        let human = claimant(&ctx).await;
        let old = ctx.hub.hub.mint_claim_token().await.unwrap().unwrap();
        let second = TestHub::start(&ctx.db_url, &ctx.github, DEV_KEY_1, &[]).await;
        let (status, body) = claim(&ctx, &human.access_token, &old).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "bad_claim_token");
        let fresh = second.hub.mint_claim_token().await.unwrap().unwrap();
        assert_eq!(
            claim(&ctx, &human.access_token, &fresh).await.0,
            StatusCode::OK
        );
    })
    .await;
}

#[tokio::test]
async fn it_claim_after_owner_refused() {
    run(|ctx| async move {
        let human = claimant(&ctx).await;
        let token = ctx.hub.hub.mint_claim_token().await.unwrap().unwrap();
        assert_eq!(
            claim(&ctx, &human.access_token, &token).await.0,
            StatusCode::OK
        );
        assert!(ctx.hub.hub.mint_claim_token().await.unwrap().is_none());
        let (status, body) = claim(&ctx, &human.access_token, &"0".repeat(64)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "already_claimed");
    })
    .await;
}

#[tokio::test]
async fn it_hosted_mode_has_no_claim_route() {
    run(|ctx| async move {
        sqlx::query("DELETE FROM hub_group")
            .execute(&ctx.pool)
            .await
            .unwrap();
        let hosted =
            TestHub::start_in_mode(&ctx.db_url, &ctx.github, DEV_KEY_1, &[], HubMode::Hosted).await;
        let response = ctx
            .http
            .post(hosted.url("/v1/claim"))
            .json(&json!({ "token": "0".repeat(64) }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM hub_group")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    })
    .await;
}

#[tokio::test]
async fn it_many_tenants_refuse_self_host() {
    run(|ctx| async move {
        sqlx::query("DELETE FROM hub_group")
            .execute(&ctx.pool)
            .await
            .unwrap();
        seed_project(&ctx.pool).await;
        let config = Config::from_lookup(|name| match name {
            "AGMEN_DATABASE_URL" => Some(ctx.db_url.clone()),
            "AGMEN_PUBLIC_URL" => Some(ctx.hub.base.to_string()),
            "AGMEN_GITHUB_CLIENT_ID" => Some(agmen_hub::FAKE_GITHUB_CLIENT_ID.to_owned()),
            "AGMEN_GITHUB_URL" | "AGMEN_GITHUB_API_URL" => Some(ctx.github.base().to_string()),
            "AGMEN_EMQX_API_URL" => Some(env("AGMEN_IT_EMQX_API")),
            "AGMEN_EMQX_API_KEY" => Some(env("AGMEN_IT_EMQX_API_KEY")),
            "AGMEN_EMQX_API_SECRET" => Some(env("AGMEN_IT_EMQX_API_SECRET")),
            "AGMEN_MQTT_URL" => Some("mqtt://localhost:1883".to_owned()),
            "AGMEN_JWT_SIGNING_KEY" => Some(
                std::path::Path::new(&env("AGMEN_IT_JWT_DIR"))
                    .join(format!("{DEV_KEY_1}.pem"))
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ),
            _ => None,
        })
        .unwrap();
        let error = Hub::connect(&config).await.unwrap_err();
        assert!(
            matches!(error, HubError::Group(GroupError::ManyTenants(2))),
            "{error:?}"
        );
    })
    .await;
}

#[tokio::test]
async fn it_adopted_group_with_members_mints_no_claim_token() {
    run(|ctx| async move {
        // A database that was used before this hub: one tenant with a member
        // and no hub_group row.
        sqlx::query("DELETE FROM hub_group")
            .execute(&ctx.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM tenants")
            .execute(&ctx.pool)
            .await
            .unwrap();
        let project = seed_project(&ctx.pool).await;
        let human = seed_human(&ctx.pool, 77, "veteran").await;
        seed_member(&ctx.pool, &human, &project, "member").await;
        let second = TestHub::start(&ctx.db_url, &ctx.github, DEV_KEY_1, &[]).await;
        let (adopted, pending): (String, bool) =
            sqlx::query_as("SELECT tenant_id, claim_hash IS NOT NULL FROM hub_group")
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(adopted, project.tenant);
        assert!(!pending, "start stored a claim token for a used group");
        assert!(second.hub.mint_claim_token().await.unwrap().is_none());
    })
    .await;
}

#[tokio::test]
async fn it_claim_refused_once_the_group_has_a_member() {
    run(|ctx| async move {
        let human = claimant(&ctx).await;
        let token = ctx.hub.hub.mint_claim_token().await.unwrap().unwrap();
        // Someone joins the empty group by another route after the mint.
        let member = seed_human(&ctx.pool, 78, "early").await;
        sqlx::query(
            "INSERT INTO tenant_members (tenant_id, human_id, role)
             SELECT tenant_id, $1, 'member' FROM hub_group",
        )
        .bind(&member)
        .execute(&ctx.pool)
        .await
        .unwrap();
        let (status, body) = claim(&ctx, &human.access_token, &token).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "already_claimed");
        let cleared: bool = sqlx::query_scalar(
            "SELECT claim_hash IS NULL AND claim_expires_at IS NULL FROM hub_group",
        )
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert!(cleared, "the refusal left the token stored");
    })
    .await;
}
