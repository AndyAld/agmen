//! Retention rules, prefix deletion, replay and concurrent ingest on a
//! database of each test's own. Only the startup tests use the dev broker.
#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_hub::{BillingEvent, BusinessStep, Hub, Plan, PlanRecord, Role};
use agmen_proto::{DeliveryReply, IdentityId, Timestamp, Topics};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::{Ctx, DEV_KEY_1, TestHub, envelope, run, run_hosted, ulid};

const DAY: Duration = Duration::from_secs(86_400);

async fn project(hub: &Hub, plan: Plan) -> (Topics, IdentityId, String) {
    let topics = hub.seed_project().await.unwrap();
    let record =
        PlanRecord::trial(Timestamp::now()).apply(BillingEvent::Active(plan), Timestamp::now());
    hub.set_plan(topics.tenant(), &record).await.unwrap();
    let (identity, token) = hub.seed_identity(&topics).await.unwrap();
    (topics, identity.id, token)
}

async fn post(hub: &Hub, topics: &Topics, from: &IdentityId) -> u64 {
    let raw = envelope(&ulid(), "FYI", json!({}), json!({}));
    let reply = hub.ingest(topics, from, &raw).await.unwrap();
    match reply {
        DeliveryReply::Ack { seq, .. } => seq.0,
        other => panic!("expected ACK, got {other:?}"),
    }
}

async fn age(pool: &PgPool, topics: &Topics, stream: &str, seq: u64, at: Timestamp) {
    sqlx::query(
        "UPDATE messages SET hub_at = $4::text::timestamptz
         WHERE project_id = $1 AND stream = $2 AND seq = $3",
    )
    .bind(topics.project().as_str())
    .bind(stream)
    .bind(i64::try_from(seq).unwrap())
    .bind(at.to_string())
    .execute(pool)
    .await
    .unwrap();
}

