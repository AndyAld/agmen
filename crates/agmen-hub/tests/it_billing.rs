//! Signed Paddle delivery, tenant isolation, ordering, and plan transitions.
#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_hub::{
    BillingEvent, BusinessStep, HUB_IDENTITY, HubMode, Plan, PlanRecord, PlanState, ReadOnlyCause,
};
use agmen_proto::{DeliveryReply, StampedEnvelope, Timestamp};
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};
use support::fake_paddle::FakePaddle;
use support::{
    Ctx, Project, count, envelope, login, member_with_token, mint_ok, run_hosted_with_paddle,
    run_hosted_with_public_url, run_in_mode, topics, ulid,
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
    format!("{}checkout/txn_1", paddle.link_base())
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
            json!({"portal_url": format!("{}portal/ctm_test", paddle.link_base())})
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

async fn send_for_retry(
    ctx: &Ctx,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let mut request = ctx
        .http
        .request(method, ctx.hub.url(path))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let retry = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    (status, response.json().await.unwrap_or(Value::Null), retry)
}

/// The limiter refills one call every 10 s and counts whole seconds from the
/// first call, so a slow test may see a few seconds less than 10.
fn assert_retry_within_a_refill(retry: Option<&str>) {
    let secs: u64 = retry
        .expect("a Retry-After header")
        .parse()
        .expect("Retry-After in whole seconds");
    assert!((1..=10).contains(&secs), "Retry-After {secs}");
}

/// A request with no token, or with `token` when given: its status only.
async fn status_of(
    ctx: &Ctx,
    token: Option<&str>,
    method: &reqwest::Method,
    path: &str,
    body: Option<&Value>,
) -> StatusCode {
    let mut request = ctx.http.request(method.clone(), ctx.hub.url(path));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(body);
    }
    request.send().await.unwrap().status()
}

#[tokio::test]
async fn it_refused_billing_calls_do_not_use_the_limit() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (owner, project) = group(&ctx, "acme").await;
        let member = member_with_token(&ctx, &project, 61, "mel", "member").await;
        let admin = member_with_token(&ctx, &project, 62, "ada", "admin").await;
        let stranger = login(&ctx, 63, "eve").await;
        let tenant = project.tenant.as_str();
        let routes = [
            (
                reqwest::Method::PUT,
                format!("/v1/tenants/{tenant}/seats"),
                Some(json!({"paid": 5})),
            ),
            (
                reqwest::Method::POST,
                format!("/v1/tenants/{tenant}/plan"),
                Some(json!({"plan": "business"})),
            ),
            (
                reqwest::Method::POST,
                format!("/v1/tenants/{tenant}/portal"),
                None,
            ),
        ];
        // Seven rounds, one more than the limit. Nobody here may use a call.
        for round in 1..=7 {
            for (method, path, body) in &routes {
                for (who, token, expected) in [
                    ("no token", None, StatusCode::UNAUTHORIZED),
                    ("bad token", Some("not-a-token"), StatusCode::UNAUTHORIZED),
                    ("member", Some(member.as_str()), StatusCode::FORBIDDEN),
                    ("stranger", Some(stranger.as_str()), StatusCode::FORBIDDEN),
                ] {
                    let status = status_of(&ctx, token, method, path, body.as_ref()).await;
                    assert_eq!(status, expected, "round {round} {who} {method} {path}");
                }
            }
            // A group admin may set seats, but not change the plan or open the portal.
            for (method, path, body) in routes.iter().skip(1) {
                let status = status_of(&ctx, Some(&admin), method, path, body.as_ref()).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "round {round} admin {path}");
            }
        }
        assert_no_paddle(&paddle);
        // The owner still has all six calls, and the seventh is the one refused.
        for n in 1..=6 {
            let (status, body) = put_seats(&ctx, &owner, tenant, 5).await;
            assert_eq!(status, StatusCode::OK, "owner call {n}: {body}");
        }
        let (status, body) = put_seats(&ctx, &owner, tenant, 5).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(paddle.requests().len(), 6, "{:?}", paddle.requests());
    })
    .await;
}

