//! Ingest, numbering, the sweeper, and ACK and NACK, against an in-process
//! hub with its own broker connection, a database of the test's own on
//! `AGMEN_IT_PG`, and the dev EMQX (`AGMEN_IT_MQTT`).
//!
//! Identities publish over MQTT with broker tokens the hub minted; humans
//! have no broker access in the MVP, so their posts call `Hub::ingest`
//! directly, the path the web console's `POST /messages` will take.
//!
//! Each test runs in [`run_in_room`], which deletes the retained
//! `state/head` the sweeper published and the `state/roster` the presence
//! task published (minting an identity changes the roster) for the test's
//! project, so the dev broker keeps none of them.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use agmen_hub::{HUB_IDENTITY, HubPost};
use agmen_proto::{
    DeliveryReply, IdentityKind, NackReason, Needs, Payload, Seq, StampedEnvelope, Stream, Topics,
    Urgency,
};
use jiff::Timestamp;
use reqwest::StatusCode;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{PubAckReason, SubscribeReasonCode};
use serde_json::{Value, json};
use support::mqtt::{self, ACK_WAIT, Client, connect_ok};
use support::{
    Ctx, DEV_KEY_1, Minted, Project, TestHub, broker_token, clearing, count, emqx_api, env,
    envelope, gone_from_broker, login, mint_kind, mint_ok, run, seed_human, seed_member,
    seed_project, topics, ulid,
};

const ANDY: u64 = 4242;
const CAROL: u64 = 77;
/// How long a test waits to be sure nothing arrives.
const QUIET: Duration = Duration::from_secs(2);

fn fyi(id: &str) -> Vec<u8> {
    envelope(id, "FYI", json!({}), json!({}))
}

/// One identity's broker connection, subscribed to its `ack/`, its
/// `inbox/` and the feed.
#[derive(Debug)]
struct Agent {
    id: String,
    client: Client,
    topics: Topics,
}

impl Agent {
    async fn connect(ctx: &Ctx, project: &Project, minted: &Minted) -> Self {
        let (status, body) = broker_token(&ctx.http, &ctx.hub, &minted.id, &minted.token).await;
        assert_eq!(status, StatusCode::OK, "broker-token answered {body}");
        let token = body["token"].as_str().unwrap();
        let client_id = format!("{}-mcp", minted.id);
        let mut options = mqtt::options(&client_id, &minted.id, token);
        // The broker's cap (ADR-0002); the client's default is 10 KiB.
        options.set_max_packet_size(Some(96 * 1024));
        let mut client = connect_ok(options).await;
        let topics = topics(project);
        let id = minted.id.parse().unwrap();
        for filter in [topics.ack(&id), topics.inbox(&id), topics.feed()] {
            assert_eq!(
                client.subscribe(&filter).await,
                SubscribeReasonCode::Success(QoS::AtLeastOnce),
                "{filter}"
            );
        }
        Self {
            id: minted.id.clone(),
            client,
            topics,
        }
    }

    fn identity(&self) -> agmen_proto::IdentityId {
        self.id.parse().unwrap()
    }

    /// Publishes `raw` to its own `up/` without waiting for the reply.
    async fn send(&mut self, raw: &[u8]) {
        let up = self.topics.up(&self.identity());
        assert_eq!(self.client.publish(&up, raw).await, PubAckReason::Success);
    }

    /// The next reply on its `ack/`.
    async fn reply(&mut self) -> DeliveryReply {
        let ack = self.topics.ack(&self.identity());
        let publish = self
            .client
            .recv_on(&ack, ACK_WAIT)
            .await
            .unwrap_or_else(|| panic!("no reply on {ack} within {ACK_WAIT:?}"));
        serde_json::from_slice(&publish.payload).unwrap()
    }

    async fn post(&mut self, raw: &[u8]) -> DeliveryReply {
        self.send(raw).await;
        self.reply().await
    }

    /// The next message on the feed, or `None` within `within`.
    async fn feed(&mut self, within: Duration) -> Option<StampedEnvelope> {
        let feed = self.topics.feed();
        let publish = self.client.recv_on(&feed, within).await?;
        Some(serde_json::from_slice(&publish.payload).unwrap())
    }

    /// The next message on its inbox, or `None` within `within`.
    async fn inbox(&mut self, within: Duration) -> Option<StampedEnvelope> {
        let inbox = self.topics.inbox(&self.identity());
        let publish = self.client.recv_on(&inbox, within).await?;
        Some(serde_json::from_slice(&publish.payload).unwrap())
    }
}

