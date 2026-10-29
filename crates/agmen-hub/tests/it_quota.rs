//! Hosted write gates and atomic UTC daily quotas, using ingest in process.
#![cfg(test)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use agmen_hub::{
    HUB_IDENTITY, Plan, PlanRecord, PlanState, Role, SeededHuman, TenantRole, next_utc_midnight,
    seed_tenant_member,
};
use agmen_proto::{
    DeliveryReply, IdentityId, NackReason, Needs, Payload, StampedEnvelope, Stream, Timestamp,
    Topics, Urgency,
};
use jiff::SignedDuration;
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{Ctx, envelope, login, run, run_hosted, ulid};
use tokio::sync::Barrier;
use tokio::task::JoinSet;

struct Room {
    topics: Topics,
    owner: SeededHuman,
    agent: IdentityId,
}

async fn room(ctx: &Ctx) -> Room {
    let hub = &ctx.hub.hub;
    let topics = hub.seed_project().await.unwrap();
    let owner = hub.seed_human(&topics, Role::Admin).await.unwrap();
    let mut tx = ctx.pool.begin().await.unwrap();
    sqlx::query("DELETE FROM tenant_members WHERE tenant_id = $1 AND human_id = $2")
        .bind(topics.tenant().as_str())
        .bind(owner.id.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    seed_tenant_member(
        &mut tx,
        topics.tenant().as_str(),
        owner.id.as_str(),
        TenantRole::Owner,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    hub.set_plan(topics.tenant(), &PlanRecord::trial(Timestamp::now()))
        .await
        .unwrap();
    let (agent, _) = hub.seed_identity(&topics).await.unwrap();
    sqlx::query(
        "INSERT INTO items (project_id, key, title, created_by) VALUES ($1, 'B-1', 'work', $2)",
    )
    .bind(topics.project().as_str())
    .bind(agent.id.as_str())
    .execute(&ctx.pool)
    .await
    .unwrap();
    Room {
        topics,
        owner,
        agent: agent.id,
    }
}

fn fyi() -> Vec<u8> {
    envelope(&ulid(), "FYI", json!({}), json!({}))
}

fn claim() -> Vec<u8> {
    envelope(
        &ulid(),
        "CLAIM",
        json!({"item": "B-1"}),
        json!({"valid_until": (Timestamp::now() + Duration::from_secs(60)).to_string()}),
    )
}

async fn post(ctx: &Ctx, room: &Room, raw: &[u8]) -> DeliveryReply {
    ctx.hub
        .hub
        .ingest(&room.topics, &room.agent, raw)
        .await
        .unwrap()
}

fn assert_ack(reply: &DeliveryReply) {
    assert!(matches!(reply, DeliveryReply::Ack { .. }), "{reply:?}");
}

fn assert_final(reply: &DeliveryReply, expected: NackReason) {
    assert!(
        matches!(reply, DeliveryReply::Nack { reason, retryable: false, .. } if *reason == expected),
        "{reply:?}"
    );
}

async fn used_today(ctx: &Ctx, topics: &Topics) -> i32 {
    sqlx::query_scalar(
        "SELECT coalesce((SELECT messages FROM tenant_usage
         WHERE tenant_id = $1 AND day = (now() AT TIME ZONE 'UTC')::date), 0)",
    )
    .bind(topics.tenant().as_str())
    .fetch_one(&ctx.pool)
    .await
    .unwrap()
}

async fn usage_rows(ctx: &Ctx, topics: &Topics) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM tenant_usage WHERE tenant_id = $1")
        .bind(topics.tenant().as_str())
        .fetch_one(&ctx.pool)
        .await
        .unwrap()
}

async fn alerts(ctx: &Ctx, topics: &Topics) -> Vec<StampedEnvelope> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT envelope::text FROM messages WHERE project_id = $1 AND stream = 'feed'
         AND from_id = $2 AND envelope->>'verb' = 'ALERT' ORDER BY seq LIMIT 51",
    )
    .bind(topics.project().as_str())
    .bind(HUB_IDENTITY)
    .fetch_all(&ctx.pool)
    .await
    .unwrap();
    rows.iter()
        .map(|raw| serde_json::from_str(raw).unwrap())
        .collect()
}