#[tokio::test]
async fn it_paddle_urls_the_hub_will_not_send_are_refused() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        let tenant = project.tenant.as_str();
        let other_host =
            json!({"data": {"checkout": {"url": "https://evil.example/pay?_ptxn=txn_1"}}});
        let plain_http =
            json!({"data": {"checkout": {"url": "http://checkout.paddle.com/pay?_ptxn=txn_1"}}});
        let with_userinfo =
            json!({"data": {"checkout": {"url": "https://u:p@checkout.paddle.com/pay"}}});
        // A trial group asking for a fifth seat starts a checkout.
        for (n, answer) in [other_host.clone(), plain_http, with_userinfo]
            .into_iter()
            .enumerate()
        {
            paddle.fail_next(200, answer);
            let (status, body) = put_seats(&ctx, &token, tenant, 5).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "answer {n}: {body}");
            assert_eq!(body["error"], "paddle_bad_url", "answer {n}: {body}");
            assert!(!body.to_string().contains("evil.example"), "{body}");
        }
        paddle.fail_next(200, other_host.clone());
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{tenant}/plan"),
            Some(json!({"plan": "team"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(body["error"], "paddle_bad_url");
        // The group is still created, with no checkout link.
        paddle.fail_next(200, other_host);
        let (status, created) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            "/v1/groups",
            Some(json!({"name": "beta", "project": "web"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["state"], "read_only");
        assert!(created["checkout_url"].is_null(), "{created}");
        // The portal link gets the same check.
        set_active(&ctx, &project, Plan::Team { extra_seats: 0 }).await;
        seed_subscription(&ctx, &project).await;
        paddle.fail_next(
            200,
            json!({"data": {"urls": {"general": {"overview": "https://evil.example/portal"}}}}),
        );
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{tenant}/portal"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(body["error"], "paddle_bad_url");
        assert!(!body.to_string().contains("evil.example"), "{body}");
    })
    .await;
}

#[tokio::test]
async fn it_checkout_links_on_the_public_domain_are_returned() {
    run_hosted_with_public_url("https://app.getagmen.ai", |ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        let tenant = project.tenant.as_str();
        // Paddle's default payment link is a page on the hub's site.
        let link = "https://getagmen.ai/checkout?_ptxn=txn_1";
        paddle.fail_next(200, json!({"data": {"checkout": {"url": link}}}));
        let (status, body) = put_seats(&ctx, &token, tenant, 5).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["checkout_url"], link, "{body}");
        // A link on a lookalike host is still refused.
        paddle.fail_next(
            200,
            json!({"data": {"checkout": {"url": "https://evil-getagmen.ai/checkout?_ptxn=txn_2"}}}),
        );
        let (status, body) = put_seats(&ctx, &token, tenant, 5).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(body["error"], "paddle_bad_url");
        // The group creation path returns the link too.
        paddle.fail_next(200, json!({"data": {"checkout": {"url": link}}}));
        let (status, created) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            "/v1/groups",
            Some(json!({"name": "beta", "project": "web"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["checkout_url"], link, "{created}");
        // The portal link gets the same rule.
        set_active(&ctx, &project, Plan::Team { extra_seats: 0 }).await;
        seed_subscription(&ctx, &project).await;
        let portal = "https://billing.getagmen.ai/portal";
        paddle.fail_next(
            200,
            json!({"data": {"urls": {"general": {"overview": portal}}}}),
        );
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{tenant}/portal"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["portal_url"], portal, "{body}");
    })
    .await;
}

#[tokio::test]
async fn it_billing_routes_are_rate_limited() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        let member = member_with_token(&ctx, &project, 61, "mel", "member").await;
        let (status, body) = put_seats(&ctx, &member, &project.tenant, 5).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "group_admin_required");
        assert_no_paddle(&paddle);
        for n in 1..=6 {
            let (status, body) = put_seats(&ctx, &token, &project.tenant, 5).await;
            assert_eq!(status, StatusCode::OK, "call {n}: {body}");
        }
        assert_eq!(paddle.requests().len(), 6);
        let (status, body, retry) = send_for_retry(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{}/plan", project.tenant),
            Some(json!({"plan": "business"})),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(body["error"], "rate_limited");
        assert_retry_within_a_refill(retry.as_deref());
        assert_eq!(paddle.requests().len(), 6, "{:?}", paddle.requests());

        let (status, created) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            "/v1/groups",
            Some(json!({"name": "beta", "project": "web"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let beta = Project {
            tenant: created["tenant"].as_str().unwrap().to_owned(),
            project: created["project"].as_str().unwrap().to_owned(),
        };
        set_active(&ctx, &beta, Plan::Team { extra_seats: 0 }).await;
        seed_subscription(&ctx, &beta).await;
        let before = paddle.requests().len();
        let portal = format!("/v1/tenants/{}/portal", beta.tenant);
        for n in 1..=5 {
            let (status, body) = call(&ctx, &token, reqwest::Method::POST, &portal, None).await;
            assert_eq!(status, StatusCode::OK, "portal {n}: {body}");
        }
        let (status, body, retry) = send_for_retry(
            &ctx,
            &token,
            reqwest::Method::PUT,
            &format!("/v1/tenants/{}/seats", beta.tenant),
            Some(json!({"paid": 6})),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(body["error"], "rate_limited");
        assert_retry_within_a_refill(retry.as_deref());
        assert_eq!(
            paddle.requests().len(),
            before + 5,
            "{:?}",
            paddle.requests()
        );
    })
    .await;
}

#[tokio::test]
async fn it_plan_route_refuses_operator_groups() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 0 }).await;
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{}/plan", project.tenant),
            Some(json!({"plan": "business"})),
        )
        .await;
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
async fn it_plan_route_waits_for_an_active_plan() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        seed_subscription(&ctx, &project).await;
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{}/plan", project.tenant),
            Some(json!({"plan": "team"})),
        )
        .await;
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

async fn ended_group(ctx: &Ctx, cause: ReadOnlyCause, plan: Plan) -> (String, Project) {
    let (token, project) = group(ctx, "ended").await;
    let at = now() - Duration::from_secs(SECS_PER_DAY);
    let mut record = PlanRecord::trial(at).apply(BillingEvent::Active(plan), at);
    record.state = PlanState::ReadOnly;
    record.read_only_cause = Some(cause);
    record.read_only_since = Some(at);
    if cause == ReadOnlyCause::PaymentFailed {
        record.past_due_since = Some(at - Duration::from_secs(14 * SECS_PER_DAY));
    }
    ctx.hub
        .hub
        .set_plan(topics(&project).tenant(), &record)
        .await
        .unwrap();
    seed_subscription(ctx, &project).await;
    (token, project)
}

async fn checkout_plan(
    ctx: &Ctx,
    token: &str,
    project: &Project,
    body: Value,
) -> (StatusCode, Value) {
    call(
        ctx,
        token,
        reqwest::Method::POST,
        &format!("/v1/tenants/{}/plan", project.tenant),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn it_canceled_group_portal_refuses_and_plan_checks_out_without_storing() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = ended_group(
            &ctx,
            ReadOnlyCause::Canceled,
            Plan::Team { extra_seats: 2 },
        )
        .await;
        let before = plan_row(&ctx, &project).await;
        let (status, body) = call(
            &ctx,
            &token,
            reqwest::Method::POST,
            &format!("/v1/tenants/{}/portal", project.tenant),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "no_subscription");
        assert_eq!(
            body["error_description"],
            "this group's subscription ended; its owner can start a new one with: agmen group billing"
        );
        assert_no_paddle(&paddle);
        let (status, body) = checkout_plan(&ctx, &token, &project, json!({"plan": "team"})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"checkout_url": checkout_url(&paddle)}));
        assert_eq!(
            paddle.requests(),
            vec![(
                reqwest::Method::POST,
                "/transactions".to_owned(),
                json!({
                    "items": [
                        {"price_id": "pri_team_base", "quantity": 1},
                        {"price_id": "pri_team_seat", "quantity": 2},
                    ],
                    "custom_data": {"agmen_tenant": project.tenant},
                }),
            )]
        );
        assert_eq!(plan_row(&ctx, &project).await, before);
    })
    .await;
}

