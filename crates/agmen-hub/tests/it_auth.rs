//! Hub login (RFC 8628 device flow with GitHub sign-in on the approve page)
//! and invites, against an in-process hub, a fake GitHub and a database of
//! the test's own on `AGMEN_IT_PG`.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{
    Ctx, approve_in_browser, count, login, member_with_token, poll_device_token, run, seed_human,
    seed_member, seed_project, start_device_login,
};

const ANDY: u64 = 4242;

async fn refresh(ctx: &Ctx, refresh_token: &str) -> (StatusCode, Value) {
    let resp = ctx
        .http
        .post(ctx.hub.url("/v1/auth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .unwrap();
    (resp.status(), resp.json().await.unwrap())
}

#[tokio::test]
async fn it_device_flow_issues_human_token() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        let andy = seed_human(&ctx.pool, ANDY, "andy").await;
        seed_member(&ctx.pool, &andy, &project, "member").await;

        let start = start_device_login(&ctx).await;
        assert_eq!(start.user_code.len(), 9, "user code {}", start.user_code);
        assert_eq!(start.body["interval"], 5);
        assert_eq!(start.body["expires_in"], 600);
        assert!(
            start.verification_uri_complete.contains(&start.user_code),
            "{}",
            start.verification_uri_complete
        );
        let form = ctx.http.get(&start.verification_uri).send().await.unwrap();
        assert_eq!(form.status(), StatusCode::OK);
        assert!(form.text().await.unwrap().contains("name=\"user_code\""));

        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "authorization_pending");

        // GitHub reports a different login case; the seeded human matches by
        // account id, not by login.
        ctx.github.sign_in_as(ANDY, "Andy");
        let page = approve_in_browser(&ctx, &start).await;
        assert!(page.contains(&start.user_code), "confirm page: {page}");

        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["token_type"], "Bearer");
        assert_eq!(body["expires_in"], 3600);
        let access = body["access_token"].as_str().unwrap();
        let refresh_token = body["refresh_token"].as_str().unwrap();
        assert_ne!(access, refresh_token);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM humans WHERE provider_sub = $1",
                "4242"
            )
            .await,
            1,
            "sign-in made a second human for the seeded account"
        );

        // The access token acts as the seeded human: it mints andy/ children.
        let (status, minted) = support::mint(
            &ctx,
            access,
            &json!({ "handle": "andy/sol", "project": project.project }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{minted}");

        // A device code is redeemed once.
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
    })
    .await;
}

#[tokio::test]
async fn it_device_flow_new_human_gets_lowercase_handle() {
    run(|ctx| async move {
        login(&ctx, 777, "Adam-K").await;
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM humans WHERE handle = $1",
                "adam-k"
            )
            .await,
            1
        );
    })
    .await;
}

#[tokio::test]
async fn it_refresh_token_rotates() {
    run(|ctx| async move {
        seed_human(&ctx.pool, ANDY, "andy").await;
        ctx.github.sign_in_as(ANDY, "andy");
        let start = start_device_login(&ctx).await;
        approve_in_browser(&ctx, &start).await;
        let (_, first) = poll_device_token(&ctx, &start.device_code).await;
        let old = first["refresh_token"].as_str().unwrap();

        let (status, second) = refresh(&ctx, old).await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_ne!(second["refresh_token"], first["refresh_token"]);
        assert_ne!(second["access_token"], first["access_token"]);

        let (status, body) = refresh(&ctx, old).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "old refresh token reused");
        assert_eq!(body["error"], "invalid_grant");

        // An access token is not a refresh token.
        let (status, _) = refresh(&ctx, second["access_token"].as_str().unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    })
    .await;
}

#[tokio::test]
async fn it_invite_once() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        let andy = member_with_token(&ctx, &project, ANDY, "andy", "admin").await;
        let adam = login(&ctx, 2, "adam").await;
        let eve = login(&ctx, 3, "eve").await;

        // Only project admins invite.
        let refused = ctx
            .http
            .post(ctx.hub.url("/v1/invites"))
            .bearer_auth(&adam)
            .json(&json!({ "project": project.project, "role": "member" }))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);

        let created = ctx
            .http
            .post(ctx.hub.url("/v1/invites"))
            .bearer_auth(&andy)
            .json(&json!({ "project": project.project, "role": "member" }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let invite: Value = created.json().await.unwrap();
        let code = invite["code"].as_str().unwrap();
        assert!(invite["expires_at"].is_string(), "{invite}");

        let accept = |token: String| {
            let req = ctx
                .http
                .post(ctx.hub.url(&format!("/v1/invites/{code}/accept")))
                .bearer_auth(token);
            async move { req.send().await.unwrap() }
        };
        let first = accept(adam.clone()).await;
        assert_eq!(first.status(), StatusCode::OK);
        let joined: Value = first.json().await.unwrap();
        assert_eq!(joined["project"], project.project.as_str());
        assert_eq!(joined["tenant"], project.tenant.as_str());
        assert_eq!(joined["role"], "member");

        assert_eq!(accept(eve).await.status(), StatusCode::CONFLICT);
        assert_eq!(accept(adam).await.status(), StatusCode::CONFLICT);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM memberships WHERE project_id = $1",
                &project.project
            )
            .await,
            2,
            "andy and adam are members, eve is not"
        );

        let unknown = ctx
            .http
            .post(ctx.hub.url("/v1/invites/nosuchcode/accept"))
            .bearer_auth(&andy)
            .send()
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    })
    .await;
}
