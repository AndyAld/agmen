//! Signed Paddle delivery, tenant isolation, ordering, and plan transitions.
#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_hub::{
    BillingEvent, BusinessStep, HUB_IDENTITY, HubMode, Plan, PlanRecord, ReadOnlyCause,
};
use agmen_proto::{DeliveryReply, StampedEnvelope, Timestamp};
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};
use support::fake_paddle::FakePaddle;
use support::{
    Ctx, Project, count, envelope, login, member_with_token, mint_ok, run_hosted_with_paddle,
    run_in_mode, topics, ulid,
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

async fn set_active(ctx: &Ctx, project: &Project, plan: Plan) {
    let at = now();
    let record = PlanRecord::trial(at).apply(BillingEvent::Active(plan), at);
    ctx.hub
        .hub
        .set_plan(topics(project).tenant(), &record)
        .await
        .unwrap();
}

async fn seed_subscription(ctx: &Ctx, project: &Project) {
    sqlx::query(
        "UPDATE tenant_plans
         SET paddle_customer_id = 'ctm_test', paddle_subscription_id = 'sub_test'
         WHERE tenant_id = $1",
    )
    .bind(&project.tenant)
    .execute(&ctx.pool)
    .await
    .unwrap();
}

async fn call(
    ctx: &Ctx,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = ctx
        .http
        .request(method, ctx.hub.url(path))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn put_seats(ctx: &Ctx, token: &str, tenant: &str, paid: u32) -> (StatusCode, Value) {
    call(
        ctx,
        token,
        reqwest::Method::PUT,
        &format!("/v1/tenants/{tenant}/seats"),
        Some(json!({"paid": paid})),
    )
    .await
}

async fn get_seats(ctx: &Ctx, token: &str, tenant: &str) -> Value {
    let (status, body) = call(
        ctx,
        token,
        reqwest::Method::GET,
        &format!("/v1/tenants/{tenant}/seats"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn paddle_body(paddle: &FakePaddle, method: &str, path: &str) -> Value {
    let requests = paddle.requests();
    let mut matched = requests
        .iter()
        .filter(|(seen, seen_path, _)| seen.as_str() == method && seen_path == path);
    let Some((_, _, body)) = matched.next() else {
        panic!("no {method} {path} in {requests:?}");
    };
    assert!(
        matched.next().is_none(),
        "more than one {method} {path} in {requests:?}"
    );
    body.clone()
}

fn assert_no_paddle(paddle: &FakePaddle) {
    let requests = paddle.requests();
    assert!(requests.is_empty(), "{requests:?}");
}

fn checkout_url(paddle: &FakePaddle) -> String {
    format!("{}checkout/txn_1", paddle.base())
}

#[tokio::test]
async fn it_trial_seat_add_starts_checkout() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 5).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"checkout_url": checkout_url(&paddle)}));
        let sent = paddle_body(&paddle, "POST", "/transactions");
        assert_eq!(
            sent["items"],
            json!([
                {"price_id": "pri_team_base", "quantity": 1},
                {"price_id": "pri_team_seat", "quantity": 1},
            ])
        );
        assert_eq!(sent["custom_data"]["agmen_tenant"], project.tenant);
        assert_eq!(get_seats(&ctx, &token, &project.tenant).await["paid"], 4);
    })
    .await;
}

#[tokio::test]
async fn it_trial_seats_up_to_four_change_nothing() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 4).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": null, "seats_paid": 4}));
        assert_no_paddle(&paddle);
    })
    .await;
}

#[tokio::test]
async fn it_team_seat_add_updates_quantity() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 0 }).await;
        seed_subscription(&ctx, &project).await;
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": "team", "seats_paid": 6}));
        let sent = paddle_body(&paddle, "PATCH", "/subscriptions/sub_test");
        assert_eq!(
            sent["items"],
            json!([
                {"price_id": "pri_team_base", "quantity": 1},
                {"price_id": "pri_team_seat", "quantity": 2},
            ])
        );
        assert_eq!(sent["proration_billing_mode"], "prorated_immediately");
        assert_eq!(get_seats(&ctx, &token, &project.tenant).await["paid"], 6);
    })
    .await;
}

#[tokio::test]
async fn it_team_seats_to_four_drops_the_seat_item() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 2 }).await;
        seed_subscription(&ctx, &project).await;
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 4).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": "team", "seats_paid": 4}));
        let sent = paddle_body(&paddle, "PATCH", "/subscriptions/sub_test");
        assert_eq!(
            sent["items"],
            json!([{"price_id": "pri_team_base", "quantity": 1}])
        );
        assert_eq!(get_seats(&ctx, &token, &project.tenant).await["paid"], 4);
    })
    .await;
}