#[tokio::test]
async fn it_payment_failed_group_cancels_before_checkout_without_storing() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = ended_group(
            &ctx,
            ReadOnlyCause::PaymentFailed,
            Plan::Team { extra_seats: 1 },
        )
        .await;
        let before = plan_row(&ctx, &project).await;
        let (status, body) = checkout_plan(
            &ctx,
            &token,
            &project,
            json!({"plan": "business", "seats": 25}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"checkout_url": checkout_url(&paddle)}));
        assert_eq!(
            paddle.requests(),
            vec![
                (
                    reqwest::Method::POST,
                    "/subscriptions/sub_test/cancel".to_owned(),
                    json!({"effective_from": "immediately"}),
                ),
                (
                    reqwest::Method::POST,
                    "/transactions".to_owned(),
                    json!({
                        "items": [{"price_id": "pri_business_25", "quantity": 1}],
                        "custom_data": {"agmen_tenant": project.tenant},
                    }),
                ),
            ]
        );
        assert_eq!(plan_row(&ctx, &project).await, before);
    })
    .await;
}

#[tokio::test]
async fn it_payment_failed_group_already_canceled_still_checks_out() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = ended_group(
            &ctx,
            ReadOnlyCause::PaymentFailed,
            Plan::Team { extra_seats: 0 },
        )
        .await;
        let before = plan_row(&ctx, &project).await;
        paddle.fail_next(
            400,
            json!({"error": {"code": "subscription_update_when_canceled"}}),
        );
        let (status, body) = checkout_plan(&ctx, &token, &project, json!({"plan": "team"})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"checkout_url": checkout_url(&paddle)}));
        let requests = paddle.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].1, "/subscriptions/sub_test/cancel");
        assert_eq!(requests[0].2, json!({"effective_from": "immediately"}));
        assert_eq!(requests[1].1, "/transactions");
        assert_eq!(plan_row(&ctx, &project).await, before);
    })
    .await;
}

#[tokio::test]
async fn it_payment_failed_group_canceled_action_still_checks_out() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = ended_group(
            &ctx,
            ReadOnlyCause::PaymentFailed,
            Plan::Team { extra_seats: 0 },
        )
        .await;
        let before = plan_row(&ctx, &project).await;
        paddle.fail_next(
            400,
            json!({"error": {"code": "subscription_is_canceled_action_invalid"}}),
        );
        let (status, body) = checkout_plan(&ctx, &token, &project, json!({"plan": "team"})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"checkout_url": checkout_url(&paddle)}));
        let requests = paddle.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].1, "/subscriptions/sub_test/cancel");
        assert_eq!(requests[0].2, json!({"effective_from": "immediately"}));
        assert_eq!(requests[1].1, "/transactions");
        assert_eq!(plan_row(&ctx, &project).await, before);
    })
    .await;
}

#[tokio::test]
async fn it_payment_failed_group_cancels_without_customer_id() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = ended_group(
            &ctx,
            ReadOnlyCause::PaymentFailed,
            Plan::Team { extra_seats: 0 },
        )
        .await;
        sqlx::query("UPDATE tenant_plans SET paddle_customer_id = NULL WHERE tenant_id = $1")
            .bind(&project.tenant)
            .execute(&ctx.pool)
            .await
            .unwrap();
        let before = plan_row(&ctx, &project).await;
        let (status, body) = checkout_plan(&ctx, &token, &project, json!({"plan": "team"})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"checkout_url": checkout_url(&paddle)}));
        let requests = paddle.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].1, "/subscriptions/sub_test/cancel");
        assert_eq!(requests[0].2, json!({"effective_from": "immediately"}));
        assert_eq!(requests[1].1, "/transactions");
        assert_eq!(plan_row(&ctx, &project).await, before);
    })
    .await;
}

#[tokio::test]
async fn it_payment_failed_group_cancel_failure_does_not_check_out() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = ended_group(
            &ctx,
            ReadOnlyCause::PaymentFailed,
            Plan::Team { extra_seats: 0 },
        )
        .await;
        let before = plan_row(&ctx, &project).await;
        paddle.fail_next(503, json!({"error": {"code": "unavailable"}}));
        let (status, body) = checkout_plan(&ctx, &token, &project, json!({"plan": "team"})).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(body["error"], "upstream_unavailable");
        assert_eq!(
            paddle.requests(),
            vec![(
                reqwest::Method::POST,
                "/subscriptions/sub_test/cancel".to_owned(),
                json!({"effective_from": "immediately"}),
            )]
        );
        assert_eq!(plan_row(&ctx, &project).await, before);
    })
    .await;
}

#[tokio::test]
async fn it_ended_group_seats_refuses_without_paddle() {
    for cause in [ReadOnlyCause::Canceled, ReadOnlyCause::PaymentFailed] {
        run_hosted_with_paddle(move |ctx, paddle| async move {
            let (token, project) = ended_group(&ctx, cause, Plan::Team { extra_seats: 0 }).await;
            let admin = member_with_token(&ctx, &project, 21, "ada", "admin").await;
            let before = plan_row(&ctx, &project).await;
            for caller in [&token, &admin] {
                let (status, body) = put_seats(&ctx, caller, &project.tenant, 6).await;
                assert_eq!(status, StatusCode::CONFLICT, "{body}");
                assert_eq!(body["error"], "no_subscription");
                assert_eq!(
                    body["error_description"],
                    "this group's subscription ended; its owner can start a new one with: agmen group billing"
                );
            }
            assert_no_paddle(&paddle);
            assert_eq!(plan_row(&ctx, &project).await, before);
        })
        .await;
    }
}