async fn rows(pool: &PgPool, topics: &Topics, stream: &str) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT seq FROM messages WHERE project_id = $1 AND stream = $2 ORDER BY seq",
    )
    .bind(topics.project().as_str())
    .bind(stream)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn head(pool: &PgPool, topics: &Topics, stream: &str) -> (i64, i64) {
    sqlx::query_as("SELECT seq, pruned_through FROM stream_heads WHERE stream = $1")
        .bind(format!("{}/{stream}", topics.project()))
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn seed_old(pool: &PgPool, topics: &Topics, count: i64, now: Timestamp) {
    sqlx::query(
        "INSERT INTO messages
             (project_id, stream, seq, id, from_id, payload_hash, envelope, hub_at, ack_row)
         SELECT $1, 'feed', s, s::text, 'retention', '\\x00'::bytea, '{}'::jsonb,
                $3::text::timestamptz, true
         FROM generate_series(1, $2::bigint) s",
    )
    .bind(topics.project().as_str())
    .bind(count)
    .bind((now - 100 * DAY).to_string())
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO stream_heads (stream, seq) VALUES ($1, $2)")
        .bind(format!("{}/feed", topics.project()))
        .bind(count)
        .execute(pool)
        .await
        .unwrap();
}

async fn configured(ctx: &Ctx, days: Option<u32>) -> TestHub {
    TestHub::start_configured(&ctx.db_url, &ctx.github, DEV_KEY_1, &[], |config| {
        config.retention_days = days;
    })
    .await
}

async fn check_window(ctx: &Ctx, hub: &Hub, topics: &Topics, from: &IdentityId, days: u32) {
    let now = Timestamp::now();
    assert_eq!(post(hub, topics, from).await, 1);
    assert_eq!(post(hub, topics, from).await, 2);
    age(&ctx.pool, topics, "feed", 1, now - (days + 1) * DAY).await;
    age(&ctx.pool, topics, "feed", 2, now - (days - 1) * DAY).await;
    assert_eq!(hub.prune_at(now).await.unwrap(), 1);
    assert_eq!(rows(&ctx.pool, topics, "feed").await, [2]);
    assert_eq!(head(&ctx.pool, topics, "feed").await, (2, 1));
}

#[tokio::test]
async fn it_team_keeps_90_days() {
    run_hosted(|ctx| async move {
        let (topics, from, _) = project(&ctx.hub.hub, Plan::Team { extra_seats: 0 }).await;
        check_window(&ctx, &ctx.hub.hub, &topics, &from, 90).await;
    })
    .await;
}

#[tokio::test]
async fn it_business_keeps_1_year() {
    run_hosted(|ctx| async move {
        let (topics, from, _) = project(&ctx.hub.hub, Plan::Business(BusinessStep::Seats10)).await;
        check_window(&ctx, &ctx.hub.hub, &topics, &from, 365).await;
    })
    .await;
}

#[tokio::test]
async fn it_tenant_without_plan_row_is_not_pruned() {
    run_hosted(|ctx| async move {
        let topics = ctx.hub.hub.seed_project().await.unwrap();
        let (identity, _) = ctx.hub.hub.seed_identity(&topics).await.unwrap();
        post(&ctx.hub.hub, &topics, &identity.id).await;
        let now = Timestamp::now();
        age(&ctx.pool, &topics, "feed", 1, now - 3650 * DAY).await;
        assert_eq!(ctx.hub.hub.prune_at(now).await.unwrap(), 0);
        assert_eq!(rows(&ctx.pool, &topics, "feed").await, [1]);
        assert_eq!(head(&ctx.pool, &topics, "feed").await, (1, 0));
    })
    .await;
}

#[tokio::test]
async fn it_self_host_keeps_everything() {
    run(|ctx| async move {
        let topics = ctx.hub.hub.seed_project().await.unwrap();
        let (identity, _) = ctx.hub.hub.seed_identity(&topics).await.unwrap();
        post(&ctx.hub.hub, &topics, &identity.id).await;
        let now = Timestamp::now();
        age(&ctx.pool, &topics, "feed", 1, now - 3650 * DAY).await;
        assert_eq!(ctx.hub.hub.prune_at(now).await.unwrap(), 0);
        assert_eq!(rows(&ctx.pool, &topics, "feed").await, [1]);
        assert_eq!(head(&ctx.pool, &topics, "feed").await, (1, 0));
    })
    .await;
}

#[tokio::test]
async fn it_self_host_limit_applies() {
    run(|ctx| async move {
        let hub = configured(&ctx, Some(30)).await;
        let topics = hub.hub.seed_project().await.unwrap();
        let (identity, _) = hub.hub.seed_identity(&topics).await.unwrap();
        check_window(&ctx, &hub.hub, &topics, &identity.id, 30).await;
    })
    .await;
}

#[tokio::test]
async fn it_self_host_limit_runs_on_start() {
    run(|ctx| async move {
        let hub = configured(&ctx, Some(30)).await;
        let topics = hub.hub.seed_project().await.unwrap();
        let (identity, _) = hub.hub.seed_identity(&topics).await.unwrap();
        post(&hub.hub, &topics, &identity.id).await;
        age(&ctx.pool, &topics, "feed", 1, Timestamp::now() - 31 * DAY).await;
        // Empty the outbox so this startup test leaves no retained broker state.
        sqlx::query("DELETE FROM publish_outbox")
            .execute(&ctx.pool)
            .await
            .unwrap();
        let messaging = hub.start_messaging().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            for _ in 0..250 {
                if rows(&ctx.pool, &topics, "feed").await.is_empty() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            panic!("retention did not run on startup");
        })
        .await
        .expect("retention did not delete the old message within 5 s");
        assert_eq!(head(&ctx.pool, &topics, "feed").await, (1, 1));
        drop(messaging);
    })
    .await;
}

#[tokio::test]
async fn it_self_host_no_limit_prunes_nothing() {
    run(|ctx| async move {
        let hub = configured(&ctx, None).await;
        let topics = hub.hub.seed_project().await.unwrap();
        let (identity, _) = hub.hub.seed_identity(&topics).await.unwrap();
        post(&hub.hub, &topics, &identity.id).await;
        age(&ctx.pool, &topics, "feed", 1, Timestamp::now() - 3650 * DAY).await;
        sqlx::query("DELETE FROM publish_outbox")
            .execute(&ctx.pool)
            .await
            .unwrap();
        let messaging = hub.start_messaging().await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(rows(&ctx.pool, &topics, "feed").await, [1]);
        assert_eq!(head(&ctx.pool, &topics, "feed").await, (1, 0));
        drop(messaging);
    })
    .await;
}