#[tokio::test]
async fn it_trial_quota_5000() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), 0, 4_999)
            .await
            .unwrap();
        assert_ack(&post(&ctx, &room, &fyi()).await);
        let before = Timestamp::now();
        let reply = post(&ctx, &room, &fyi()).await;
        assert_final(&reply, NackReason::QuotaExceeded);
        let DeliveryReply::Nack {
            retry_after_ms: Some(ms),
            ..
        } = reply
        else {
            panic!("quota refusal must name midnight");
        };
        let until = next_utc_midnight(before).as_millisecond() - before.as_millisecond();
        assert!(
            ms.abs_diff(u64::try_from(until).unwrap()) <= 2_000,
            "{ms} vs {until}"
        );
        assert_eq!(used_today(&ctx, &room.topics).await, 5_000);
    })
    .await;
}

#[tokio::test]
async fn it_80_percent_alerts_owner_once() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        sqlx::query("UPDATE tenants SET name = 'andy/acme' WHERE id = $1")
            .bind(room.topics.tenant().as_str())
            .execute(&ctx.pool)
            .await
            .unwrap();
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), 0, 3_999)
            .await
            .unwrap();
        assert_ack(&post(&ctx, &room, &fyi()).await);
        let first = alerts(&ctx, &room.topics).await;
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].client.body.as_str(),
            "andy/acme used 80% of today's 5,000 messages; posts stop at 5,000 until 00:00 UTC."
        );
        assert!(
            first[0]
                .mentioned
                .contains(&room.owner.id.as_str().parse().unwrap())
        );
        assert_eq!(first[0].stream, Stream::Feed);
        let Payload::Alert {
            need,
            urgency,
            needs,
            ..
        } = &first[0].client.payload
        else {
            panic!("expected ALERT");
        };
        assert_eq!(need, first[0].client.body.as_str());
        assert_eq!(*urgency, Urgency::Normal);
        assert_eq!(*needs, Needs::Anyone);
        for _ in 0..2 {
            assert_ack(&post(&ctx, &room, &fyi()).await);
        }
        assert_eq!(alerts(&ctx, &room.topics).await.len(), 1);
        assert_eq!(used_today(&ctx, &room.topics).await, 4_002);
    })
    .await;
}

#[tokio::test]
async fn it_team_quota_counts_extra_seats() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        let mut plan = PlanRecord::trial(Timestamp::now());
        plan.state = PlanState::Active;
        plan.plan = Some(Plan::Team { extra_seats: 2 });
        ctx.hub
            .hub
            .set_plan(room.topics.tenant(), &plan)
            .await
            .unwrap();
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), 0, 11_999)
            .await
            .unwrap();
        assert_ack(&post(&ctx, &room, &fyi()).await);
        assert_final(&post(&ctx, &room, &fyi()).await, NackReason::QuotaExceeded);
        assert_eq!(used_today(&ctx, &room.topics).await, 12_000);
    })
    .await;
}

#[tokio::test]
async fn it_quota_resets_at_utc_midnight() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), -1, 5_000)
            .await
            .unwrap();
        assert_ack(&post(&ctx, &room, &fyi()).await);
        assert_eq!(used_today(&ctx, &room.topics).await, 1);
        assert_eq!(usage_rows(&ctx, &room.topics).await, 2);
    })
    .await;
}

#[tokio::test]
async fn it_quota_exempts_heartbeat_release_bye() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        assert_ack(&post(&ctx, &room, &claim()).await);
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), 0, 5_000)
            .await
            .unwrap();
        for (verb, data) in [
            ("HEARTBEAT", json!({"items": ["B-1"]})),
            ("RELEASE", json!({"item": "B-1"})),
            ("BYE", json!({})),
        ] {
            assert_ack(&post(&ctx, &room, &envelope(&ulid(), verb, data, json!({}))).await);
        }
        assert_eq!(used_today(&ctx, &room.topics).await, 5_000);
        assert!(alerts(&ctx, &room.topics).await.is_empty());
    })
    .await;
}