#[tokio::test]
async fn it_new_checkout_activation_ignores_delayed_old_cancellation() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = ended_group(
            &ctx,
            ReadOnlyCause::PaymentFailed,
            Plan::Team { extra_seats: 0 },
        )
        .await;
        let (status, body) =
            checkout_plan(&ctx, &token, &project, json!({"plan": "business"})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["checkout_url"].is_string(), "{body}");
        let at = now();
        let mut active = subscription(
            &project,
            "evt_new_subscription",
            "subscription.activated",
            "active",
            at,
        );
        active["data"]["id"] = json!("sub_new");
        active["data"]["customer_id"] = json!("ctm_new");
        assert_eq!(
            send_event(&ctx, &paddle, &active).await.status(),
            StatusCode::OK
        );
        let restored = plan_row(&ctx, &project).await;
        assert_eq!(restored["state"], "active");
        assert_eq!(restored["plan"], "business");
        assert_eq!(restored["business_step"], 25);
        assert!(restored["read_only_cause"].is_null());
        assert!(restored["read_only_since"].is_null());
        assert_eq!(restored["paddle_subscription_id"], "sub_new");
        assert_eq!(restored["paddle_customer_id"], "ctm_new");
        let mut canceled = subscription(
            &project,
            "evt_old_subscription_canceled",
            "subscription.canceled",
            "canceled",
            at - Duration::from_secs(1),
        );
        canceled["data"]["id"] = json!("sub_test");
        canceled["data"]["customer_id"] = json!("ctm_test");
        assert_eq!(
            send_event(&ctx, &paddle, &canceled).await.status(),
            StatusCode::OK
        );
        assert_eq!(plan_row(&ctx, &project).await, restored);
    })
    .await;
}

/// `event` with the subscription and customer ids Paddle would send for it.
fn of_subscription(mut event: Value, id: &str, customer: &str) -> Value {
    event["data"]["id"] = json!(id);
    event["data"]["customer_id"] = json!(customer);
    event
}

/// A `transaction.completed` event for `sub_id`.
fn payment(
    project: &Project,
    event_id: &str,
    sub_id: &str,
    customer: &str,
    at: Timestamp,
) -> Value {
    let mut event = subscription(project, event_id, "transaction.completed", "completed", at);
    event["data"]["id"] = json!("txn_1");
    event["data"]["subscription_id"] = json!(sub_id);
    event["data"]["customer_id"] = json!(customer);
    event
}

#[tokio::test]
async fn it_old_subscription_events_stamped_after_the_new_activation_change_nothing() {
    for past_due in [false, true] {
        run_hosted_with_paddle(move |ctx, paddle| async move {
            let (token, project) = ended_group(
                &ctx,
                ReadOnlyCause::PaymentFailed,
                Plan::Team { extra_seats: 0 },
            )
            .await;
            let (status, body) =
                checkout_plan(&ctx, &token, &project, json!({"plan": "business"})).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let at = now();
            let stamp = |seconds| at + Duration::from_secs(seconds);
            let mut new_events = vec![of_subscription(
                subscription(
                    &project,
                    "evt_new_active",
                    "subscription.activated",
                    "active",
                    at,
                ),
                "sub_new",
                "ctm_new",
            )];
            if past_due {
                new_events.push(of_subscription(
                    subscription(
                        &project,
                        "evt_new_past_due",
                        "subscription.past_due",
                        "past_due",
                        stamp(1),
                    ),
                    "sub_new",
                    "ctm_new",
                ));
            }
            for event in &new_events {
                assert_eq!(
                    send_event(&ctx, &paddle, event).await.status(),
                    StatusCode::OK
                );
            }
            let followed = plan_row(&ctx, &project).await;
            assert_eq!(
                followed["state"],
                if past_due { "past_due" } else { "active" }
            );
            assert_eq!(followed["plan"], "business");
            assert_eq!(followed["paddle_subscription_id"], "sub_new");
            assert_eq!(followed["paddle_customer_id"], "ctm_new");
            let alerts_before = alerts(&ctx, &project).await.len();
            // Paddle stamps these events of the old subscription after the new
            // one activated, so the event clock does not drop them.
            let late = [
                of_subscription(
                    subscription(
                        &project,
                        "evt_old_canceled",
                        "subscription.canceled",
                        "canceled",
                        stamp(10),
                    ),
                    "sub_test",
                    "ctm_test",
                ),
                of_subscription(
                    subscription(
                        &project,
                        "evt_old_past_due",
                        "subscription.past_due",
                        "past_due",
                        stamp(11),
                    ),
                    "sub_test",
                    "ctm_test",
                ),
                payment(&project, "evt_old_paid", "sub_test", "ctm_test", stamp(12)),
            ];
            for event in &late {
                assert_eq!(
                    send_event(&ctx, &paddle, event).await.status(),
                    StatusCode::OK
                );
                assert_eq!(
                    plan_row(&ctx, &project).await,
                    followed,
                    "{}",
                    event["event_type"]
                );
            }
            assert_eq!(alerts(&ctx, &project).await.len(), alerts_before);
        })
        .await;
    }
}