/// A project where andy (an admin) owns agent sol and coordinator coord.
#[derive(Debug)]
struct Room {
    project: Project,
    andy: String,
    andy_token: String,
    sol: Minted,
    coord: Minted,
}

async fn room(ctx: &Ctx) -> Room {
    let project = seed_project(&ctx.pool).await;
    let andy = seed_human(&ctx.pool, ANDY, "andy").await;
    seed_member(&ctx.pool, &andy, &project, "admin").await;
    let andy_token = login(ctx, ANDY, "andy").await;
    let sol = mint_ok(ctx, &andy_token, "andy/sol", &project).await;
    let coord = mint_kind(ctx, &andy_token, "andy/coord", &project, "coordinator").await;
    Room {
        project,
        andy,
        andy_token,
        sol,
        coord,
    }
}

/// Runs `body` in a fresh [`Room`], then deletes the retained `state/head`
/// and `state/roster` the hub published for the room's project, also when
/// `body` panics.
///
/// `body` drops its `Messaging` when it ends, but the hub's last PUBLISH
/// may still be on its way to EMQX. Once EMQX no longer lists the hub's
/// client, it has handled everything that client sent, so the deletion
/// comes after the last `state/head`.
async fn run_in_room<F, Fut>(body: F)
where
    F: FnOnce(Arc<Ctx>, Room) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    run(|ctx| async move {
        let room = room(&ctx).await;
        let topics = topics(&room.project);
        let retained = vec![topics.state_head(), topics.state("roster")];
        clearing(ctx, retained, |ctx| async move {
            body(Arc::clone(&ctx), room).await;
            gone_from_broker(&ctx.http, &ctx.hub.mqtt.client_id()).await;
        })
        .await;
    })
    .await;
}

/// Posts `raw` as `from` without the broker, as a human's post will.
async fn ingest(ctx: &Ctx, project: &Project, from: &str, raw: &[u8]) -> DeliveryReply {
    ctx.hub
        .hub
        .ingest(&topics(project), &from.parse().unwrap(), raw)
        .await
        .unwrap()
}

fn assert_ack(reply: &DeliveryReply, stream: &Stream, seq: u64) {
    match reply {
        DeliveryReply::Ack {
            stream: s, seq: n, ..
        } => {
            assert_eq!((s, *n), (stream, Seq(seq)), "{reply:?}");
        }
        DeliveryReply::Nack { .. } => panic!("expected an ACK, got {reply:?}"),
    }
}

/// The id of the message `reply` answers.
fn reply_id(reply: &DeliveryReply) -> &str {
    match reply {
        DeliveryReply::Ack { id, .. } | DeliveryReply::Nack { id, .. } => id.as_str(),
    }
}

fn assert_final_nack(reply: &DeliveryReply, reason: NackReason) {
    match reply {
        DeliveryReply::Nack {
            retryable,
            reason: r,
            ..
        } => {
            assert_eq!((*r, *retryable), (reason, false), "{reply:?}");
        }
        DeliveryReply::Ack { .. } => panic!("expected NACK {reason}, got {reply:?}"),
    }
}

/// Messages stored from `from`, every copy counted.
async fn stored_from(ctx: &Ctx, from: &str) -> i64 {
    count(
        &ctx.pool,
        "SELECT count(*) FROM messages WHERE from_id = $1",
        from,
    )
    .await
}

/// Each copy of message `id`, by stream.
async fn copies(ctx: &Ctx, id: &str) -> Vec<(String, StampedEnvelope)> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT stream, envelope::text FROM messages WHERE id = $1 ORDER BY stream")
            .bind(id)
            .fetch_all(&ctx.pool)
            .await
            .unwrap();
    rows.into_iter()
        .map(|(stream, json)| (stream, serde_json::from_str(&json).unwrap()))
        .collect()
}

/// The streams message `id` was stored in.
async fn streams_of(ctx: &Ctx, id: &str) -> BTreeSet<String> {
    copies(ctx, id)
        .await
        .into_iter()
        .map(|(stream, _)| stream)
        .collect()
}

/// Rows still waiting in the outbox for `project`.
async fn outbox(ctx: &Ctx, project: &Project) -> i64 {
    count(
        &ctx.pool,
        "SELECT count(*) FROM publish_outbox WHERE project_id = $1",
        &project.project,
    )
    .await
}

