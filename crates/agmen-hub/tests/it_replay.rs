//! Replay, presence, the retained roster and the hub's answer to ROLLCALL,
//! against an in-process hub, a database of the test's own on
//! `AGMEN_IT_PG`, and the dev EMQX (`AGMEN_IT_MQTT`).
//!
//! Tests that use the broker end by disconnecting cleanly (so no last will
//! fires late) and stopping the hub. [`clearing`] then deletes the retained
//! messages they caused (presence, `state/roster`, `state/head`) through
//! the EMQX REST API, even when the test panics, so the dev broker keeps
//! none of them.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use agmen_hub::{HubPost, PostError};
use agmen_proto::{
    DeliveryReply, IdentityKind, Payload, Seq, StampedEnvelope, Stream, Topics, Verb,
};
use jiff::Timestamp;
use reqwest::StatusCode;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{LastWill, PubAckReason, SubscribeReasonCode};
use serde_json::{Value, json};
use support::mqtt::{self, ACK_WAIT, Client, connect_ok};
use support::{
    Ctx, DEV_KEY_1, Minted, Project, TestHub, clearing, envelope, gone_from_broker, login, mint_ok,
    run, seed_human, seed_member, seed_project, topics, ulid,
};

const ANDY: u64 = 4242;
const CAROL: u64 = 77;
const DAVE: u64 = 9001;

/// Keep-alive for the clients in the presence tests. EMQX drops a silent
/// client after 1.5 times this (ADR-0003).
const KEEP_ALIVE: Duration = Duration::from_secs(5);
/// How long a roster change may take to show up.
const ROSTER_WAIT: Duration = Duration::from_secs(10);

const ONLINE: &[u8] = br#"{"status":"online"}"#;
const AWAY: &[u8] = br#"{"status":"away"}"#;
const OFFLINE: &[u8] = br#"{"status":"offline"}"#;

/// Posts `raw` as `from` without the broker, as a human's post will.
async fn ingest(ctx: &Ctx, project: &Project, from: &str, raw: &[u8]) -> DeliveryReply {
    ctx.hub
        .hub
        .ingest(&topics(project), &from.parse().unwrap(), raw)
        .await
        .unwrap()
}

fn assert_acked(reply: &DeliveryReply) {
    assert!(
        matches!(reply, DeliveryReply::Ack { .. }),
        "expected an ACK, got {reply:?}"
    );
}

/// A project where andy (an admin) owns agent sol.
#[derive(Debug)]
struct Room {
    project: Project,
    andy: String,
    andy_token: String,
    sol: Minted,
}

async fn room(ctx: &Ctx) -> Room {
    let project = seed_project(&ctx.pool).await;
    let andy = seed_human(&ctx.pool, ANDY, "andy").await;
    seed_member(&ctx.pool, &andy, &project, "admin").await;
    let andy_token = login(ctx, ANDY, "andy").await;
    let sol = mint_ok(ctx, &andy_token, "andy/sol", &project).await;
    Room {
        project,
        andy,
        andy_token,
        sol,
    }
}