#[tokio::test]
async fn it_payment_delivered_before_the_new_activation_keeps_the_new_plan() {
    for custom_data in [true, false] {
        run_hosted_with_paddle(move |ctx, paddle| async move {
            let (token, project) =
                ended_group(&ctx, ReadOnlyCause::Canceled, Plan::Team { extra_seats: 0 }).await;
            let (status, body) = checkout_plan(
                &ctx,
                &token,
                &project,
                json!({"plan": "business", "seats": 25}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let at = now();
            // Paddle stamps the activation first and the payment a second
            // later, yet the payment arrives first. The group still holds its
            // old Team plan, so the payment must not close the door on the
            // activation that names the plan paid for.
            let paid = payment(
                &project,
                "evt_new_paid",
                "sub_new",
                "ctm_new",
                at + Duration::from_secs(1),
            );
            assert_eq!(
                send_event(&ctx, &paddle, &paid).await.status(),
                StatusCode::OK
            );
            let paid_row = plan_row(&ctx, &project).await;
            assert_eq!(paid_row["state"], "active");
            assert_eq!(paid_row["paddle_subscription_id"], "sub_new");
            assert_eq!(paid_row["paddle_customer_id"], "ctm_new");
            let mut active = of_subscription(
                subscription(
                    &project,
                    "evt_new_active",
                    "subscription.activated",
                    "active",
                    at,
                ),
                "sub_new",
                "ctm_new",
            );
            if !custom_data {
                // The hub finds the group by the subscription id the payment stored.
                active["data"]["custom_data"] = Value::Null;
            }
            assert_eq!(
                send_event(&ctx, &paddle, &active).await.status(),
                StatusCode::OK
            );
            let row = plan_row(&ctx, &project).await;
            assert_eq!(row["state"], "active");
            assert_eq!(row["plan"], "business");
            assert_eq!(row["business_step"], 25);
            assert_eq!(row["paddle_subscription_id"], "sub_new");
            assert_eq!(row["paddle_customer_id"], "ctm_new");
            assert!(row["read_only_cause"].is_null());
            assert!(row["read_only_since"].is_null());
        })
        .await;
    }
}

#[tokio::test]
async fn it_unpaid_read_only_billing_keeps_existing_behavior() {
    for cause in [ReadOnlyCause::TrialEnded, ReadOnlyCause::AwaitingPayment] {
        for subscribed in [false, true] {
            run_hosted_with_paddle(move |ctx, paddle| async move {
                let (token, project) = group(&ctx, "unpaid").await;
                let mut record = PlanRecord::awaiting_payment(now());
                record.read_only_cause = Some(cause);
                ctx.hub
                    .hub
                    .set_plan(topics(&project).tenant(), &record)
                    .await
                    .unwrap();
                if subscribed {
                    seed_subscription(&ctx, &project).await;
                }
                let before = plan_row(&ctx, &project).await;
                let (status, body) = call(
                    &ctx,
                    &token,
                    reqwest::Method::POST,
                    &format!("/v1/tenants/{}/portal", project.tenant),
                    None,
                )
                .await;
                if subscribed {
                    assert_eq!(status, StatusCode::OK, "{body}");
                    assert_eq!(
                        body,
                        json!({"portal_url": format!("{}portal/ctm_test", paddle.link_base())})
                    );
                } else {
                    assert_eq!(status, StatusCode::CONFLICT, "{body}");
                    assert_eq!(body["error"], "no_subscription");
                    assert_eq!(
                        body["error_description"],
                        "this group has no subscription yet: agmen group seats add 1 starts one"
                    );
                    assert_no_paddle(&paddle);
                }
                let (status, body) =
                    checkout_plan(&ctx, &token, &project, json!({"plan": "team"})).await;
                if subscribed {
                    assert_eq!(status, StatusCode::CONFLICT, "{body}");
                    assert_eq!(body["error"], "no_subscription");
                    assert_eq!(
                        body["error_description"],
                        "the subscription is not active yet; try again in a minute"
                    );
                    assert_eq!(paddle.requests().len(), 1);
                    assert_eq!(
                        paddle.requests()[0].1,
                        "/customers/ctm_test/portal-sessions"
                    );
                } else {
                    assert_eq!(status, StatusCode::OK, "{body}");
                    assert_eq!(body, json!({"checkout_url": checkout_url(&paddle)}));
                    assert_eq!(paddle.requests().len(), 1);
                    assert_eq!(paddle.requests()[0].1, "/transactions");
                }
                assert_eq!(plan_row(&ctx, &project).await, before);
            })
            .await;
        }
    }
}

#[tokio::test]
async fn it_payment_failed_checkout_counts_each_paddle_call() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = ended_group(
            &ctx,
            ReadOnlyCause::PaymentFailed,
            Plan::Team { extra_seats: 0 },
        )
        .await;
        let before = plan_row(&ctx, &project).await;
        let path = format!("/v1/tenants/{}/plan", project.tenant);
        // Leave one token: cancellation can run, but the checkout cannot.
        for _ in 0..5 {
            paddle.fail_next(503, json!({"error": {"code": "unavailable"}}));
            let (status, body) =
                checkout_plan(&ctx, &token, &project, json!({"plan": "team"})).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        }
        let (status, body, retry) = send_for_retry(
            &ctx,
            &token,
            reqwest::Method::POST,
            &path,
            Some(json!({"plan": "team"})),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(body["error"], "rate_limited");
        assert_retry_within_a_refill(retry.as_deref());
        let requests = paddle.requests();
        assert_eq!(requests.len(), 6);
        assert!(requests.iter().all(|(method, path, body)| {
            method == reqwest::Method::POST
                && path == "/subscriptions/sub_test/cancel"
                && body == &json!({"effective_from": "immediately"})
        }));
        assert_eq!(plan_row(&ctx, &project).await, before);
    })
    .await;
}