fn idset<'a>(ids: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    ids.into_iter().map(str::to_owned).collect()
}

fn mentioned(env: &StampedEnvelope) -> BTreeSet<String> {
    env.mentioned.iter().map(|id| id.to_string()).collect()
}

#[tokio::test]
async fn it_fyi_gets_ack_and_feed() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        let id = ulid();
        let reply = sol.post(&fyi(&id)).await;
        assert_ack(&reply, &Stream::Feed, 1);
        let got = sol.feed(ACK_WAIT).await.expect("the FYI on the feed");
        assert_eq!(got.client.id.as_str(), id);
        assert_eq!(got.from.as_str(), room.sol.id);
        assert_eq!(got.from_handle, "andy/sol");
        assert_eq!(got.from_kind, IdentityKind::Agent);
        assert_eq!((got.stream, got.seq), (Stream::Feed, Seq(1)));
        assert!(got.mentioned.is_empty());

        // The second message is numbered next, and the outbox drains.
        assert_ack(&sol.post(&fyi(&ulid())).await, &Stream::Feed, 2);
        assert_eq!(sol.feed(ACK_WAIT).await.unwrap().seq, Seq(2));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(outbox(&ctx, &room.project).await, 0);
    })
    .await;
}

#[tokio::test]
async fn it_duplicate_gets_same_ack() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        let raw = fyi(&ulid());
        let first = sol.post(&raw).await;
        let second = sol.post(&raw).await;
        assert_ack(&first, &Stream::Feed, 1);
        assert_eq!(first, second);
        assert!(sol.feed(ACK_WAIT).await.is_some());
        assert!(
            sol.feed(QUIET).await.is_none(),
            "the resend reached the feed"
        );
        assert_eq!(stored_from(&ctx, &room.sol.id).await, 1);
    })
    .await;
}

#[tokio::test]
async fn it_conflicting_duplicate_nacks() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        let id = ulid();
        assert_ack(&sol.post(&fyi(&id)).await, &Stream::Feed, 1);
        let changed = envelope(&id, "FYI", json!({}), json!({ "body": "something else" }));
        assert_final_nack(&sol.post(&changed).await, NackReason::Conflict);
        assert_eq!(stored_from(&ctx, &room.sol.id).await, 1);
    })
    .await;
}

#[tokio::test]
async fn it_oversize_body_final_nack() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        let big = envelope(
            &ulid(),
            "FYI",
            json!({}),
            json!({ "body": "x".repeat(32_769) }),
        );
        assert_final_nack(&sol.post(&big).await, NackReason::BodyTooLarge);
        let at_limit = envelope(
            &ulid(),
            "FYI",
            json!({}),
            json!({ "body": "x".repeat(32_768) }),
        );
        assert_ack(&sol.post(&at_limit).await, &Stream::Feed, 1);
        assert_eq!(stored_from(&ctx, &room.sol.id).await, 1);
    })
    .await;
}

#[tokio::test]
async fn it_secret_shaped_body_nacks() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        // Built at runtime (ruling R22) so no secret-shaped literal is committed.
        let token = format!("{}{}", "ghp_", "aB3d".repeat(9));
        let body = format!("use {token} for the API");
        let leaky = envelope(&ulid(), "FYI", json!({}), json!({ "body": body }));
        assert_final_nack(&sol.post(&leaky).await, NackReason::SecretDetected);
        // Text in data is scanned too.
        let status = envelope(&ulid(), "STATUS", json!({ "summary": token }), json!({}));
        assert_final_nack(&sol.post(&status).await, NackReason::SecretDetected);
        assert_eq!(stored_from(&ctx, &room.sol.id).await, 0);
        assert!(sol.feed(QUIET).await.is_none());
    })
    .await;
}