/// `GET /v1/projects/{p}/streams/{s}?{query}` with `token`.
async fn replay(
    ctx: &Ctx,
    token: &str,
    project: &str,
    stream: &str,
    query: &str,
) -> (StatusCode, Value) {
    let resp = ctx
        .http
        .get(
            ctx.hub
                .url(&format!("/v1/projects/{project}/streams/{stream}?{query}")),
        )
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// Stores `count` FYIs from `from` in `project`'s feed with seqs 1 to
/// `count`, as ingest would have, in a few bulk statements.
async fn seed_feed(ctx: &Ctx, project: &Project, from: &Minted, count: u64) {
    const BATCH: u64 = 1_000;
    let hub_at = Timestamp::now();
    let mut next = 1;
    while next <= count {
        let last = (next + BATCH - 1).min(count);
        let (mut seqs, mut ids, mut envelopes) = (Vec::new(), Vec::new(), Vec::new());
        for seq in next..=last {
            // 26 Crockford digits; the seq makes each one unique.
            let id = format!("01JB{seq:022}");
            let stamped = json!({
                "v": 1, "id": id, "verb": "FYI", "data": {}, "reply_to": null, "to": [],
                "mentions": [], "items": [], "lane": null, "body": format!("message {seq}"),
                "valid_until": null, "sent_at": hub_at.to_string(),
                "stream": "feed", "seq": seq, "from": from.id, "from_handle": "andy/sol",
                "from_kind": "agent", "mentioned": [], "refs": [], "hub_at": hub_at.to_string(),
            });
            // The replay API parses what it reads back, so check the shape
            // here, where a mistake is easy to see.
            serde_json::from_value::<StampedEnvelope>(stamped.clone()).unwrap();
            seqs.push(i64::try_from(seq).unwrap());
            ids.push(id);
            envelopes.push(stamped.to_string());
        }
        sqlx::query(
            "INSERT INTO messages
                 (project_id, stream, seq, id, from_id, payload_hash, envelope, hub_at, ack_row)
             SELECT $1, 'feed', s, i, $2, '\\x00'::bytea, e::jsonb, now(), true
             FROM UNNEST($3::bigint[], $4::text[], $5::text[]) AS t(s, i, e)",
        )
        .bind(&project.project)
        .bind(&from.id)
        .bind(&seqs)
        .bind(&ids)
        .bind(&envelopes)
        .execute(&ctx.pool)
        .await
        .unwrap();
        next = last + 1;
    }
    sqlx::query("INSERT INTO stream_heads (stream, seq) VALUES ($1, $2)")
        .bind(format!("{}/feed", project.project))
        .bind(i64::try_from(count).unwrap())
        .execute(&ctx.pool)
        .await
        .unwrap();
}

fn seqs(page: &Value) -> Vec<u64> {
    page["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["seq"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn it_replay_pages_in_order() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        seed_feed(&ctx, &room.project, &room.sol, 10_000).await;
        let project = &room.project.project;

        let (mut after, mut pages, mut seen) = (0_u64, 0, Vec::new());
        let head = loop {
            let (status, page) = replay(
                &ctx,
                &room.sol.token,
                project,
                "feed",
                &format!("after={after}"),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{page}");
            pages += 1;
            let got = seqs(&page);
            assert_eq!(got.len(), 500, "page {pages} holds {} messages", got.len());
            // Every message parses as a stamped envelope of this stream.
            for message in page["messages"].as_array().unwrap() {
                let env: StampedEnvelope = serde_json::from_value(message.clone()).unwrap();
                assert_eq!(env.stream, Stream::Feed);
            }
            seen.extend(got);
            match page["next_after"].as_u64() {
                Some(next) => {
                    assert_eq!(Some(&next), seen.last(), "next_after is the last seq");
                    after = next;
                }
                None => {
                    assert!(page["next_after"].is_null(), "{}", page["next_after"]);
                    break page["head"].as_u64().unwrap();
                }
            }
            assert!(pages < 25, "replay did not end");
        };
        assert_eq!(pages, 20);
        assert_eq!(seen, (1..=10_000).collect::<Vec<_>>());
        assert_eq!(head, 10_000);

        // limit is capped at 500 and may be smaller; 0 is refused.
        let (status, page) = replay(&ctx, &room.sol.token, project, "feed", "limit=900").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(seqs(&page).len(), 500);
        let (status, page) =
            replay(&ctx, &room.sol.token, project, "feed", "after=9990&limit=3").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(seqs(&page), [9991, 9992, 9993]);
        assert_eq!(page["next_after"], 9993);
        let (status, page) = replay(&ctx, &room.sol.token, project, "feed", "limit=0").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{page}");
        assert_eq!(page["error"], "invalid_request");
        // Past the head the page is empty and says where the head is.
        let (status, page) = replay(&ctx, &room.sol.token, project, "feed", "after=10000").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            page,
            json!({ "messages": [], "next_after": null, "head": 10_000 })
        );
    })
    .await;
}