#[tokio::test]
async fn it_seat_decrease_racing_a_join_is_refused() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 1 }).await;
        seed_subscription(&ctx, &project).await;
        for (sub, handle) in [(51_u64, "ada"), (52, "bea"), (53, "cy")] {
            member_with_token(&ctx, &project, sub, handle, "member").await;
        }
        let raced = hold_plan_and_join(&ctx, &paddle, &token, &project, Race::JOIN_ONLY).await;
        assert_eq!(raced.0, StatusCode::CONFLICT, "{}", raced.1);
        assert_eq!(raced.1["error"], "seats_below_used");
        assert_eq!(
            raced.1["error_description"],
            "a member joined while seats were changing"
        );
        assert_eq!(plan_row(&ctx, &project).await["extra_seats"], 1);
        let patches = seat_patches(&paddle);
        assert_eq!(patches.len(), 2, "{patches:?}");
        assert_eq!(
            patches[0],
            json!([{"price_id": "pri_team_base", "quantity": 1}])
        );
        assert_eq!(patches[1], team_plus_one_seat());
        assert!(alerts(&ctx, &project).await.is_empty());
    })
    .await;
}

#[tokio::test]
async fn it_seat_revert_failure_stores_paddles_count_and_alerts_the_owner() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 1 }).await;
        seed_subscription(&ctx, &project).await;
        for (sub, handle) in [(51_u64, "ada"), (52, "bea"), (53, "cy")] {
            member_with_token(&ctx, &project, sub, handle, "member").await;
        }
        let race = Race {
            revert_fails: true,
            ..Race::JOIN_ONLY
        };
        let raced = hold_plan_and_join(&ctx, &paddle, &token, &project, race).await;
        assert_eq!(raced.0, StatusCode::CONFLICT, "{}", raced.1);
        assert_eq!(raced.1["error"], "seats_below_used");
        assert_eq!(
            raced.1["error_description"],
            "paid seats are below the members in the group, and Paddle could not be restored"
        );
        // Paddle kept the lower count, so that is what the hub stores.
        assert_eq!(plan_row(&ctx, &project).await["extra_seats"], 0);
        assert_eq!(seat_patches(&paddle).len(), 2);
        let sent = alerts(&ctx, &project).await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        let shown = me(&ctx, &token).await;
        assert!(
            sent[0]
                .mentioned
                .contains(&shown["id"].as_str().unwrap().parse().unwrap()),
            "{:?}",
            sent[0].mentioned
        );
        let text = sent[0].client.body.as_str();
        assert!(text.contains("could not be restored"), "{text}");
    })
    .await;
}

#[tokio::test]
async fn it_seat_revert_puts_back_the_items_from_before_the_change() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (token, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Team { extra_seats: 1 }).await;
        seed_subscription(&ctx, &project).await;
        for (sub, handle) in [(51_u64, "ada"), (52, "bea"), (53, "cy")] {
            member_with_token(&ctx, &project, sub, handle, "member").await;
        }
        // Paddle's event for the lowered count lands first, so the stored plan is
        // already the lowered one when the seat change gets to store.
        let race = Race {
            webhook_first: true,
            ..Race::JOIN_ONLY
        };
        let raced = hold_plan_and_join(&ctx, &paddle, &token, &project, race).await;
        assert_eq!(raced.0, StatusCode::CONFLICT, "{}", raced.1);
        assert_eq!(raced.1["error"], "seats_below_used");
        assert_eq!(
            raced.1["error_description"],
            "a member joined while seats were changing"
        );
        let patches = seat_patches(&paddle);
        assert_eq!(patches.len(), 2, "{patches:?}");
        // The second call restores what the group had before this request, not
        // what the plan row said when the request got the lock.
        assert_eq!(patches[1], team_plus_one_seat());
        assert!(alerts(&ctx, &project).await.is_empty());
    })
    .await;
}

async fn subscribed_team(ctx: &Ctx, extra_seats: u32) -> (String, Project) {
    let (token, project) = group(ctx, "acme").await;
    set_active(ctx, &project, Plan::Team { extra_seats }).await;
    seed_subscription(ctx, &project).await;
    (token, project)
}

async fn group_admin(ctx: &Ctx, project: &Project) -> String {
    member_with_token(ctx, project, 77, "blair", "admin").await
}

fn assert_no_owner_alert(sent: &[StampedEnvelope]) {
    assert!(sent.is_empty(), "{sent:?}");
}

/// Makes every ALERT insert in this test's database fail, and counts each
/// attempt. `run_hosted_with_paddle` gives each test a database of its own
/// and drops it at the end, so the trigger, function and sequence cannot
/// reach another test.
///
/// `nextval` is not rolled back with the insert, so the count stays when
/// the hub's post transaction aborts.
async fn block_owner_alerts(ctx: &Ctx) {
    for sql in [
        "CREATE SEQUENCE alert_post_attempts",
        "CREATE FUNCTION reject_alert() RETURNS trigger LANGUAGE plpgsql AS $fn$
         BEGIN
             PERFORM nextval('alert_post_attempts');
             RAISE EXCEPTION 'alert post blocked';
         END
         $fn$",
        "CREATE TRIGGER reject_alert BEFORE INSERT ON messages
         FOR EACH ROW WHEN (NEW.envelope->>'verb' = 'ALERT')
         EXECUTE FUNCTION reject_alert()",
    ] {
        sqlx::raw_sql(sql).execute(&ctx.pool).await.unwrap();
    }
}