#[tokio::test]
async fn it_resend_after_quota_gets_ack() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), 0, 4_999)
            .await
            .unwrap();
        let raw = fyi();
        let ack = post(&ctx, &room, &raw).await;
        assert_ack(&ack);
        assert_eq!(post(&ctx, &room, &raw).await, ack);
        ctx.hub
            .hub
            .set_plan(
                room.topics.tenant(),
                &PlanRecord::awaiting_payment(Timestamp::now()),
            )
            .await
            .unwrap();
        assert_eq!(post(&ctx, &room, &raw).await, ack);
        assert_eq!(used_today(&ctx, &room.topics).await, 5_000);
    })
    .await;
}

#[tokio::test]
async fn it_read_only_refuses_posts_claims_invites() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        assert_ack(&post(&ctx, &room, &claim()).await);
        ctx.hub
            .hub
            .set_plan(
                room.topics.tenant(),
                &PlanRecord::awaiting_payment(Timestamp::now()),
            )
            .await
            .unwrap();
        for raw in [
            fyi(),
            claim(),
            envelope(&ulid(), "HEARTBEAT", json!({"items": ["B-1"]}), json!({})),
        ] {
            assert_final(&post(&ctx, &room, &raw).await, NackReason::ReadOnly);
        }
        for (verb, data) in [("RELEASE", json!({"item": "B-1"})), ("BYE", json!({}))] {
            assert_ack(&post(&ctx, &room, &envelope(&ulid(), verb, data, json!({}))).await);
        }
        let lanes = format!("/v1/projects/{}/lanes", room.topics.project());
        for (method, path, body) in [
            (
                reqwest::Method::POST,
                "/v1/invites",
                json!({"project": room.topics.project(), "role": "member"}),
            ),
            (reqwest::Method::PUT, lanes.as_str(), json!({"lanes": []})),
            (
                reqwest::Method::POST,
                "/v1/projects",
                json!({"name": "blocked", "tenant": room.topics.tenant()}),
            ),
        ] {
            let response = ctx
                .http
                .request(method, ctx.hub.url(path))
                .bearer_auth(&room.owner.access_token)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["error"], "group_read_only");
            assert_eq!(
                body["error_description"],
                "the group is read-only until its owner pays: agmen group billing"
            );
        }
        let response = ctx
            .http
            .get(ctx.hub.url(&format!(
                "/v1/projects/{}/streams/feed?after=0&limit=50",
                room.topics.project()
            )))
            .bearer_auth(&room.owner.access_token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    })
    .await;
}

async fn forty_messages_without_usage(ctx: &Ctx, topics: &Topics) {
    for _ in 0..2 {
        let (identity, _) = ctx.hub.hub.seed_identity(topics).await.unwrap();
        for _ in 0..20 {
            assert_ack(
                &ctx.hub
                    .hub
                    .ingest(topics, &identity.id, &fyi())
                    .await
                    .unwrap(),
            );
        }
    }
    assert_eq!(usage_rows(ctx, topics).await, 0);
}

#[tokio::test]
async fn it_self_host_has_no_quota() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        ctx.hub
            .hub
            .set_plan(
                room.topics.tenant(),
                &PlanRecord::awaiting_payment(Timestamp::now()),
            )
            .await
            .unwrap();
        forty_messages_without_usage(&ctx, &room.topics).await;
    })
    .await;
}

#[tokio::test]
async fn it_hosted_tenant_without_plan_row_is_open() {
    run_hosted(|ctx| async move {
        let topics = ctx.hub.hub.seed_project().await.unwrap();
        forty_messages_without_usage(&ctx, &topics).await;
    })
    .await;
}