#[tokio::test]
async fn it_business_eleventh_seat_moves_to_25() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Business(BusinessStep::Seats10)).await;
        seed_subscription(&ctx, &project).await;
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 11).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": "business-25", "seats_paid": 25}));
        let sent = paddle_body(&paddle, "PATCH", "/subscriptions/sub_test");
        assert_eq!(
            sent["items"],
            json!([{"price_id": "pri_business_25", "quantity": 1}])
        );
        assert_eq!(get_seats(&ctx, &token, &project.tenant).await["paid"], 25);
    })
    .await;
}

#[tokio::test]
async fn it_seats_below_used_refused() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        for (sub, handle) in [(11_u64, "ada"), (12, "bea"), (13, "cy"), (14, "dee")] {
            member_with_token(&ctx, &project, sub, handle, "member").await;
        }
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 4).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "seats_in_use");
        assert_eq!(body["error_description"], "remove members first");
        assert_no_paddle(&paddle);
    })
    .await;
}

#[tokio::test]
async fn it_seats_over_50_refused() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 51).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], "seats_over_50");
        assert_eq!(
            body["error_description"],
            "more than 50 seats is custom: write to sales@getagmen.ai"
        );
        assert_no_paddle(&paddle);
    })
    .await;
}

#[tokio::test]
async fn it_operator_group_has_no_subscription() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 0 }).await;
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "no_subscription");
        assert_eq!(
            body["error_description"],
            "this group is billed outside Paddle"
        );
        assert_no_paddle(&paddle);
    })
    .await;
}

#[tokio::test]
async fn it_team_to_business() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 0 }).await;
        seed_subscription(&ctx, &project).await;
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{}/plan", project.tenant),
            Some(json!({"plan": "business"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": "business-10", "seats_paid": 10}));
        let sent = paddle_body(&paddle, "PATCH", "/subscriptions/sub_test");
        assert_eq!(
            sent["items"],
            json!([{"price_id": "pri_business_10", "quantity": 1}])
        );
    })
    .await;
}

#[tokio::test]
async fn it_plan_change_without_subscription_checks_out() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{}/plan", project.tenant),
            Some(json!({"plan": "business", "seats": 25})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"checkout_url": checkout_url(&paddle)}));
        let sent = paddle_body(&paddle, "POST", "/transactions");
        assert_eq!(
            sent["items"],
            json!([{"price_id": "pri_business_25", "quantity": 1}])
        );
    })
    .await;
}

#[tokio::test]
async fn it_billing_owner_only() {
    run_hosted_with_paddle(|ctx, _paddle| async move {
        let (_owner, project) = group(&ctx, "acme").await;
        let admin = member_with_token(&ctx, &project, 21, "ada", "admin").await;
        let member = member_with_token(&ctx, &project, 22, "mel", "member").await;
        let plan = format!("/v1/tenants/{}/plan", project.tenant);
        let portal = format!("/v1/tenants/{}/portal", project.tenant);
        let (status, body) = call(
            &ctx,
            &admin,
            reqwest::Method::POST,
            &plan,
            Some(json!({"plan": "business"})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "owner_required");
        assert_eq!(
            body["error_description"],
            "only the owner can change billing"
        );
        let (status, body) = call(&ctx, &admin, reqwest::Method::POST, &portal, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "owner_required");
        let (status, body) = put_seats(&ctx, &admin, &project.tenant, 4).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": null, "seats_paid": 4}));
        let (status, body) = put_seats(&ctx, &member, &project.tenant, 4).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "group_admin_required");
    })
    .await;
}

#[tokio::test]
async fn it_portal_needs_subscription() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        let portal = format!("/v1/tenants/{}/portal", project.tenant);
        let (status, body) = call(&ctx, &token, reqwest::Method::POST, &portal, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "no_subscription");
        assert_eq!(
            body["error_description"],
            "this group has no subscription yet: agmen group seats add 1 starts one"
        );
        assert_no_paddle(&paddle);
        seed_subscription(&ctx, &project).await;
        let (status, body) = call(&ctx, &token, reqwest::Method::POST, &portal, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            json!({"portal_url": format!("{}portal/ctm_test", paddle.base())})
        );
        let sent = paddle_body(&paddle, "POST", "/customers/ctm_test/portal-sessions");
        assert_eq!(sent["subscription_ids"], json!(["sub_test"]));
    })
    .await;
}

#[tokio::test]
async fn it_paddle_refusal_is_502_and_changes_nothing() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 0 }).await;
        seed_subscription(&ctx, &project).await;
        paddle.fail_next(400, json!({"error": {"code": "bad_request"}}));
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(body["error"], "upstream_unavailable");
        assert_eq!(get_seats(&ctx, &token, &project.tenant).await["paid"], 4);
    })
    .await;
}