#[tokio::test]
async fn it_rate_limit_retryable() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        // The limiter judges each message when the hub ingests it, at a
        // burst of 30 and 10 per second after. However slowly the hub works
        // through these, under 5 s it admits at most 30 + 10 * 5 = 80, so
        // at least one is limited without a clock assert.
        const SENT: i64 = 80;
        for _ in 0..SENT {
            sol.send(&fyi(&ulid())).await;
        }
        let mut acked = 0;
        let mut limited = 0;
        for _ in 0..SENT {
            match sol.reply().await {
                DeliveryReply::Ack { .. } => acked += 1,
                DeliveryReply::Nack {
                    retryable: true,
                    retry_after_ms: Some(ms),
                    reason: NackReason::RateLimited,
                    ..
                } if (1..=1_000).contains(&ms) => limited += 1,
                other => panic!("unexpected reply {other:?}"),
            }
        }
        assert!(limited >= 1, "nothing was rate limited");
        assert!(acked >= 30, "{acked} acked, {limited} limited");
        assert_eq!(acked + limited, SENT);
        assert_eq!(stored_from(&ctx, &room.sol.id).await, acked);
    })
    .await;
}

#[tokio::test]
async fn it_valid_until_uses_hub_clock() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        // The laptop's clock is 10 minutes fast; the hub judges valid_until
        // by its own clock with 120 s of skew allowed.
        let now = Timestamp::now();
        let skewed = |until: Timestamp| {
            envelope(
                &ulid(),
                "FYI",
                json!({}),
                json!({
                    "sent_at": (now + Duration::from_secs(600)).to_string(),
                    "valid_until": until.to_string(),
                }),
            )
        };
        let recent = skewed(now - Duration::from_secs(60));
        assert_ack(&sol.post(&recent).await, &Stream::Feed, 1);
        let stale = skewed(now - Duration::from_secs(180));
        assert_final_nack(&sol.post(&stale).await, NackReason::Expired);
        assert_eq!(stored_from(&ctx, &room.sol.id).await, 1);
    })
    .await;
}

#[tokio::test]
async fn it_dm_fan_out_and_privacy() {
    run_in_room(|ctx, room| async move {
        let b = mint_ok(&ctx, &room.andy_token, "andy/b", &room.project).await;
        let carol = seed_human(&ctx.pool, CAROL, "carol").await;
        seed_member(&ctx.pool, &carol, &room.project, "member").await;
        let carol_token = login(&ctx, CAROL, "carol").await;
        let c = mint_ok(&ctx, &carol_token, "carol/c", &room.project).await;
        let _messaging = ctx.hub.start_messaging().await;
        let mut a = Agent::connect(&ctx, &room.project, &room.sol).await;
        let mut b_client = Agent::connect(&ctx, &room.project, &b).await;
        let mut c_client = Agent::connect(&ctx, &room.project, &c).await;

        let id = ulid();
        let dm = envelope(&id, "DM", json!({}), json!({ "to": [b.id] }));
        let reply = a.post(&dm).await;
        // The ACK reports the sender's own copy (ruling R33a).
        assert_ack(&reply, &Stream::Inbox(a.identity()), 1);

        let to_b = b_client.inbox(ACK_WAIT).await.expect("B's copy");
        assert_eq!(to_b.client.id.as_str(), id);
        assert_eq!(
            (to_b.stream, to_b.seq),
            (Stream::Inbox(b_client.identity()), Seq(1))
        );
        let to_a = a.inbox(ACK_WAIT).await.expect("A's copy");
        assert_eq!(to_a.stream, Stream::Inbox(a.identity()));

        assert_eq!(
            streams_of(&ctx, &id).await,
            idset([
                format!("inbox:{}", room.sol.id).as_str(),
                format!("inbox:{}", b.id).as_str(),
                format!("inbox:{}", room.andy).as_str(),
            ])
        );
        assert!(c_client.inbox(QUIET).await.is_none(), "C got the DM");
        assert!(
            c_client.feed(Duration::ZERO).await.is_none(),
            "the DM reached the feed"
        );
        assert!(
            a.feed(Duration::ZERO).await.is_none(),
            "the DM reached the feed"
        );
    })
    .await;
}