#[tokio::test]
async fn it_expired_trial_is_read_only_before_the_clock_runs() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        let start = Timestamp::now()
            .checked_sub(SignedDuration::from_hours(8 * 24))
            .unwrap();
        ctx.hub
            .hub
            .set_plan(room.topics.tenant(), &PlanRecord::trial(start))
            .await
            .unwrap();
        assert_final(&post(&ctx, &room, &fyi()).await, NackReason::ReadOnly);
        let state: String =
            sqlx::query_scalar("SELECT state FROM tenant_plans WHERE tenant_id = $1")
                .bind(room.topics.tenant().as_str())
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(state, "trial", "the gate must not persist the tick");
        assert_eq!(usage_rows(&ctx, &room.topics).await, 0);
    })
    .await;
}

#[tokio::test]
async fn it_hub_posts_ignore_the_gate() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        ctx.hub
            .hub
            .set_plan(
                room.topics.tenant(),
                &PlanRecord::awaiting_payment(Timestamp::now()),
            )
            .await
            .unwrap();
        ctx.hub
            .hub
            .post_owner_alert(room.topics.tenant(), "billing\nowner action")
            .await
            .unwrap();
        let alerts = alerts(&ctx, &room.topics).await;
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].client.body.as_str(), "billing\nowner action");
        let Payload::Alert { need, .. } = &alerts[0].client.payload else {
            panic!("expected ALERT");
        };
        assert_eq!(need, "billing");
        assert!(
            alerts[0]
                .mentioned
                .contains(&room.owner.id.as_str().parse().unwrap())
        );
        assert_eq!(usage_rows(&ctx, &room.topics).await, 0);
    })
    .await;
}

#[tokio::test]
async fn it_quota_counts_only_recorded_messages() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), 0, 0)
            .await
            .unwrap();
        let credential = format!("{}{}", "ghp_", "aB3d".repeat(9));
        let leaky = envelope(&ulid(), "FYI", json!({}), json!({"body": credential}));
        assert_final(&post(&ctx, &room, &leaky).await, NackReason::SecretDetected);
        let big = envelope(
            &ulid(),
            "FYI",
            json!({}),
            json!({"body": "x".repeat(32_769)}),
        );
        assert_final(&post(&ctx, &room, &big).await, NackReason::BodyTooLarge);
        let lend = envelope(
            &ulid(),
            "LEND",
            json!({"lane": "L-API", "paths": ["src/**"], "to": room.agent,
                   "until": (Timestamp::now() + Duration::from_secs(60)).to_string()}),
            json!({}),
        );
        // This refusal occurs after the usage increment and message write.
        assert_final(&post(&ctx, &room, &lend).await, NackReason::Schema);
        assert_eq!(used_today(&ctx, &room.topics).await, 0);
        assert_ack(&post(&ctx, &room, &fyi()).await);
        assert_eq!(used_today(&ctx, &room.topics).await, 1);
    })
    .await;
}

#[tokio::test]
async fn it_quota_never_overshoots_under_concurrency() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), 0, 4_999)
            .await
            .unwrap();
        let mut tasks = JoinSet::new();
        let ready = Arc::new(Barrier::new(8));
        for _ in 0..8 {
            let (agent, _) = ctx.hub.hub.seed_identity(&room.topics).await.unwrap();
            let hub = ctx.hub.hub.clone();
            let topics = room.topics.clone();
            let ready = Arc::clone(&ready);
            tasks.spawn(async move {
                ready.wait().await;
                hub.ingest(&topics, &agent.id, &fyi()).await.unwrap()
            });
        }
        let mut acks = 0;
        for _ in 0..8 {
            match tokio::time::timeout(Duration::from_secs(20), tasks.join_next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
            {
                DeliveryReply::Ack { .. } => acks += 1,
                reply => assert_final(&reply, NackReason::QuotaExceeded),
            }
        }
        assert_eq!(acks, 1);
        assert_eq!(used_today(&ctx, &room.topics).await, 5_000);
        assert_eq!(alerts(&ctx, &room.topics).await.len(), 1);
    })
    .await;
}

