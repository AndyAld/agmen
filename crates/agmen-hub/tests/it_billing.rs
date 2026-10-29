//! Signed Paddle delivery, tenant isolation, ordering, and plan transitions.
#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_hub::{BillingEvent, HUB_IDENTITY, HubMode, Plan, PlanRecord, ReadOnlyCause};
use agmen_proto::{DeliveryReply, StampedEnvelope, Timestamp};
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};
use support::fake_paddle::FakePaddle;
use support::{
    Ctx, Project, count, envelope, login, run_hosted_with_paddle, run_in_mode, topics, ulid,
};

const SECS_PER_DAY: u64 = 86_400;
const MAX_ALERT_ROWS: i64 = 2;
const STALE_SECONDS: u64 = 400;

fn now() -> Timestamp {
    Timestamp::from_second(Timestamp::now().as_second()).unwrap()
}

async fn group(ctx: &Ctx, name: &str) -> (String, Project) {
    let access = login(ctx, 4242, "andy").await;
    let response = ctx
        .http
        .post(ctx.hub.url("/v1/groups"))
        .bearer_auth(&access)
        .json(&json!({"name": name, "project": "web"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let made: Value = response.json().await.unwrap();
    (
        access,
        Project {
            tenant: made["tenant"].as_str().unwrap().to_owned(),
            project: made["project"].as_str().unwrap().to_owned(),
        },
    )
}

fn subscription(project: &Project, id: &str, kind: &str, status: &str, at: Timestamp) -> Value {
    json!({
        "event_id": id, "notification_id": "ntf_ignored", "event_type": kind,
        "occurred_at": at,
        "data": {
            "id": "sub_1", "customer_id": "ctm_1", "status": status,
            "custom_data": {"agmen_tenant": project.tenant},
            "items": [{"status": "active", "price": {"id": "pri_business_25"}, "quantity": 1}],
        },
    })
}

async fn send_event(ctx: &Ctx, paddle: &FakePaddle, event: &Value) -> Response {
    let body = serde_json::to_vec(event).unwrap();
    ctx.http
        .post(ctx.hub.url("/v1/paddle/webhook"))
        .header(
            "Paddle-Signature",
            paddle.sign(&body, Timestamp::now().as_second()),
        )
        .body(body)
        .send()
        .await
        .unwrap()
}

async fn plan_row(ctx: &Ctx, project: &Project) -> Value {
    let row: String =
        sqlx::query_scalar("SELECT row_to_json(p)::text FROM tenant_plans p WHERE tenant_id = $1")
            .bind(&project.tenant)
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
    serde_json::from_str(&row).unwrap()
}

async fn me(ctx: &Ctx, access: &str) -> Value {
    let response = ctx
        .http
        .get(ctx.hub.url("/v1/me"))
        .bearer_auth(access)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn activate(ctx: &Ctx, project: &Project, at: Timestamp) {
    let record =
        PlanRecord::trial(at).apply(BillingEvent::Active(Plan::Team { extra_seats: 0 }), at);
    ctx.hub
        .hub
        .set_plan(topics(project).tenant(), &record)
        .await
        .unwrap();
}

async fn post_fyi(ctx: &Ctx, project: &Project) {
    let topics = topics(project);
    let (agent, _) = ctx.hub.hub.seed_identity(&topics).await.unwrap();
    let raw = envelope(&ulid(), "FYI", json!({}), json!({}));
    let reply = ctx.hub.hub.ingest(&topics, &agent.id, &raw).await.unwrap();
    assert!(matches!(reply, DeliveryReply::Ack { .. }), "{reply:?}");
}

async fn alerts(ctx: &Ctx, project: &Project) -> Vec<StampedEnvelope> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT envelope::text FROM messages WHERE project_id = $1 AND stream = 'feed'
         AND from_id = $2 AND envelope->>'verb' = 'ALERT' ORDER BY seq LIMIT $3",
    )
    .bind(&project.project)
    .bind(HUB_IDENTITY)
    .bind(MAX_ALERT_ROWS)
    .fetch_all(&ctx.pool)
    .await
    .unwrap();
    rows.iter()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect()
}

#[tokio::test]
async fn it_unsigned_webhook_refused() {
    run_hosted_with_paddle(|ctx, _paddle| async move {
        let (_, project) = group(&ctx, "acme").await;
        let before = plan_row(&ctx, &project).await;
        let response = ctx
            .http
            .post(ctx.hub.url("/v1/paddle/webhook"))
            .body("{")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "bad_signature"
        );
        let other = FakePaddle::start().await.unwrap();
        let event = subscription(
            &project,
            "evt_unsigned",
            "subscription.activated",
            "active",
            now(),
        );
        let response = send_event(&ctx, &other, &event).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "bad_signature"
        );
        assert_eq!(plan_row(&ctx, &project).await, before);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM paddle_events WHERE event_id <> $1",
                ""
            )
            .await,
            0
        );
    })
    .await;
}