#[tokio::test]
async fn it_second_group_gets_checkout_url() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, _) = group(&ctx, "acme").await;
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            "/v1/groups",
            Some(json!({"name": "beta", "project": "web"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["checkout_url"], checkout_url(&paddle));
        let sent = paddle_body(&paddle, "POST", "/transactions");
        assert_eq!(
            sent["items"],
            json!([{"price_id": "pri_team_base", "quantity": 1}])
        );
        assert_eq!(sent["custom_data"]["agmen_tenant"], body["tenant"]);
    })
    .await;
}

#[tokio::test]
async fn it_billing_routes_not_on_self_host() {
    run_in_mode(HubMode::SelfHost, |ctx| async move {
        let token = login(&ctx, 4242, "andy").await;
        let tenant = "selfhosttenant1";
        let (status, body) = put_seats(&ctx, &token, tenant, 4).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"], "not_hosted");
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{tenant}/plan"),
            Some(json!({"plan": "team"})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"], "not_hosted");
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{tenant}/portal"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"], "not_hosted");
    })
    .await;
}

#[tokio::test]
async fn it_billing_refuses_members_strangers_and_agents() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (owner, project) = group(&ctx, "acme").await;
        let member = member_with_token(&ctx, &project, 22, "mel", "member").await;
        let stranger = login(&ctx, 31, "eve").await;
        let agent = mint_ok(&ctx, &owner, "andy/bot", &project).await.token;
        // A subscribed Team group, so a call let through would reach Paddle.
        set_active(&ctx, &project, Plan::Team { extra_seats: 0 }).await;
        seed_subscription(&ctx, &project).await;
        let tenant = project.tenant.as_str();
        let plan = format!("/v1/tenants/{tenant}/plan");
        let portal = format!("/v1/tenants/{tenant}/portal");
        for (who, token, seats_error, owner_error) in [
            ("member", &member, "group_admin_required", "owner_required"),
            (
                "stranger",
                &stranger,
                "group_admin_required",
                "owner_required",
            ),
            (
                "agent",
                &agent,
                "human_token_required",
                "human_token_required",
            ),
        ] {
            let (status, body) = put_seats(&ctx, token, tenant, 6).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {body}");
            assert_eq!(body["error"], seats_error, "{who}: {body}");
            let business = Some(json!({"plan": "business"}));
            let (status, body) = call(&ctx, token, reqwest::Method::POST, &plan, business).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {body}");
            assert_eq!(body["error"], owner_error, "{who}: {body}");
            let (status, body) = call(&ctx, token, reqwest::Method::POST, &portal, None).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {body}");
            assert_eq!(body["error"], owner_error, "{who}: {body}");
        }
        assert_no_paddle(&paddle);
        assert_eq!(get_seats(&ctx, &owner, tenant).await["paid"], 4);
    })
    .await;
}

#[tokio::test]
async fn it_seat_change_stores_only_the_seat_count() {
    run_hosted_with_paddle(|ctx, _paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        activate(&ctx, &project, now() - Duration::from_secs(3_600)).await;
        seed_subscription(&ctx, &project).await;
        let before = plan_row(&ctx, &project).await;
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // The webhook confirms the rest: not the state, nor `last_event_at`.
        let mut expected = before.clone();
        expected["extra_seats"] = json!(2);
        assert_eq!(plan_row(&ctx, &project).await, expected);
    })
    .await;
}

#[tokio::test]
async fn it_webhook_after_a_seat_change_still_applies() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        activate(&ctx, &project, now() - Duration::from_secs(3_600)).await;
        seed_subscription(&ctx, &project).await;
        // Paddle stamps its event before the hub's clock reads the answer to
        // the seat change, so a hub that stamped its own clock would drop it.
        let event_at = now() - Duration::from_secs(300);
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let event = subscription(
            &project,
            "evt_after_put",
            "subscription.updated",
            "active",
            event_at,
        );
        assert_eq!(
            send_event(&ctx, &paddle, &event).await.status(),
            StatusCode::OK
        );
        let row = plan_row(&ctx, &project).await;
        assert_eq!(row["plan"], "business");
        assert_eq!(row["business_step"], 25);
    })
    .await;
}

#[tokio::test]
async fn it_seat_change_retry_is_idempotent() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (owner, project) = group(&ctx, "acme").await;
        let admin = member_with_token(&ctx, &project, 21, "ada", "admin").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 0 }).await;
        seed_subscription(&ctx, &project).await;
        let first = put_seats(&ctx, &owner, &project.tenant, 6).await;
        let stored = plan_row(&ctx, &project).await;
        // The retry comes from a group admin, who may change seats too.
        let second = put_seats(&ctx, &admin, &project.tenant, 6).await;
        assert_eq!(
            first,
            (StatusCode::OK, json!({"plan": "team", "seats_paid": 6}))
        );
        assert_eq!(second, first);
        assert_eq!(plan_row(&ctx, &project).await, stored);
        let requests = paddle.requests();
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert_eq!(requests[0], requests[1]);
    })
    .await;
}