#[tokio::test]
async fn it_replay_inbox_scoping() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let project = room.project.project.clone();
        let carol = seed_human(&ctx.pool, CAROL, "carol").await;
        seed_member(&ctx.pool, &carol, &room.project, "member").await;
        let carol_token = login(&ctx, CAROL, "carol").await;
        let bee = mint_ok(&ctx, &carol_token, "carol/bee", &room.project).await;
        // Dave signed in but belongs to another project, where andy has an
        // agent too.
        let other = seed_project(&ctx.pool).await;
        let dave = seed_human(&ctx.pool, DAVE, "dave").await;
        seed_member(&ctx.pool, &dave, &other, "member").await;
        seed_member(&ctx.pool, &room.andy, &other, "member").await;
        let dave_token = login(&ctx, DAVE, "dave").await;
        let elsewhere = mint_ok(&ctx, &room.andy_token, "andy/far", &other).await;

        // Andy DMs sol: copies in sol's inbox and andy's own.
        let id = ulid();
        let dm = envelope(&id, "DM", json!({}), json!({ "to": [room.sol.id] }));
        assert_acked(&ingest(&ctx, &room.project, &room.andy, &dm).await);
        let sol_inbox = format!("inbox:{}", room.sol.id);
        let andy_inbox = format!("inbox:{}", room.andy);

        // B reads A's inbox: refused.
        let (status, body) = replay(&ctx, &bee.token, &project, &sol_inbox, "").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "forbidden_stream");
        // Andy, A's owner, reads it.
        let (status, page) = replay(&ctx, &room.andy_token, &project, &sol_inbox, "").await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["head"], 1);
        assert_eq!(page["messages"][0]["id"], id.as_str());
        assert_eq!(page["messages"][0]["stream"], sol_inbox.as_str());

        for (token, stream, want) in [
            // A reads its own inbox and the feed.
            (&room.sol.token, sol_inbox.as_str(), StatusCode::OK),
            (&room.sol.token, "feed", StatusCode::OK),
            // A human reads its own inbox and the feed of its project.
            (&room.andy_token, andy_inbox.as_str(), StatusCode::OK),
            (&carol_token, "feed", StatusCode::OK),
            (&bee.token, "feed", StatusCode::OK),
            // Nobody else's inbox: not another member's, nor its owner's.
            (&carol_token, sol_inbox.as_str(), StatusCode::FORBIDDEN),
            (&carol_token, andy_inbox.as_str(), StatusCode::FORBIDDEN),
            (&room.sol.token, andy_inbox.as_str(), StatusCode::FORBIDDEN),
            // Nothing in a project one does not belong to.
            (&dave_token, "feed", StatusCode::FORBIDDEN),
            (&elsewhere.token, "feed", StatusCode::FORBIDDEN),
            // A stream name that is neither feed nor inbox:<id>.
            (&room.sol.token, "inbox:Not-An-Id", StatusCode::BAD_REQUEST),
        ] {
            let (status, body) = replay(&ctx, token, &project, stream, "").await;
            assert_eq!(status, want, "{stream}: {body}");
        }
        // No token at all.
        let resp = ctx
            .http
            .get(ctx.hub.url(&format!("/v1/projects/{project}/streams/feed")))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    })
    .await;
}

#[tokio::test]
async fn it_replay_hub_post_refuses_state() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        // STATE goes to retained state/ topics, never to a numbered stream.
        let refused = ctx
            .hub
            .hub
            .post_as_hub(
                &topics(&room.project),
                HubPost {
                    payload: Payload::State {
                        key: "roster".to_owned(),
                        v: 1,
                        as_of_seq: Seq(0),
                    },
                    body: "roster changed".try_into().unwrap(),
                    reply_to: None,
                    items: Vec::new(),
                    lane: None,
                    mentioned: Vec::new(),
                    streams: vec![Stream::Feed],
                },
            )
            .await;
        assert!(
            matches!(&refused, Err(PostError::NotHubVerb(verb)) if verb == "STATE"),
            "{refused:?}"
        );
    })
    .await;
}

/// The retained topics a presence test leaves behind for `ids`.
fn retained_of(topics: &Topics, ids: &[&Minted]) -> Vec<String> {
    let mut retained = vec![topics.state("roster"), topics.state_head()];
    retained.extend(ids.iter().map(|m| topics.presence(&m.id.parse().unwrap())));
    retained
}