#[tokio::test]
async fn it_second_group_still_gets_its_first_project() {
    run_hosted(|ctx| async move {
        let access = login(&ctx, 4242, "andy").await;
        for (name, state) in [("one", "trial"), ("two", "read_only")] {
            let response = ctx
                .http
                .post(ctx.hub.url("/v1/groups"))
                .bearer_auth(&access)
                .json(&json!({"name": name, "project": "web"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["state"], state);
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM projects WHERE tenant_id = $1 AND id = $2)",
            )
            .bind(body["tenant"].as_str().unwrap())
            .bind(body["project"].as_str().unwrap())
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
            assert!(exists);
        }
    })
    .await;
}

#[tokio::test]
async fn it_writable_plan_without_quota_is_read_only() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        let mut plan = PlanRecord::trial(Timestamp::now());
        plan.state = PlanState::Active;
        plan.plan = None;
        ctx.hub
            .hub
            .set_plan(room.topics.tenant(), &plan)
            .await
            .unwrap();
        assert_final(&post(&ctx, &room, &fyi()).await, NackReason::ReadOnly);
        assert_eq!(usage_rows(&ctx, &room.topics).await, 0);
    })
    .await;
}

#[tokio::test]
async fn it_owner_alert_without_owner_still_posts() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        sqlx::query("DELETE FROM tenant_members WHERE tenant_id = $1 AND role = 'owner'")
            .bind(room.topics.tenant().as_str())
            .execute(&ctx.pool)
            .await
            .unwrap();
        ctx.hub
            .hub
            .post_owner_alert(room.topics.tenant(), "billing")
            .await
            .unwrap();
        let alerts = alerts(&ctx, &room.topics).await;
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].mentioned.is_empty());
        assert_eq!(usage_rows(&ctx, &room.topics).await, 0);
    })
    .await;
}

#[tokio::test]
async fn it_owner_alert_stops_at_fifty_projects() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        for _ in 0..50 {
            let id = support::new_id();
            sqlx::query("INSERT INTO projects (id, tenant_id, name) VALUES ($1, $2, $1)")
                .bind(id)
                .bind(room.topics.tenant().as_str())
                .execute(&ctx.pool)
                .await
                .unwrap();
        }
        ctx.hub
            .hub
            .post_owner_alert(room.topics.tenant(), "billing")
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM messages m JOIN projects p ON p.id = m.project_id
             WHERE p.tenant_id = $1 AND m.stream = 'feed' AND m.from_id = $2
             AND m.envelope->>'verb' = 'ALERT'",
        )
        .bind(room.topics.tenant().as_str())
        .bind(HUB_IDENTITY)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(count, 50);
        assert_eq!(usage_rows(&ctx, &room.topics).await, 0);
    })
    .await;
}

#[tokio::test]
async fn it_exhausted_quota_allows_http_writes() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), 0, 5_000)
            .await
            .unwrap();
        let lanes = format!("/v1/projects/{}/lanes", room.topics.project());
        for (method, path, body, expected) in [
            (
                reqwest::Method::POST,
                "/v1/invites",
                json!({"project": room.topics.project(), "role": "member"}),
                StatusCode::CREATED,
            ),
            (
                reqwest::Method::PUT,
                lanes.as_str(),
                json!({"lanes": []}),
                StatusCode::OK,
            ),
            (
                reqwest::Method::POST,
                "/v1/projects",
                json!({"name": "allowed", "tenant": room.topics.tenant()}),
                StatusCode::CREATED,
            ),
        ] {
            let response = ctx
                .http
                .request(method, ctx.hub.url(path))
                .bearer_auth(&room.owner.access_token)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{path}");
        }
        assert_eq!(used_today(&ctx, &room.topics).await, 5_000);
    })
    .await;
}

