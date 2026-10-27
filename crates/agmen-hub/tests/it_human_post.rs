//! `POST /v1/projects/{p}/messages`: a signed-in member posts to the chat as
//! themselves, against an in-process hub and a database of the test's own
//! on `AGMEN_IT_PG`. The route runs the same ingest as `up/` (protocol
//! 5.2) and answers with the ACK or NACK body.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{
    Ctx, Project, count, envelope, login, mint_ok, run, seed_human, seed_member, seed_project, ulid,
};

const ANDY: u64 = 4242;
const CAROL: u64 = 77;

/// `POST /v1/projects/{project}/messages` with `token` and the raw `body`:
/// the status, the `Retry-After` header, and the JSON answer.
async fn post(
    ctx: &Ctx,
    token: &str,
    project: &str,
    body: Vec<u8>,
) -> (StatusCode, Option<String>, Value) {
    let resp = ctx
        .http
        .post(ctx.hub.url(&format!("/v1/projects/{project}/messages")))
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .map(|value| value.to_str().unwrap().to_owned());
    (
        status,
        retry_after,
        resp.json().await.unwrap_or(Value::Null),
    )
}

/// A project where andy is a member and has a token.
async fn room(ctx: &Ctx) -> (Project, String, String) {
    let project = seed_project(&ctx.pool).await;
    let andy = seed_human(&ctx.pool, ANDY, "andy").await;
    seed_member(&ctx.pool, &andy, &project, "admin").await;
    let token = login(ctx, ANDY, "andy").await;
    (project, andy, token)
}

fn fyi(id: &str, body: &str) -> Vec<u8> {
    envelope(id, "FYI", json!({}), json!({ "body": body }))
}

#[tokio::test]
async fn it_human_post_is_stamped_as_the_human() {
    run(|ctx| async move {
        let (project, andy, token) = room(&ctx).await;
        let id = ulid();
        // The same bytes on a resend: `sent_at` is part of the content hash.
        let raw = fyi(&id, "hello team");
        let (status, _, ack) = post(&ctx, &token, &project.project, raw.clone()).await;
        assert_eq!(status, StatusCode::OK, "{ack}");
        assert_eq!(ack["verb"], "ACK");
        assert_eq!(ack["id"], id);
        assert_eq!(ack["stream"], "feed");
        assert_eq!(ack["seq"], 1);

        // The message is on the feed, from the human, with their handle.
        let resp = ctx
            .http
            .get(
                ctx.hub
                    .url(&format!("/v1/projects/{}/streams/feed", project.project)),
            )
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        let page: Value = resp.json().await.unwrap();
        let stored = &page["messages"][0];
        assert_eq!(stored["id"], id);
        assert_eq!(stored["from"], andy);
        assert_eq!(stored["from_handle"], "andy");
        assert_eq!(stored["from_kind"], "human");

        // A resend gets the same ACK and records nothing more.
        let (status, _, again) = post(&ctx, &token, &project.project, raw).await;
        assert_eq!(status, StatusCode::OK, "{again}");
        assert_eq!(again, ack);
        let stored = count(
            &ctx.pool,
            "SELECT count(*) FROM messages WHERE project_id = $1",
            &project.project,
        )
        .await;
        assert_eq!(stored, 1, "the resend was recorded again");

        // The HTTP caller reads the ACK from the response: nothing is
        // queued on ack/{human}.
        let acks = count(
            &ctx.pool,
            "SELECT count(*) FROM publish_outbox WHERE topic LIKE '%/ack/%' AND project_id = $1",
            &project.project,
        )
        .await;
        assert_eq!(acks, 0, "an ACK for the human was queued");
    })
    .await;
}

#[tokio::test]
async fn it_human_post_runs_ingest_checks() {
    run(|ctx| async move {
        let (project, _, token) = room(&ctx).await;

        // Over 32,768 bytes of body, inside the 64 KiB envelope limit.
        let big = "x".repeat(40 * 1024);
        let (status, _, nack) = post(&ctx, &token, &project.project, fyi(&ulid(), &big)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{nack}");
        assert_eq!(nack["verb"], "NACK");
        assert_eq!(nack["reason"], "body_too_large");
        assert_eq!(nack["retryable"], false);

        // A 70 KiB body also exceeds the envelope limit, which ingest
        // answers as `schema` (the envelope check comes first).
        let huge = "x".repeat(70 * 1024);
        let (status, _, nack) = post(&ctx, &token, &project.project, fyi(&ulid(), &huge)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{nack}");
        assert_eq!(nack["reason"], "schema");

        // A credential-shaped string.
        let secret = "key AKIAIOSFODNN7EXAMPLE";
        let (status, _, nack) = post(&ctx, &token, &project.project, fyi(&ulid(), secret)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{nack}");
        assert_eq!(nack["reason"], "secret_detected");

        // A verb a human may not send.
        let claim = envelope(&ulid(), "CLAIM", json!({}), json!({}));
        let (status, _, nack) = post(&ctx, &token, &project.project, claim).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{nack}");
        assert_eq!(nack["verb"], "NACK");

        // No readable id: nothing to name in a NACK.
        let (status, _, body) = post(&ctx, &token, &project.project, b"{}".to_vec()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    })
    .await;
}

#[tokio::test]
async fn it_human_post_is_rate_limited() {
    run(|ctx| async move {
        let (project, _, token) = room(&ctx).await;
        let mut limited = None;
        for _ in 0..40 {
            let (status, retry_after, reply) =
                post(&ctx, &token, &project.project, fyi(&ulid(), "spam")).await;
            if status == StatusCode::TOO_MANY_REQUESTS {
                limited = Some((retry_after, reply));
                break;
            }
            assert_eq!(status, StatusCode::OK, "{reply}");
        }
        let (retry_after, nack) = limited.expect("40 quick posts were never limited");
        assert_eq!(nack["reason"], "rate_limited");
        assert_eq!(nack["retryable"], true);
        assert!(nack["retry_after_ms"].as_u64().unwrap() > 0, "{nack}");
        assert!(retry_after.is_some(), "no Retry-After header");
    })
    .await;
}

#[tokio::test]
async fn it_human_post_needs_membership() {
    run(|ctx| async move {
        let (project, _, _) = room(&ctx).await;
        let other = seed_project(&ctx.pool).await;
        let carol = seed_human(&ctx.pool, CAROL, "carol").await;
        seed_member(&ctx.pool, &carol, &other, "member").await;
        let carol_token = login(&ctx, CAROL, "carol").await;

        let (status, _, body) = post(
            &ctx,
            &carol_token,
            &project.project,
            fyi(&ulid(), "let me in"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "not_a_member");
        let stored = count(
            &ctx.pool,
            "SELECT count(*) FROM messages WHERE project_id = $1",
            &project.project,
        )
        .await;
        assert_eq!(stored, 0);

        // No token at all.
        let resp = ctx
            .http
            .post(
                ctx.hub
                    .url(&format!("/v1/projects/{}/messages", project.project)),
            )
            .body(fyi(&ulid(), "anon"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    })
    .await;
}

#[tokio::test]
async fn it_human_post_refuses_an_identity_token() {
    run(|ctx| async move {
        let (project, _, token) = room(&ctx).await;
        let sol = mint_ok(&ctx, &token, "andy/sol", &project).await;
        let (status, _, body) = post(
            &ctx,
            &sol.token,
            &project.project,
            fyi(&ulid(), "as an identity"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "human_token_required");
    })
    .await;
}