async fn alert_post_attempts(ctx: &Ctx) -> i64 {
    sqlx::query_scalar(
        "SELECT CASE WHEN is_called THEN last_value ELSE 0 END FROM alert_post_attempts",
    )
    .fetch_one(&ctx.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn it_admin_seat_raise_alerts_the_owner() {
    run_hosted_with_paddle(|ctx, _paddle| async move {
        let (owner, project) = subscribed_team(&ctx, 0).await;
        let admin = group_admin(&ctx, &project).await;
        let (status, body) = put_seats(&ctx, &admin, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": "team", "seats_paid": 6}));
        let sent = alerts(&ctx, &project).await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        let shown = me(&ctx, &owner).await;
        assert!(
            sent[0]
                .mentioned
                .contains(&shown["id"].as_str().unwrap().parse().unwrap()),
            "{:?}",
            sent[0].mentioned
        );
        assert_eq!(
            sent[0].client.body.as_str(),
            "blair raised andy/acme from 4 to 6 paid seats (team). Paddle bills the change to your card."
        );
    })
    .await;
}

#[tokio::test]
async fn it_admin_business_seat_raise_names_the_new_step() {
    run_hosted_with_paddle(|ctx, _paddle| async move {
        let (_owner, project) = group(&ctx, "acme").await;
        set_active(&ctx, &project, Plan::Business(BusinessStep::Seats10)).await;
        seed_subscription(&ctx, &project).await;
        let admin = group_admin(&ctx, &project).await;
        let (status, body) = put_seats(&ctx, &admin, &project.tenant, 11).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": "business-25", "seats_paid": 25}));
        let sent = alerts(&ctx, &project).await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(
            sent[0].client.body.as_str(),
            "blair raised andy/acme from 10 to 25 paid seats (business-25). Paddle bills the change to your card."
        );
    })
    .await;
}

#[tokio::test]
async fn it_owner_seat_raise_does_not_alert() {
    run_hosted_with_paddle(|ctx, _paddle| async move {
        let (owner, project) = subscribed_team(&ctx, 0).await;
        let (status, body) = put_seats(&ctx, &owner, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": "team", "seats_paid": 6}));
        assert_no_owner_alert(&alerts(&ctx, &project).await);
    })
    .await;
}

#[tokio::test]
async fn it_admin_seat_lower_does_not_alert() {
    run_hosted_with_paddle(|ctx, _paddle| async move {
        let (_owner, project) = subscribed_team(&ctx, 2).await;
        let admin = group_admin(&ctx, &project).await;
        let (status, body) = put_seats(&ctx, &admin, &project.tenant, 4).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": "team", "seats_paid": 4}));
        assert_no_owner_alert(&alerts(&ctx, &project).await);
    })
    .await;
}

#[tokio::test]
async fn it_refused_admin_seat_change_does_not_alert() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (owner, project) = subscribed_team(&ctx, 0).await;
        let admin = group_admin(&ctx, &project).await;
        paddle.fail_next(400, json!({"error": {"code": "bad_request"}}));
        let (status, body) = put_seats(&ctx, &admin, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
        assert_eq!(body["error"], "upstream_unavailable");
        assert_eq!(get_seats(&ctx, &owner, &project.tenant).await["paid"], 4);
        assert_no_owner_alert(&alerts(&ctx, &project).await);

        // Five humans, and the paid count is still 4, so 4 is below the members.
        for (sub, handle) in [(78_u64, "ada"), (79, "bea"), (80, "cy")] {
            member_with_token(&ctx, &project, sub, handle, "member").await;
        }
        let (status, body) = put_seats(&ctx, &admin, &project.tenant, 4).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "seats_in_use");
        assert_eq!(body["error_description"], "remove members first");
        assert_eq!(get_seats(&ctx, &owner, &project.tenant).await["paid"], 4);
        assert_no_owner_alert(&alerts(&ctx, &project).await);
    })
    .await;
}

#[tokio::test]
async fn it_admin_seat_raise_to_checkout_does_not_alert() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        // A trial has no subscription, so more than 4 seats starts a checkout.
        let (_owner, project) = group(&ctx, "acme").await;
        let admin = group_admin(&ctx, &project).await;
        let (status, body) = put_seats(&ctx, &admin, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"checkout_url": checkout_url(&paddle)}));
        assert_no_owner_alert(&alerts(&ctx, &project).await);
    })
    .await;
}

#[tokio::test]
async fn it_admin_seat_raise_alert_failure_still_answers() {
    run_hosted_with_paddle(|ctx, _paddle| async move {
        let (_owner, project) = subscribed_team(&ctx, 0).await;
        let admin = group_admin(&ctx, &project).await;
        block_owner_alerts(&ctx).await;
        let (status, body) = put_seats(&ctx, &admin, &project.tenant, 6).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"plan": "team", "seats_paid": 6}));
        assert_eq!(get_seats(&ctx, &admin, &project.tenant).await["paid"], 6);
        assert_eq!(alert_post_attempts(&ctx).await, 1);
        assert_no_owner_alert(&alerts(&ctx, &project).await);
    })
    .await;
}

/// Locks the plan row until the returned transaction ends, so a seat change
/// that reaches Paddle waits there to store.
async fn lock_plan_row(ctx: &Ctx, project: &Project) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut tx = ctx.pool.begin().await.unwrap();
    let locked: Option<String> =
        sqlx::query_scalar("SELECT tenant_id FROM tenant_plans WHERE tenant_id = $1 FOR UPDATE")
            .bind(&project.tenant)
            .fetch_optional(&mut *tx)
            .await
            .unwrap();
    assert!(locked.is_some());
    tx
}

/// Sends `PUT /v1/tenants/{tenant}/seats` from a task of its own.
fn spawn_put_seats(
    ctx: &Ctx,
    token: &str,
    project: &Project,
    paid: u32,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let http = ctx.http.clone();
    let url = ctx
        .hub
        .url(&format!("/v1/tenants/{}/seats", project.tenant));
    let token = token.to_owned();
    tokio::spawn(async move {
        let response = http
            .put(url)
            .bearer_auth(token)
            .json(&json!({"paid": paid}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        (
            status,
            response.json::<Value>().await.unwrap_or(Value::Null),
        )
    })
}

/// Waits up to five seconds until Paddle has seen `count` seat changes.
async fn wait_for_seat_patches(paddle: &FakePaddle, count: usize) {
    for _ in 0..100 {
        if seat_patches(paddle).len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        seat_patches(paddle).len() >= count,
        "Paddle did not see {count} seat changes: {:?}",
        paddle.requests()
    );
}

async fn finished(put: tokio::task::JoinHandle<(StatusCode, Value)>) -> (StatusCode, Value) {
    tokio::time::timeout(Duration::from_secs(5), put)
        .await
        .expect("seat change stayed blocked on the plan row")
        .unwrap()
}

#[tokio::test]
async fn it_two_admin_seat_raises_in_flight_alert_the_owner_once() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_owner, project) = subscribed_team(&ctx, 0).await;
        let admin = group_admin(&ctx, &project).await;
        // Both requests read 4 paid seats and reach Paddle before either one
        // stores. Only the one that stores first raises the stored count.
        let tx = lock_plan_row(&ctx, &project).await;
        let puts = [
            spawn_put_seats(&ctx, &admin, &project, 6),
            spawn_put_seats(&ctx, &admin, &project, 6),
        ];
        wait_for_seat_patches(&paddle, 2).await;
        tx.commit().await.unwrap();
        for put in puts {
            let (status, body) = finished(put).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body, json!({"plan": "team", "seats_paid": 6}));
        }
        let sent = alerts(&ctx, &project).await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(
            sent[0].client.body.as_str(),
            "blair raised andy/acme from 4 to 6 paid seats (team). Paddle bills the change to your card."
        );
    })
    .await;
}