#[tokio::test]
async fn it_concurrent_resend_at_the_limit_gets_ack() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), 0, 4_999)
            .await
            .unwrap();
        // Hold the usage row so both copies of one message pass the
        // "already recorded" check and then wait on the counter.
        let mut hold = ctx.pool.begin().await.unwrap();
        sqlx::query(
            "UPDATE tenant_usage SET messages = messages
             WHERE tenant_id = $1 AND day = (now() AT TIME ZONE 'UTC')::date",
        )
        .bind(room.topics.tenant().as_str())
        .execute(&mut *hold)
        .await
        .unwrap();
        let raw = Arc::new(fyi());
        let mut tasks = JoinSet::new();
        for _ in 0..2 {
            let hub = ctx.hub.hub.clone();
            let topics = room.topics.clone();
            let agent = room.agent.clone();
            let raw = Arc::clone(&raw);
            tasks.spawn(async move { hub.ingest(&topics, &agent, &raw).await.unwrap() });
        }
        let mut waiting = 0_i64;
        for _ in 0..100 {
            waiting = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity
                 WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
            if waiting >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(waiting, 2, "both copies should wait on the usage row");
        hold.commit().await.unwrap();
        let mut replies = Vec::new();
        for _ in 0..2 {
            replies.push(
                tokio::time::timeout(Duration::from_secs(20), tasks.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
            );
        }
        assert_ack(&replies[0]);
        assert_eq!(replies[0], replies[1]);
        assert_eq!(used_today(&ctx, &room.topics).await, 5_000);
    })
    .await;
}

async fn invite_code(ctx: &Ctx, room: &Room) -> String {
    let response = ctx
        .http
        .post(ctx.hub.url("/v1/invites"))
        .bearer_auth(&room.owner.access_token)
        .json(&json!({"project": room.topics.project(), "role": "member"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body: Value = response.json().await.unwrap();
    body["code"].as_str().unwrap().to_owned()
}

async fn accept(ctx: &Ctx, token: &str, code: &str) -> (StatusCode, Value) {
    let response = ctx
        .http
        .post(ctx.hub.url(&format!("/v1/invites/{code}/accept")))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn it_read_only_refuses_invite_accept() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        let used_code = invite_code(&ctx, &room).await;
        let open_code = invite_code(&ctx, &room).await;
        let first = login(&ctx, 7001, "first").await;
        let (status, body) = accept(&ctx, &first, &used_code).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        ctx.hub
            .hub
            .set_plan(
                room.topics.tenant(),
                &PlanRecord::awaiting_payment(Timestamp::now()),
            )
            .await
            .unwrap();
        // A dead invite still answers first.
        let second = login(&ctx, 7002, "second").await;
        let (status, body) = accept(&ctx, &second, &used_code).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "invite_used");
        // A live invite on a read-only group is refused as read-only, not as a full group.
        let (status, body) = accept(&ctx, &second, &open_code).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "group_read_only");
        assert_eq!(
            body["error_description"],
            "the group is read-only until its owner pays: agmen group billing"
        );
        let unused: i64 =
            sqlx::query_scalar("SELECT count(*) FROM invites WHERE code_hash IS NOT NULL AND project_id = $1 AND used_by IS NULL")
                .bind(room.topics.project().as_str())
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(unused, 1);
    })
    .await;
}

#[tokio::test]
async fn it_seats_report_used_today() {
    run_hosted(|ctx| async move {
        let room = room(&ctx).await;
        let seats = || async {
            let response = ctx
                .http
                .get(
                    ctx.hub
                        .url(&format!("/v1/tenants/{}/seats", room.topics.tenant())),
                )
                .bearer_auth(&room.owner.access_token)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            response.json::<Value>().await.unwrap()
        };
        assert_eq!(seats().await["used_today"].as_u64(), Some(0));
        // Another UTC day does not count.
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), -1, 999)
            .await
            .unwrap();
        assert_eq!(seats().await["used_today"].as_u64(), Some(0));
        ctx.hub
            .hub
            .set_usage(room.topics.tenant(), 0, 1_234)
            .await
            .unwrap();
        let body = seats().await;
        assert_eq!(body["used_today"].as_u64(), Some(1_234), "{body}");
        assert_eq!(body["quota"].as_u64(), Some(5_000), "{body}");
    })
    .await;
}