#[tokio::test]
async fn it_non_human_forbidden_verbs() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        let parent = ulid();
        assert_ack(&sol.post(&fyi(&parent)).await, &Stream::Feed, 1);
        let reply_to = json!({ "reply_to": parent });
        for raw in [
            envelope(
                &ulid(),
                "RULING",
                json!({ "subject": "ship it" }),
                json!({}),
            ),
            envelope(&ulid(), "PUSH_OK", json!({}), reply_to.clone()),
            envelope(&ulid(), "PUSH_NO", json!({}), reply_to.clone()),
        ] {
            assert_final_nack(&sol.post(&raw).await, NackReason::ForbiddenVerb);
        }
        // No listener (dispatcher) identity can exist before the
        // roles-listener plan adds the kind; the verb table's listener rows
        // are unit tests in src/ingest_tests.rs. A human may not CLAIM.
        let claim = envelope(
            &ulid(),
            "CLAIM",
            json!({ "item": "B-1" }),
            json!({ "valid_until": (Timestamp::now() + Duration::from_secs(60)).to_string() }),
        );
        assert_final_nack(
            &ingest(&ctx, &room.project, &room.andy, &claim).await,
            NackReason::ForbiddenVerb,
        );
        // A human may rule.
        let ruling = envelope(
            &ulid(),
            "RULING",
            json!({ "subject": "ship it" }),
            json!({}),
        );
        assert_ack(
            &ingest(&ctx, &room.project, &room.andy, &ruling).await,
            &Stream::Feed,
            2,
        );
        assert_eq!(stored_from(&ctx, &room.sol.id).await, 1);
    })
    .await;
}

#[tokio::test]
async fn it_sweeper_resumes_after_crash() {
    run_in_room(|ctx, room| async move {
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        // ctx.hub never starts its sweeper, so this is a hub that died
        // after the commit and before publishing anything.
        let id = ulid();
        let reply = ingest(&ctx, &room.project, &room.sol.id, &fyi(&id)).await;
        assert_ack(&reply, &Stream::Feed, 1);
        assert_eq!(
            outbox(&ctx, &room.project).await,
            2,
            "the feed copy and the ACK wait in the outbox"
        );
        assert!(sol.feed(QUIET).await.is_none());

        // The restarted hub's sweeper publishes what the dead one left.
        let restarted = TestHub::start(&ctx.db_url, &ctx.github, DEV_KEY_1, &[]).await;
        let messaging = restarted.start_messaging().await;
        let got = sol.feed(ACK_WAIT).await.expect("the FYI after the restart");
        assert_eq!((got.client.id.as_str(), got.seq), (id.as_str(), Seq(1)));
        assert_eq!(sol.reply().await, reply);

        // The sweeper publishes state/head before it deletes the rows, so
        // once they are gone a new subscription gets the retained head.
        let deadline = std::time::Instant::now() + ACK_WAIT;
        while outbox(&ctx, &room.project).await > 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the outbox did not drain"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let head_topic = sol.topics.state_head();
        assert_eq!(
            sol.client.subscribe(&head_topic).await,
            SubscribeReasonCode::Success(QoS::AtLeastOnce)
        );
        let head = sol
            .client
            .recv_on(&head_topic, ACK_WAIT)
            .await
            .expect("retained state/head");
        assert!(head.retain);
        let head: Value = serde_json::from_slice(&head.payload).unwrap();
        assert_eq!(head["feed_seq"], 1, "{head}");

        // As run_in_room does for ctx.hub: the clearing comes after the
        // restarted hub's last PUBLISH.
        drop(messaging);
        gone_from_broker(&ctx.http, &restarted.mqtt.client_id()).await;
    })
    .await;
}

#[tokio::test]
async fn it_mention_all_expands() {
    run_in_room(|ctx, room| async move {
        let gone = mint_ok(&ctx, &room.andy_token, "andy/gone", &room.project).await;
        sqlx::query("UPDATE identities SET revoked_at = now() WHERE id = $1")
            .bind(&gone.id)
            .execute(&ctx.pool)
            .await
            .unwrap();
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        let raw = envelope(&ulid(), "FYI", json!({}), json!({ "mentions": ["all"] }));
        assert_ack(
            &ingest(&ctx, &room.project, &room.andy, &raw).await,
            &Stream::Feed,
            1,
        );
        let got = sol.feed(ACK_WAIT).await.expect("andy's FYI");
        assert_eq!(
            (got.from_handle.as_str(), got.from_kind),
            ("andy", IdentityKind::Human)
        );
        assert_eq!(
            mentioned(&got),
            idset([room.sol.id.as_str(), room.coord.id.as_str()])
        );
        assert!(!mentioned(&got).contains(HUB_IDENTITY));
    })
    .await;
}