#[tokio::test]
async fn it_prune_deletes_a_prefix_only() {
    run_hosted(|ctx| async move {
        let (topics, from, _) = project(&ctx.hub.hub, Plan::Team { extra_seats: 0 }).await;
        let now = Timestamp::now();
        for seq in 1..=10 {
            assert_eq!(post(&ctx.hub.hub, &topics, &from).await, seq);
            if seq <= 5 || seq == 7 {
                age(&ctx.pool, &topics, "feed", seq, now - 100 * DAY).await;
            }
        }
        assert_eq!(ctx.hub.hub.prune_at(now).await.unwrap(), 7);
        assert_eq!(rows(&ctx.pool, &topics, "feed").await, [8, 9, 10]);
        assert_eq!(head(&ctx.pool, &topics, "feed").await, (10, 7));
        assert_eq!(ctx.hub.hub.prune_at(now).await.unwrap(), 0);
        assert_eq!(head(&ctx.pool, &topics, "feed").await, (10, 7));
    })
    .await;
}

#[tokio::test]
async fn it_prune_is_batched() {
    run_hosted(|ctx| async move {
        let (topics, _, _) = project(&ctx.hub.hub, Plan::Team { extra_seats: 0 }).await;
        let now = Timestamp::now();
        seed_old(&ctx.pool, &topics, 12_000, now).await;
        assert_eq!(ctx.hub.hub.prune_at(now).await.unwrap(), 12_000);
        assert!(rows(&ctx.pool, &topics, "feed").await.is_empty());
        assert_eq!(head(&ctx.pool, &topics, "feed").await, (12_000, 12_000));
    })
    .await;
}

#[tokio::test]
async fn it_prune_leaves_other_streams_and_tenants() {
    run_hosted(|ctx| async move {
        let (short, from, _) = project(&ctx.hub.hub, Plan::Team { extra_seats: 0 }).await;
        let (long, other, _) = project(&ctx.hub.hub, Plan::Business(BusinessStep::Seats10)).await;
        let now = Timestamp::now();
        post(&ctx.hub.hub, &short, &from).await;
        age(&ctx.pool, &short, "feed", 1, now - 100 * DAY).await;
        post(&ctx.hub.hub, &long, &other).await;
        age(&ctx.pool, &long, "feed", 1, now - 100 * DAY).await;
        // A second project in the short-retention tenant stays fresh.
        let second = support::new_id();
        sqlx::query("INSERT INTO projects (id, tenant_id, name) VALUES ($1, $2, $1)")
            .bind(&second)
            .bind(short.tenant().as_str())
            .execute(&ctx.pool)
            .await
            .unwrap();
        let second = Topics::new(short.tenant().clone(), second.parse().unwrap());
        let (fresh, _) = ctx.hub.hub.seed_identity(&second).await.unwrap();
        post(&ctx.hub.hub, &second, &fresh.id).await;
        let dm = envelope(&ulid(), "DM", json!({}), json!({"to": [from]}));
        let reply = ctx.hub.hub.ingest(&short, &from, &dm).await.unwrap();
        assert!(matches!(reply, DeliveryReply::Ack { .. }), "{reply:?}");
        let inbox = format!("inbox:{from}");
        assert_eq!(ctx.hub.hub.prune_at(now).await.unwrap(), 1);
        assert!(rows(&ctx.pool, &short, "feed").await.is_empty());
        for (topics, stream) in [(&short, inbox.as_str()), (&second, "feed"), (&long, "feed")] {
            assert_eq!(rows(&ctx.pool, topics, stream).await, [1]);
            assert_eq!(head(&ctx.pool, topics, stream).await, (1, 0));
        }
    })
    .await;
}

async fn prune_six(ctx: &Ctx) -> (Topics, IdentityId, String) {
    let (topics, from, token) = project(&ctx.hub.hub, Plan::Team { extra_seats: 0 }).await;
    let now = Timestamp::now();
    for seq in 1..=10 {
        assert_eq!(post(&ctx.hub.hub, &topics, &from).await, seq);
        if seq <= 6 {
            age(&ctx.pool, &topics, "feed", seq, now - 100 * DAY).await;
        }
    }
    assert_eq!(ctx.hub.hub.prune_at(now).await.unwrap(), 6);
    (topics, from, token)
}