/// `minted`'s broker connection with keep-alive [`KEEP_ALIVE`] and a last
/// will that marks it offline.
async fn connect_with_will(ctx: &Ctx, project: &Project, minted: &Minted) -> Client {
    let (status, body) =
        support::broker_token(&ctx.http, &ctx.hub, &minted.id, &minted.token).await;
    assert_eq!(status, StatusCode::OK, "broker-token answered {body}");
    let token = body["token"].as_str().unwrap();
    let client_id = format!("{}-mcp", minted.id);
    let mut options = mqtt::options(&client_id, &minted.id, token);
    options
        .set_keep_alive(KEEP_ALIVE)
        .set_max_packet_size(Some(96 * 1024))
        .set_last_will(LastWill::new(
            topics(project).presence(&minted.id.parse().unwrap()),
            OFFLINE.to_vec(),
            QoS::AtLeastOnce,
            true,
            None,
        ));
    connect_ok(options).await
}

/// Connects `minted` and publishes its retained presence `status`.
async fn appear(ctx: &Ctx, project: &Project, minted: &Minted, status: &[u8]) -> Client {
    let mut client = connect_with_will(ctx, project, minted).await;
    let topic = topics(project).presence(&minted.id.parse().unwrap());
    // With no subscriber yet the broker answers 0x10, which is a success.
    let reason = client.publish_retained(&topic, status).await;
    assert!(
        matches!(
            reason,
            PubAckReason::Success | PubAckReason::NoMatchingSubscribers
        ),
        "{reason:?}"
    );
    client
}

async fn subscribe(client: &mut Client, filter: &str) {
    assert_eq!(
        client.subscribe(filter).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce),
        "{filter}"
    );
}

/// The roster entry for `id`, if the roster lists it.
fn entry<'a>(roster: &'a [Value], id: &str) -> Option<&'a Value> {
    roster.iter().find(|entry| entry["identity_id"] == id)
}

fn status_of(roster: &[Value], id: &str) -> Option<String> {
    entry(roster, id).map(|entry| entry["status"].as_str().unwrap().to_owned())
}

/// Reads `state/roster` on `client` (subscribed to it) until `done` holds,
/// and returns that roster; fails after `within`.
async fn roster_until(
    client: &mut Client,
    topics: &Topics,
    within: Duration,
    done: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let topic = topics.state("roster");
    let deadline = tokio::time::Instant::now() + within;
    let mut last = None;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Some(publish) = client.recv_on(&topic, left).await else {
            panic!("the roster did not get there within {within:?}; last seen: {last:?}");
        };
        let roster: Vec<Value> = serde_json::from_slice(&publish.payload).unwrap();
        if done(&roster) {
            return roster;
        }
        last = Some(roster);
    }
}