#[tokio::test]
async fn it_admin_seat_raise_refused_below_used_posts_no_raise_alert() {
    run_hosted_with_paddle(|ctx, paddle| async move {
        let (_owner, project) = subscribed_team(&ctx, 0).await;
        let admin = group_admin(&ctx, &project).await;
        for (sub, handle) in [(78_u64, "ada"), (79, "bea")] {
            member_with_token(&ctx, &project, sub, handle, "member").await;
        }
        // Four humans fill the 4 paid seats. Two more join while the raise to
        // 5 waits to store, and Paddle then refuses to put the old items back.
        let joiners = [
            support::seed_human(&ctx.pool, 80, "cy").await,
            support::seed_human(&ctx.pool, 81, "dee").await,
        ];
        let mut tx = lock_plan_row(&ctx, &project).await;
        let put = spawn_put_seats(&ctx, &admin, &project, 5);
        wait_for_seat_patches(&paddle, 1).await;
        paddle.fail_next(500, json!({"error": {"code": "internal_error"}}));
        for joiner in &joiners {
            sqlx::query(
                "INSERT INTO tenant_members (tenant_id, human_id, role) VALUES ($1, $2, 'member')",
            )
            .bind(&project.tenant)
            .bind(joiner)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        let (status, body) = finished(put).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "seats_below_used");
        // Paddle kept the raised count, so the hub stores it, and the only
        // ALERT is the one for the failed restore.
        assert_eq!(plan_row(&ctx, &project).await["extra_seats"], 1);
        let sent = alerts(&ctx, &project).await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        let text = sent[0].client.body.as_str();
        assert!(text.contains("could not be restored"), "{text}");
        assert!(!text.contains("raised"), "{text}");
    })
    .await;
}

/// Team with one extra seat, as a Paddle request body lists it.
fn team_plus_one_seat() -> Value {
    json!([
        {"price_id": "pri_team_base", "quantity": 1},
        {"price_id": "pri_team_seat", "quantity": 1},
    ])
}

/// The `items` of each subscription change Paddle saw, oldest first.
fn seat_patches(paddle: &FakePaddle) -> Vec<Value> {
    paddle
        .requests()
        .into_iter()
        .filter(|(method, path, _)| method.as_str() == "PATCH" && path == "/subscriptions/sub_test")
        .map(|(_, _, body)| body["items"].clone())
        .collect()
}

/// What happens while a seat change waits on the plan row.
#[derive(Clone, Copy)]
struct Race {
    /// A Paddle event stores the lowered plan before the seat change does.
    webhook_first: bool,
    /// Paddle refuses the call that puts the previous items back.
    revert_fails: bool,
}

impl Race {
    /// A member joins and nothing else happens.
    const JOIN_ONLY: Self = Self {
        webhook_first: false,
        revert_fails: false,
    };
}

/// Holds the plan row, waits until the seat drop reaches Paddle, then adds a
/// member and lets the seat change go on.
async fn hold_plan_and_join(
    ctx: &Ctx,
    paddle: &FakePaddle,
    token: &str,
    project: &Project,
    race: Race,
) -> (StatusCode, Value) {
    let joiner = support::seed_human(&ctx.pool, 54, "rae").await;
    let mut tx = ctx.pool.begin().await.unwrap();
    let locked: Option<String> =
        sqlx::query_scalar("SELECT tenant_id FROM tenant_plans WHERE tenant_id = $1 FOR UPDATE")
            .bind(&project.tenant)
            .fetch_optional(&mut *tx)
            .await
            .unwrap();
    assert!(locked.is_some());
    let http = ctx.http.clone();
    let url = ctx
        .hub
        .url(&format!("/v1/tenants/{}/seats", project.tenant));
    let owner = token.to_owned();
    let put = tokio::spawn(async move {
        let response = http
            .put(url)
            .bearer_auth(owner)
            .json(&json!({"paid": 4}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        (status, body)
    });
    let mut saw = false;
    for _ in 0..100 {
        let hit = paddle.requests().iter().any(|(method, path, _)| {
            method.as_str() == "PATCH" && path == "/subscriptions/sub_test"
        });
        if hit {
            saw = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        saw,
        "the seat change did not reach Paddle: {:?}",
        paddle.requests()
    );
    if race.revert_fails {
        paddle.fail_next(500, json!({"error": {"code": "internal_error"}}));
    }
    if race.webhook_first {
        sqlx::query("UPDATE tenant_plans SET extra_seats = 0 WHERE tenant_id = $1")
            .bind(&project.tenant)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO tenant_members (tenant_id, human_id, role) VALUES ($1, $2, 'member')")
        .bind(&project.tenant)
        .bind(&joiner)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), put)
        .await
        .expect("seat change stayed blocked on the plan row")
        .unwrap()
}