#[tokio::test]
async fn it_next_seq_continues_after_prune() {
    run_hosted(|ctx| async move {
        let (topics, from, _) = prune_six(&ctx).await;
        assert_eq!(head(&ctx.pool, &topics, "feed").await, (10, 6));
        assert_eq!(post(&ctx.hub.hub, &topics, &from).await, 11);
        assert_eq!(rows(&ctx.pool, &topics, "feed").await, [7, 8, 9, 10, 11]);
        assert_eq!(head(&ctx.pool, &topics, "feed").await, (11, 6));
    })
    .await;
}

#[tokio::test]
async fn it_page_reports_pruned_through() {
    run_hosted(|ctx| async move {
        let (topics, _, token) = prune_six(&ctx).await;
        let response = ctx
            .http
            .get(ctx.hub.url(&format!(
                "/v1/projects/{}/streams/feed?after=0",
                topics.project()
            )))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let page: Value = response.json().await.unwrap();
        let seqs: Vec<_> = page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["seq"].as_u64().unwrap())
            .collect();
        assert_eq!(seqs, [7, 8, 9, 10]);
        assert_eq!(page["head"], 10);
        assert_eq!(page["pruned_through"], 6);
        assert!(page["next_after"].is_null());
        let (unpruned, from, token) = project(&ctx.hub.hub, Plan::Team { extra_seats: 0 }).await;
        post(&ctx.hub.hub, &unpruned, &from).await;
        let page: Value = ctx
            .http
            .get(
                ctx.hub
                    .url(&format!("/v1/projects/{}/streams/feed", unpruned.project())),
            )
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(page["pruned_through"], 0);
        // An entirely new stream has neither a head nor a pruning marker.
        let human = ctx
            .hub
            .hub
            .seed_human(&unpruned, Role::Member)
            .await
            .unwrap();
        let page: Value = ctx
            .http
            .get(ctx.hub.url(&format!(
                "/v1/projects/{}/streams/inbox:{}",
                unpruned.project(),
                human.id
            )))
            .bearer_auth(&human.access_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            page,
            json!({"messages": [], "next_after": null, "head": 0, "pruned_through": 0})
        );
    })
    .await;
}

/// The events route starts a new reader at the oldest kept message: the
/// ids the stream carries are the kept seqs, in order, then it stays open.
#[tokio::test]
async fn it_events_start_at_the_oldest_kept_message() {
    run_hosted(|ctx| async move {
        let (topics, _, _) = prune_six(&ctx).await;
        let human = ctx.hub.hub.seed_human(&topics, Role::Member).await.unwrap();
        let mut response = ctx
            .http
            .get(
                ctx.hub
                    .url(&format!("/v1/projects/{}/events", topics.project())),
            )
            .bearer_auth(&human.access_token)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let mut seen = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !seen.contains("id: feed:10") && !seen.contains("id:feed:10") {
            let chunk = tokio::time::timeout_at(deadline, response.chunk())
                .await
                .expect("no feed:10 event within 10 s")
                .unwrap()
                .expect("the events stream ended before feed:10");
            seen.push_str(std::str::from_utf8(&chunk).unwrap());
        }
        let ids: Vec<&str> = seen
            .lines()
            .filter_map(|line| line.strip_prefix("id:"))
            .map(str::trim)
            .collect();
        assert_eq!(ids, ["feed:7", "feed:8", "feed:9", "feed:10"]);
    })
    .await;
}