#[tokio::test]
async fn it_stale_webhook_refused() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_, project) = group(&ctx, "acme").await;
        let before = plan_row(&ctx, &project).await;
        let body = serde_json::to_vec(&subscription(
            &project,
            "evt_stale",
            "subscription.activated",
            "active",
            now(),
        ))
        .unwrap();
        let ts = (now() - Duration::from_secs(STALE_SECONDS)).as_second();
        let response = ctx
            .http
            .post(ctx.hub.url("/v1/paddle/webhook"))
            .header("Paddle-Signature", paddle.sign(&body, ts))
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "bad_signature"
        );
        assert_eq!(plan_row(&ctx, &project).await, before);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM paddle_events WHERE event_id <> $1",
                ""
            )
            .await,
            0
        );
    })
    .await;
}

#[tokio::test]
async fn it_activation_ends_trial() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (access, project) = group(&ctx, "acme").await;
        assert_eq!(plan_row(&ctx, &project).await["state"], "trial");
        let event = subscription(
            &project,
            "evt_active",
            "subscription.activated",
            "active",
            now(),
        );
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        assert_eq!(me(&ctx, &access).await["groups"][0]["state"], "active");
        let response = ctx
            .http
            .get(
                ctx.hub
                    .url(&format!("/v1/tenants/{}/seats", project.tenant)),
            )
            .bearer_auth(&access)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["paid"], 25);
        let row = plan_row(&ctx, &project).await;
        assert_eq!(row["paddle_customer_id"], "ctm_1");
        assert_eq!(row["paddle_subscription_id"], "sub_1");
    })
    .await;
}

#[tokio::test]
async fn it_subscription_event_found_by_stored_id() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_, project) = group(&ctx, "acme").await;
        let at = now();
        activate(&ctx, &project, at - Duration::from_secs(1)).await;
        sqlx::query(
            "UPDATE tenant_plans SET paddle_subscription_id = 'sub_1' WHERE tenant_id = $1",
        )
        .bind(&project.tenant)
        .execute(&ctx.pool)
        .await
        .unwrap();
        let mut event = subscription(
            &project,
            "evt_stored",
            "subscription.past_due",
            "past_due",
            at,
        );
        event["data"]["custom_data"] = Value::Null;
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        assert_eq!(plan_row(&ctx, &project).await["state"], "past_due");
    })
    .await;
}

#[tokio::test]
async fn it_past_due_keeps_access_and_alerts() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (access, project) = group(&ctx, "acme").await;
        let at = now();
        activate(&ctx, &project, at - Duration::from_secs(1)).await;
        let event = subscription(&project, "evt_due", "subscription.past_due", "past_due", at);
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        assert_eq!(plan_row(&ctx, &project).await["state"], "past_due");
        post_fyi(&ctx, &project).await;
        let shown = me(&ctx, &access).await;
        let alerts = alerts(&ctx, &project).await;
        assert_eq!(alerts.len(), 1);
        assert!(
            alerts[0]
                .mentioned
                .contains(&shown["id"].as_str().unwrap().parse().unwrap())
        );
        let text = alerts[0].client.body.as_str();
        assert!(text.contains("andy/acme"), "{text}");
        assert!(
            text.contains(&(at + Duration::from_secs(14 * SECS_PER_DAY)).to_string()),
            "{text}"
        );
    })
    .await;
}

#[tokio::test]
async fn it_payment_restores_read_only() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_, project) = group(&ctx, "acme").await;
        let at = now();
        let record = PlanRecord {
            state: agmen_hub::PlanState::ReadOnly,
            plan: Some(Plan::Team { extra_seats: 0 }),
            trial_started_at: None,
            past_due_since: Some(at - Duration::from_secs(15 * SECS_PER_DAY)),
            read_only_cause: Some(ReadOnlyCause::PaymentFailed),
            read_only_since: Some(at - Duration::from_secs(SECS_PER_DAY)),
            reminders_sent: 0,
            last_event_at: Some(at - Duration::from_secs(1)),
        };
        ctx.hub
            .hub
            .set_plan(topics(&project).tenant(), &record)
            .await
            .unwrap();
        let mut event = subscription(
            &project,
            "evt_paid",
            "transaction.completed",
            "completed",
            at,
        );
        event["data"]["id"] = json!("txn_1");
        event["data"]["subscription_id"] = json!("sub_1");
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        assert_eq!(plan_row(&ctx, &project).await["state"], "active");
        post_fyi(&ctx, &project).await;
    })
    .await;
}

#[tokio::test]
async fn it_webhook_repeat_is_idempotent() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_, project) = group(&ctx, "acme").await;
        let at = now();
        activate(&ctx, &project, at - Duration::from_secs(1)).await;
        let event = subscription(
            &project,
            "evt_repeat",
            "subscription.past_due",
            "past_due",
            at,
        );
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        let before = plan_row(&ctx, &project).await;
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        assert_eq!(plan_row(&ctx, &project).await, before);
        assert_eq!(
            before["past_due_since"]
                .as_str()
                .unwrap()
                .parse::<Timestamp>()
                .unwrap(),
            at
        );
        assert_eq!(alerts(&ctx, &project).await.len(), 1);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM paddle_events WHERE event_id <> $1",
                ""
            )
            .await,
            1
        );
    })
    .await;
}