#[tokio::test]
async fn it_agent_group_mention_forbidden() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        let all = envelope(&ulid(), "FYI", json!({}), json!({ "mentions": ["all"] }));
        assert_final_nack(&sol.post(&all).await, NackReason::ForbiddenMention);
        assert_eq!(stored_from(&ctx, &room.sol.id).await, 0);

        let one = envelope(
            &ulid(),
            "FYI",
            json!({}),
            json!({ "mentions": [room.coord.id] }),
        );
        assert_ack(&sol.post(&one).await, &Stream::Feed, 1);
        let got = sol.feed(ACK_WAIT).await.unwrap();
        assert_eq!(mentioned(&got), idset([room.coord.id.as_str()]));
    })
    .await;
}

#[tokio::test]
async fn it_reply_mentions_human_author() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        let ask = ulid();
        let raw = envelope(&ask, "ASK", json!({}), json!({}));
        assert_ack(
            &ingest(&ctx, &room.project, &room.andy, &raw).await,
            &Stream::Feed,
            1,
        );
        let answer = envelope(&ulid(), "ANSWER", json!({}), json!({ "reply_to": ask }));
        assert_ack(&sol.post(&answer).await, &Stream::Feed, 2);

        let note = ulid();
        let raw = fyi(&note);
        assert_ack(
            &ingest(&ctx, &room.project, &room.coord.id, &raw).await,
            &Stream::Feed,
            3,
        );
        let reply = envelope(&ulid(), "FYI", json!({}), json!({ "reply_to": note }));
        assert_ack(&sol.post(&reply).await, &Stream::Feed, 4);

        let feed: Vec<StampedEnvelope> = {
            let mut feed = Vec::new();
            for _ in 0..4 {
                feed.push(sol.feed(ACK_WAIT).await.unwrap());
            }
            feed
        };
        assert_eq!(mentioned(&feed[1]), idset([room.andy.as_str()]));
        assert!(feed[3].mentioned.is_empty(), "{:?}", feed[3].mentioned);
    })
    .await;
}

#[tokio::test]
async fn it_revoked_identity_publish_nacked() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;
        assert_ack(&sol.post(&fyi(&ulid())).await, &Stream::Feed, 1);

        // Revoked while still connected, with messages in its outbox.
        sqlx::query("UPDATE identities SET revoked_at = now() WHERE id = $1")
            .bind(&room.sol.id)
            .execute(&ctx.pool)
            .await
            .unwrap();
        assert_final_nack(&sol.post(&fyi(&ulid())).await, NackReason::IdentityRevoked);
        assert_eq!(stored_from(&ctx, &room.sol.id).await, 1);
    })
    .await;
}

#[tokio::test]
async fn it_ask_to_routes_like_dm_and_answer_follows() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;
        let mut coord = Agent::connect(&ctx, &room.project, &room.coord).await;

        let ask = ulid();
        let raw = envelope(&ask, "ASK", json!({}), json!({ "to": [room.coord.id] }));
        assert_ack(&sol.post(&raw).await, &Stream::Inbox(sol.identity()), 1);
        assert_eq!(coord.inbox(ACK_WAIT).await.unwrap().client.id.as_str(), ask);

        let answer = ulid();
        let raw = envelope(&answer, "ANSWER", json!({}), json!({ "reply_to": ask }));
        assert_ack(&coord.post(&raw).await, &Stream::Inbox(coord.identity()), 2);
        assert_eq!(sol.inbox(ACK_WAIT).await.unwrap().client.id.as_str(), ask);
        assert_eq!(
            sol.inbox(ACK_WAIT).await.unwrap().client.id.as_str(),
            answer
        );

        let expected = idset([
            format!("inbox:{}", room.sol.id).as_str(),
            format!("inbox:{}", room.coord.id).as_str(),
            format!("inbox:{}", room.andy).as_str(),
        ]);
        assert_eq!(streams_of(&ctx, &ask).await, expected);
        assert_eq!(streams_of(&ctx, &answer).await, expected);
        assert!(
            sol.feed(QUIET).await.is_none(),
            "a private ASK reached the feed"
        );
    })
    .await;
}