/// Down drops the marker column and the index; up restores both.
#[tokio::test]
async fn it_retention_migration_round_trips() {
    run(|ctx| async move {
        let step = |down: bool| {
            agmen_hub::MIGRATOR
                .iter()
                .find(|m| m.version == 23 && m.migration_type.is_down_migration() == down)
                .unwrap()
        };
        let present = |ctx: &Ctx| {
            let pool = ctx.pool.clone();
            async move {
                let column: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM information_schema.columns
                     WHERE table_name = 'stream_heads' AND column_name = 'pruned_through'",
                )
                .fetch_one(&pool)
                .await
                .unwrap();
                let index: Option<String> =
                    sqlx::query_scalar("SELECT to_regclass('messages_by_hub_at')::text")
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                (column, index.is_some())
            }
        };
        assert_eq!(present(&ctx).await, (1, true));
        sqlx::raw_sql(step(true).sql.clone())
            .execute(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(present(&ctx).await, (0, false));
        sqlx::raw_sql(step(false).sql.clone())
            .execute(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(present(&ctx).await, (1, true));
    })
    .await;
}

#[tokio::test]
async fn it_prune_runs_beside_ingest() {
    run_hosted(|ctx| async move {
        let (topics, from, _) = project(&ctx.hub.hub, Plan::Team { extra_seats: 0 }).await;
        let now = Timestamp::now();
        seed_old(&ctx.pool, &topics, 12_000, now).await;
        let hub = ctx.hub.hub.clone();
        tokio::time::timeout(Duration::from_secs(30), async {
            let pruning = tokio::spawn(async move { hub.prune_at(now).await.unwrap() });
            for seq in 12_001..=12_050 {
                assert_eq!(post(&ctx.hub.hub, &topics, &from).await, seq);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert_eq!(pruning.await.unwrap(), 12_000);
        })
        .await
        .expect("pruning and ingest did not finish within 30 s");
        let (last, pruned) = head(&ctx.pool, &topics, "feed").await;
        assert_eq!((last, pruned), (12_050, 12_000));
        assert_eq!(
            rows(&ctx.pool, &topics, "feed").await,
            (pruned + 1..=last).collect::<Vec<_>>()
        );
    })
    .await;
}

#[tokio::test]
async fn it_prune_locks_heads_in_stream_order() {
    run_hosted(|ctx| async move {
        let (topics, _, _) = project(&ctx.hub.hub, Plan::Team { extra_seats: 0 }).await;
        let now = Timestamp::now();
        let first = format!("{}/inbox:aaaaaaaa", topics.project());
        let second = format!("{}/inbox:bbbbbbbb", topics.project());
        // Physical order differs from ingest's order of locking streams.
        sqlx::query("INSERT INTO stream_heads (stream, seq) VALUES ($1, 1), ($2, 1)")
            .bind(&second)
            .bind(&first)
            .execute(&ctx.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO messages
                (project_id, stream, seq, id, from_id, payload_hash, envelope, hub_at, ack_row)
             SELECT $1, s, 1, s, 'retention', '\\x00'::bytea, '{}'::jsonb,
                    $2::text::timestamptz, false
             FROM UNNEST(ARRAY['inbox:aaaaaaaa', 'inbox:bbbbbbbb']) s",
        )
        .bind(topics.project().as_str())
        .bind((now - 100 * DAY).to_string())
        .execute(&ctx.pool)
        .await
        .unwrap();
        let mut earlier = ctx.pool.begin().await.unwrap();
        sqlx::query("UPDATE stream_heads SET seq = seq WHERE stream = $1")
            .bind(&first)
            .execute(&mut *earlier)
            .await
            .unwrap();
        let hub = ctx.hub.hub.clone();
        let pruning = tokio::spawn(async move { hub.prune_at(now).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(5), async {
            for _ in 0..250 {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                     WHERE datname = current_database() AND wait_event_type = 'Lock'
                       AND query LIKE '%stream_heads%' AND pid <> pg_backend_pid())",
                )
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
                if waiting {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            panic!("pruning did not reach the head lock");
        })
        .await
        .unwrap();
        // Pruning must wait on the earlier head without holding the later
        // one, so a multi-stream ingest can finish in its normal lock order.
        let mut later = ctx.pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '1s'")
            .execute(&mut *later)
            .await
            .unwrap();
        sqlx::query("UPDATE stream_heads SET seq = seq WHERE stream = $1")
            .bind(&second)
            .execute(&mut *later)
            .await
            .expect("pruning locked the later stream before the earlier one");
        later.commit().await.unwrap();
        earlier.commit().await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), pruning)
                .await
                .unwrap()
                .unwrap(),
            2
        );
    })
    .await;
}