#[tokio::test]
async fn it_presence_last_will() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let watcher = mint_ok(&ctx, &room.andy_token, "andy/watch", &room.project).await;
        let topics = topics(&room.project);
        let retained = retained_of(&topics, &[&room.sol, &watcher]);
        clearing(ctx, retained, |ctx| async move {
            let messaging = ctx.hub.start_messaging().await;
            let mut observer = connect_with_will(&ctx, &room.project, &watcher).await;
            subscribe(&mut observer, &topics.state("roster")).await;
            let sol = appear(&ctx, &room.project, &room.sol, ONLINE).await;
            roster_until(&mut observer, &topics, ROSTER_WAIT, |roster| {
                status_of(roster, &room.sol.id).as_deref() == Some("online")
            })
            .await;

            // Kill sol's socket: no DISCONNECT, so EMQX publishes its will.
            drop(sol);
            let killed = tokio::time::Instant::now();
            let roster = roster_until(&mut observer, &topics, KEEP_ALIVE * 3 / 2, |roster| {
                status_of(roster, &room.sol.id).as_deref() == Some("offline")
            })
            .await;
            let took = killed.elapsed();
            assert!(took <= KEEP_ALIVE * 3 / 2, "offline after {took:?}");
            // Sol stays listed, offline, with its handle and kind.
            let sol_entry = entry(&roster, &room.sol.id).unwrap();
            assert_eq!(sol_entry["handle"], "andy/sol");
            assert_eq!(sol_entry["kind"], "agent");

            observer.disconnect().await;
            drop(messaging);
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn it_presence_roster_resent_by_a_new_hub() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let watcher = mint_ok(&ctx, &room.andy_token, "andy/watch", &room.project).await;
        let topics = topics(&room.project);
        let retained = retained_of(&topics, &[&room.sol, &watcher]);
        clearing(ctx, retained, |ctx| async move {
            let messaging = ctx.hub.start_messaging().await;
            let mut observer = connect_with_will(&ctx, &room.project, &watcher).await;
            subscribe(&mut observer, &topics.state("roster")).await;
            let sol = appear(&ctx, &room.project, &room.sol, ONLINE).await;
            roster_until(&mut observer, &topics, ROSTER_WAIT, |roster| {
                status_of(roster, &room.sol.id).as_deref() == Some("online")
            })
            .await;

            // The hub stops, then sol dies while no hub is there: its will
            // sets its retained presence offline, and the retained roster
            // still says online.
            drop(messaging);
            gone_from_broker(&ctx.http, &ctx.hub.mqtt.client_id()).await;
            let presence = topics.presence(&room.sol.id.parse().unwrap());
            subscribe(&mut observer, &presence).await;
            drop(sol);
            let deadline = tokio::time::Instant::now() + ACK_WAIT;
            loop {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                let publish = observer.recv_on(&presence, left).await.expect("sol's will");
                if publish.payload == OFFLINE {
                    break;
                }
            }

            // A new hub process knows nothing of sol: it reads sol's
            // retained presence and sends the roster again.
            let fresh = TestHub::start(&ctx.db_url, &ctx.github, DEV_KEY_1, &[]).await;
            let messaging = fresh.start_messaging().await;
            roster_until(&mut observer, &topics, ROSTER_WAIT, |roster| {
                status_of(roster, &room.sol.id).as_deref() == Some("offline")
            })
            .await;

            observer.disconnect().await;
            drop(messaging);
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn it_presence_roster_fields_and_bye() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let topics = topics(&room.project);
        let revoked = mint_ok(&ctx, &room.andy_token, "andy/gone", &room.project).await;
        let resp = ctx
            .http
            .delete(ctx.hub.url(&format!("/v1/identities/{}", revoked.id)))
            .bearer_auth(&room.andy_token)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        support::unban_username(&ctx.http, &revoked.id).await;
        let retained = retained_of(&topics, &[&room.sol]);
        clearing(ctx, retained, |ctx| async move {
            let messaging = ctx.hub.start_messaging().await;
            let mut sol = appear(&ctx, &room.project, &room.sol, ONLINE).await;
            subscribe(&mut sol, &topics.state("roster")).await;
            let up = topics.up(&room.sol.id.parse().unwrap());
            let hello = envelope(
                &ulid(),
                "HELLO",
                json!({ "client": "agmen-cli/0.1.0", "agent": "claude-code/2.1.310", "model": "opus" }),
                json!({}),
            );
            assert_eq!(sol.publish(&up, &hello).await, PubAckReason::Success);

            let roster = roster_until(&mut sol, &topics, ROSTER_WAIT, |roster| {
                entry(roster, &room.sol.id).is_some_and(|entry| entry["model"] == "opus")
            })
            .await;
            assert_eq!(
                entry(&roster, &room.sol.id).unwrap(),
                &json!({
                    "identity_id": room.sol.id, "handle": "andy/sol", "kind": "agent",
                    "status": "online", "agent": "claude-code/2.1.310", "model": "opus",
                })
            );
            // The human member is listed (no presence of their own in the
            // MVP), the revoked identity is not.
            assert_eq!(
                entry(&roster, &room.andy).unwrap(),
                &json!({
                    "identity_id": room.andy, "handle": "andy", "kind": "human", "status": "offline",
                })
            );
            assert!(entry(&roster, &revoked.id).is_none(), "{roster:?}");
            assert_eq!(roster.len(), 2, "{roster:?}");

            // BYE: the hub marks sol offline in the roster and in its
            // retained presence, so a hub that restarts reads it back as
            // offline.
            let bye = envelope(&ulid(), "BYE", json!({}), json!({}));
            assert_eq!(sol.publish(&up, &bye).await, PubAckReason::Success);
            roster_until(&mut sol, &topics, ROSTER_WAIT, |roster| {
                status_of(roster, &room.sol.id).as_deref() == Some("offline")
            })
            .await;
            let presence = topics.presence(&room.sol.id.parse().unwrap());
            subscribe(&mut sol, &presence).await;
            let retained = sol.recv_on(&presence, ACK_WAIT).await.expect("retained presence");
            assert_eq!(&retained.payload[..], OFFLINE);

            sol.disconnect().await;
            drop(messaging);
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn it_rollcall_answered_by_hub() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let topics = topics(&room.project);
        let bee = mint_ok(&ctx, &room.andy_token, "andy/bee", &room.project).await;
        let dee = mint_ok(&ctx, &room.andy_token, "andy/dee", &room.project).await;
        // Cat is never connected, so it is offline.
        let cat = mint_ok(&ctx, &room.andy_token, "andy/cat", &room.project).await;
        let retained = retained_of(&topics, &[&room.sol, &bee, &dee]);
        clearing(ctx, retained, |ctx| async move {
            let messaging = ctx.hub.start_messaging().await;
            let mut sol = appear(&ctx, &room.project, &room.sol, ONLINE).await;
            subscribe(&mut sol, &topics.feed()).await;
            subscribe(&mut sol, &topics.state("roster")).await;
            let bee_client = appear(&ctx, &room.project, &bee, ONLINE).await;
            let dee_client = appear(&ctx, &room.project, &dee, AWAY).await;
            roster_until(&mut sol, &topics, ROSTER_WAIT, |roster| {
                status_of(roster, &room.sol.id).as_deref() == Some("online")
                    && status_of(roster, &bee.id).as_deref() == Some("online")
                    && status_of(roster, &dee.id).as_deref() == Some("away")
                    && status_of(roster, &cat.id).as_deref() == Some("offline")
            })
            .await;

            let id = ulid();
            let rollcall = envelope(&id, "ROLLCALL", json!({}), json!({}));
            assert_acked(&ingest(&ctx, &room.project, &room.andy, &rollcall).await);

            let feed = topics.feed();
            let mut answer = None;
            let deadline = tokio::time::Instant::now() + ACK_WAIT;
            while answer.is_none() {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                let publish = sol.recv_on(&feed, left).await.expect("the hub's answer");
                let env: StampedEnvelope = serde_json::from_slice(&publish.payload).unwrap();
                if env.client.payload.verb() == Some(Verb::Status) {
                    answer = Some(env);
                } else {
                    assert_eq!(env.client.id.as_str(), id, "only the ROLLCALL comes first");
                }
            }
            let answer = answer.unwrap();
            assert_eq!(answer.from_kind, IdentityKind::Hub);
            assert_eq!(
                answer.client.reply_to.as_ref().map(|r| r.as_str()),
                Some(id.as_str())
            );
            assert_eq!(answer.stream, Stream::Feed);
            // The human who asked is the one it interrupts.
            assert_eq!(
                answer
                    .mentioned
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>(),
                [room.andy.as_str()]
            );
            let Payload::Status { summary, .. } = &answer.client.payload else {
                panic!("{answer:?}");
            };
            let (count, listed) = summary.split_once(" online: ").expect(summary);
            let listed: BTreeSet<&str> = listed.split(", ").collect();
            assert_eq!(count, "3", "{summary}");
            assert_eq!(
                listed,
                BTreeSet::from(["andy/bee", "andy/dee (away)", "andy/sol"]),
                "{summary}"
            );
            for minted in [&room.sol, &bee, &dee] {
                assert!(
                    answer.client.body.as_str().contains(&minted.id),
                    "the body names {}: {}",
                    minted.id,
                    answer.client.body.as_str()
                );
            }
            assert!(!answer.client.body.as_str().contains(&cat.id));

            for client in [sol, bee_client, dee_client] {
                client.disconnect().await;
            }
            drop(messaging);
        })
        .await;
    })
    .await;
}