#[tokio::test]
async fn it_subscription_without_a_stored_plan_is_not_active_yet() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        // The webhook wrote the ids, and the plan has not landed yet.
        seed_subscription(&ctx, &project).await;
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "no_subscription");
        assert_eq!(
            body["error_description"],
            "the subscription is not active yet; try again in a minute"
        );
        assert_no_paddle(&paddle);
    })
    .await;
}

#[tokio::test]
async fn it_items_paddle_returns_that_are_not_a_plan_change_nothing() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 0 }).await;
        seed_subscription(&ctx, &project).await;
        let before = plan_row(&ctx, &project).await;
        paddle.fail_next(
            200,
            json!({"data": {"items": [
                {"status": "active", "quantity": 1, "price": {"id": "pri_unknown"}},
            ]}}),
        );
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(body["error"], "upstream_unavailable");
        assert_eq!(plan_row(&ctx, &project).await, before);
    })
    .await;
}

#[tokio::test]
async fn it_paddle_refusal_leaves_the_whole_plan_row_alone() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Business(BusinessStep::Seats10)).await;
        seed_subscription(&ctx, &project).await;
        let before = plan_row(&ctx, &project).await;
        paddle.fail_next(409, json!({"error": {"code": "subscription_locked"}}));
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 11).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(body["error"], "upstream_unavailable");
        assert_eq!(body["error_description"], "Paddle call failed");
        assert_eq!(plan_row(&ctx, &project).await, before);
        // The retry goes through once Paddle answers.
        let (status, body) = put_seats(&ctx, &token, &project.tenant, 11).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": "business-25", "seats_paid": 25}));
    })
    .await;
}

#[tokio::test]
async fn it_second_group_survives_paddle_failing_and_checks_out_later() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, _) = group(&ctx, "acme").await;
        paddle.fail_next(500, json!({"error": {"code": "internal_error"}}));
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            "/v1/groups",
            Some(json!({"name": "beta", "project": "web"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body["checkout_url"].is_null(), "{body}");
        assert_eq!(body["state"], "read_only");
        let tenant = body["tenant"].as_str().unwrap();
        assert_eq!(paddle.requests().len(), 1);
        let (status, later) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{tenant}/plan"),
            Some(json!({"plan": "team"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{later}");
        assert_eq!(later, json!({"checkout_url": checkout_url(&paddle)}));
        let requests = paddle.requests();
        assert_eq!(requests.len(), 2, "{requests:?}");
        let sent = &requests[1].2;
        assert_eq!(
            sent["items"],
            json!([{"price_id": "pri_team_base", "quantity": 1}])
        );
        assert_eq!(sent["custom_data"]["agmen_tenant"], tenant);
    })
    .await;
}

#[tokio::test]
async fn it_plan_route_checks_its_body() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        member_with_token(&ctx, &project, 22, "mel", "member").await;
        let path = format!("/v1/tenants/{}/plan", project.tenant);
        for (body, expected, code) in [
            (
                json!({"plan": "free"}),
                StatusCode::UNPROCESSABLE_ENTITY,
                "bad_plan",
            ),
            (
                json!({"plan": "team", "seats": 51}),
                StatusCode::UNPROCESSABLE_ENTITY,
                "seats_over_50",
            ),
            (
                json!({"plan": "business", "seats": 51}),
                StatusCode::UNPROCESSABLE_ENTITY,
                "seats_over_50",
            ),
            (
                json!({"plan": "team", "seats": 1}),
                StatusCode::CONFLICT,
                "seats_in_use",
            ),
        ] {
            let (status, answer) = call(
                &ctx,
                &token,
                reqwest::Method::POST,
                &path,
                Some(body.clone()),
            )
            .await;
            assert_eq!(status, expected, "{body}: {answer}");
            assert_eq!(answer["error"], code, "{body}: {answer}");
        }
        let unknown = Some(json!({"plan": "team", "coupon": "free"}));
        let (status, _) = call(&ctx, &token, reqwest::Method::POST, &path, unknown).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let seats_path = format!("/v1/tenants/{}/seats", project.tenant);
        let unknown = Some(json!({"paid": 5, "coupon": "free"}));
        let (status, _) = call(&ctx, &token, reqwest::Method::PUT, &seats_path, unknown).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_no_paddle(&paddle);
    })
    .await;
}