#[tokio::test]
async fn it_alerts_mention_owner_and_coordinators() {
    run_in_room(|ctx, room| async move {
        // adam holds the coordinator role, so ruling R35 counts him; andy
        // is an admin, which it does not.
        let adam = seed_human(&ctx.pool, 2, "adam").await;
        seed_member(&ctx.pool, &adam, &room.project, "coordinator").await;
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;

        let alert = envelope(&ulid(), "ALERT", json!({ "need": "review B-1" }), json!({}));
        assert_ack(&sol.post(&alert).await, &Stream::Feed, 1);
        let got = sol.feed(ACK_WAIT).await.unwrap();
        assert_eq!(
            mentioned(&got),
            idset([room.andy.as_str(), adam.as_str(), room.coord.id.as_str()])
        );

        let posted = ctx
            .hub
            .hub
            .post_as_hub(
                &topics(&room.project),
                HubPost {
                    payload: Payload::Alert {
                        need: "look at B-1".to_owned(),
                        urgency: Urgency::Normal,
                        needs: Needs::Anyone,
                        actions: Vec::new(),
                    },
                    body: "B-1 changed paths outside its lane".try_into().unwrap(),
                    reply_to: Some(got.client.id.clone()),
                    items: vec!["B-1".parse().unwrap()],
                    lane: None,
                    mentioned: Vec::new(),
                    streams: vec![Stream::Feed],
                },
            )
            .await
            .unwrap();
        assert_eq!(posted.copies, vec![(Stream::Feed, Seq(2))]);
        let from_hub = sol.feed(ACK_WAIT).await.unwrap();
        assert_eq!(from_hub.client.id, posted.id);
        assert_eq!(
            (
                from_hub.from.as_str(),
                from_hub.from_handle.as_str(),
                from_hub.from_kind
            ),
            (HUB_IDENTITY, "hub", IdentityKind::Hub)
        );
        assert_eq!(
            mentioned(&from_hub),
            idset([adam.as_str(), room.coord.id.as_str()])
        );

        // The hub sends only its own verbs.
        let fyi = HubPost {
            payload: Payload::Fyi {},
            body: "x".try_into().unwrap(),
            reply_to: None,
            items: Vec::new(),
            lane: None,
            mentioned: Vec::new(),
            streams: vec![Stream::Feed],
        };
        assert!(
            ctx.hub
                .hub
                .post_as_hub(&topics(&room.project), fyi)
                .await
                .is_err()
        );
    })
    .await;
}

#[tokio::test]
async fn it_hub_resubscribes_after_kick() {
    run_in_room(|ctx, room| async move {
        let _messaging = ctx.hub.start_messaging().await;
        let mut sol = Agent::connect(&ctx, &room.project, &room.sol).await;
        let first = sol.post(&fyi(&ulid())).await;
        assert_ack(&first, &Stream::Feed, 1);

        // Kick the hub's own client off EMQX; it reconnects with a fresh
        // token and, its session being clean, subscribes again.
        let kicked = ctx
            .http
            .delete(emqx_api(&["clients", &ctx.hub.mqtt.client_id()]))
            .basic_auth(
                env("AGMEN_IT_EMQX_API_KEY"),
                Some(env("AGMEN_IT_EMQX_API_SECRET")),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(kicked.status(), StatusCode::NO_CONTENT);

        // A publish made before the hub has subscribed again is lost (its
        // session is clean), and the client resends it with the same id
        // after a timeout; do the same here until it is answered. The PUBACK
        // says no subscriber matched unless other tests' subscribers are up.
        // A kick that lands before the sweeper deletes the outbox rows makes
        // it publish the first ACK again after the reconnect (at-least-once
        // delivery), so a repeat of that ACK is skipped.
        let (up, ack) = (
            sol.topics.up(&sol.identity()),
            sol.topics.ack(&sol.identity()),
        );
        let id = ulid();
        let raw = fyi(&id);
        let deadline = std::time::Instant::now() + ACK_WAIT;
        let reply = 'resend: loop {
            let reason = sol.client.publish(&up, &raw).await;
            assert!(
                matches!(
                    reason,
                    PubAckReason::Success | PubAckReason::NoMatchingSubscribers
                ),
                "{reason:?}"
            );
            let window = tokio::time::Instant::now() + Duration::from_millis(500);
            while let Some(publish) = sol
                .client
                .recv_on(
                    &ack,
                    window.saturating_duration_since(tokio::time::Instant::now()),
                )
                .await
            {
                let reply: DeliveryReply = serde_json::from_slice(&publish.payload).unwrap();
                if reply_id(&reply) == id {
                    break 'resend reply;
                }
                assert_eq!(reply, first, "a reply for neither message");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the hub did not come back"
            );
        };
        assert!(
            matches!(reply, DeliveryReply::Ack { stream: Stream::Feed, seq, .. } if seq >= Seq(2)),
            "{reply:?}"
        );
    })
    .await;
}