#[tokio::test]
async fn it_webhook_out_of_order_keeps_newest() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_, project) = group(&ctx, "acme").await;
        let at = now();
        let mut newest = subscription(&project, "evt_new", "subscription.updated", "active", at);
        newest["data"]["id"] = json!("sub_new");
        newest["data"]["customer_id"] = json!("ctm_new");
        assert_eq!(
            send_event(&ctx, &paddle, &newest).await.status(),
            StatusCode::OK
        );
        let before = plan_row(&ctx, &project).await;
        let old = subscription(
            &project,
            "evt_old",
            "subscription.past_due",
            "past_due",
            at - Duration::from_secs(1),
        );
        assert_eq!(
            send_event(&ctx, &paddle, &old).await.status(),
            StatusCode::OK
        );
        assert_eq!(plan_row(&ctx, &project).await, before);
        assert_eq!(before["state"], "active");
        assert_eq!(before["paddle_customer_id"], "ctm_new");
        assert_eq!(before["paddle_subscription_id"], "sub_new");
        assert!(alerts(&ctx, &project).await.is_empty());
    })
    .await;
}

#[tokio::test]
async fn it_webhook_for_other_tenant_only_changes_it() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_, first) = group(&ctx, "acme").await;
        let (_, second) = group(&ctx, "beta").await;
        let before = plan_row(&ctx, &first).await;
        let event = subscription(
            &second,
            "evt_other",
            "subscription.activated",
            "active",
            now(),
        );
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        assert_eq!(plan_row(&ctx, &first).await, before);
        assert_eq!(plan_row(&ctx, &second).await["state"], "active");
    })
    .await;
}

#[tokio::test]
async fn it_unusable_event_answers_200_and_is_not_recorded() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_, project) = group(&ctx, "acme").await;
        let before = plan_row(&ctx, &project).await;
        let mut event = subscription(
            &project,
            "evt_unusable",
            "subscription.activated",
            "active",
            now(),
        );
        event["data"]["items"][0]["price"]["id"] = json!("pri_x");
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        assert_eq!(plan_row(&ctx, &project).await, before);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM paddle_events WHERE event_id <> $1",
                ""
            )
            .await,
            0
        );
        event["data"]["items"][0]["price"]["id"] = json!("pri_business_25");
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        assert_eq!(plan_row(&ctx, &project).await["state"], "active");
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM paddle_events WHERE event_id <> $1",
                ""
            )
            .await,
            1
        );
    })
    .await;
}

#[tokio::test]
async fn it_webhook_without_plan_or_stored_subscription_is_not_recorded() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_, project) = group(&ctx, "acme").await;
        sqlx::query("DELETE FROM tenant_plans WHERE tenant_id = $1")
            .bind(&project.tenant)
            .execute(&ctx.pool)
            .await
            .unwrap();
        let mut event = subscription(
            &project,
            "evt_missing_plan",
            "subscription.activated",
            "active",
            now(),
        );
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        event["event_id"] = json!("evt_missing_subscription");
        event["data"]["custom_data"] = Value::Null;
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM tenant_plans WHERE tenant_id = $1",
                &project.tenant
            )
            .await,
            0
        );
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM paddle_events WHERE event_id <> $1",
                ""
            )
            .await,
            0
        );
    })
    .await;
}

#[tokio::test]
async fn it_webhook_subscription_id_collision_rolls_back() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_, first) = group(&ctx, "acme").await;
        let (_, second) = group(&ctx, "beta").await;
        let event = subscription(
            &first,
            "evt_holds_subscription",
            "subscription.activated",
            "active",
            now(),
        );
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        let first_before = plan_row(&ctx, &first).await;
        let second_before = plan_row(&ctx, &second).await;
        let event = subscription(
            &second,
            "evt_conflicting_subscription",
            "subscription.activated",
            "active",
            now(),
        );
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        assert_eq!(plan_row(&ctx, &first).await, first_before);
        assert_eq!(plan_row(&ctx, &second).await, second_before);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM paddle_events WHERE event_id <> $1",
                ""
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM paddle_events WHERE event_id = $1",
                "evt_conflicting_subscription"
            )
            .await,
            0
        );
    })
    .await;
}

#[tokio::test]
async fn it_webhook_not_on_self_host() {
    run_in_mode(HubMode::SelfHost, |ctx| async move {
        let response = ctx
            .http
            .post(ctx.hub.url("/v1/paddle/webhook"))
            .body("{")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "not_hosted"
        );
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM paddle_events WHERE event_id <> $1",
                ""
            )
            .await,
            0
        );
    })
    .await;
}
