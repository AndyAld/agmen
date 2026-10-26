//! The ledger: lanes, items, claims with leases, FOUND to ITEM, ASSIGN, and
//! the DONE lane check, against an in-process hub and a database of the
//! test's own on `AGMEN_IT_PG`.
//!
//! Every publish goes through `Hub::ingest`, the path the broker worker and
//! the web console share, and the tests read what the hub posted back from
//! the `messages` table. Nothing here needs EMQX, which other tests restart.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agmen_proto::{DeliveryReply, IdentityKind, NackReason, Seq, StampedEnvelope, Stream};
use jiff::Timestamp;
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{
    Ctx, Minted, Project, envelope, login, mint_kind, mint_ok, run, run_with_lease, seed_human,
    seed_member, seed_project, topics, ulid,
};
use tokio::sync::Barrier;
use tokio::task::JoinSet;

const ANDY: u64 = 4242;
const ADAM: u64 = 5151;
/// The lease the lease tests run with (ruling: configurable, 2 s in tests).
const SHORT_LEASE: Duration = Duration::from_secs(2);
/// How long a test waits for the lease sweeper.
const SWEEP_WAIT: Duration = Duration::from_secs(10);

fn valid_for_a_minute() -> Value {
    json!({ "valid_until": (Timestamp::now() + Duration::from_secs(60)).to_string() })
}

fn claim(id: &str, item: &str) -> Vec<u8> {
    envelope(id, "CLAIM", json!({ "item": item }), valid_for_a_minute())
}

fn found(id: &str, title: &str, lane: Option<&str>, severity: Option<&str>) -> Vec<u8> {
    envelope(
        id,
        "FOUND",
        json!({ "title": title, "lane": lane, "severity": severity }),
        json!({}),
    )
}

fn done(id: &str, item: &str, paths: &[&str]) -> Vec<u8> {
    let changes: Vec<Value> = paths
        .iter()
        .map(|path| json!({ "status": "M", "path": path }))
        .collect();
    envelope(
        id,
        "DONE",
        json!({
            "item": item, "branch": "fix/b-1", "commit": "a1b2c3d4e5f6",
            "base_sha": "0f1e2d3c4b5a", "checks": [], "changes": changes,
        }),
        json!({}),
    )
}

/// A project where andy (an admin) owns agent sol, and adam (a
/// coordinator) owns coordinator adam/coord.
#[derive(Debug)]
struct Room {
    project: Project,
    andy: String,
    andy_token: String,
    adam: String,
    sol: Minted,
    coord: Minted,
}

async fn room(ctx: &Ctx) -> Room {
    let project = seed_project(&ctx.pool).await;
    let andy = seed_human(&ctx.pool, ANDY, "andy").await;
    seed_member(&ctx.pool, &andy, &project, "admin").await;
    let adam = seed_human(&ctx.pool, ADAM, "adam").await;
    seed_member(&ctx.pool, &adam, &project, "coordinator").await;
    let andy_token = login(ctx, ANDY, "andy").await;
    let adam_token = login(ctx, ADAM, "adam").await;
    let sol = mint_ok(ctx, &andy_token, "andy/sol", &project).await;
    let coord = mint_kind(ctx, &adam_token, "adam/coord", &project, "coordinator").await;
    Room {
        project,
        andy,
        andy_token,
        adam,
        sol,
        coord,
    }
}

/// Posts `raw` as `from` through ingest, as the broker worker does.
async fn post(ctx: &Ctx, project: &Project, from: &str, raw: &[u8]) -> DeliveryReply {
    ctx.hub
        .hub
        .ingest(&topics(project), &from.parse().unwrap(), raw)
        .await
        .unwrap()
}

/// Posts `raw` and checks the hub recorded it.
async fn post_ok(ctx: &Ctx, project: &Project, from: &str, raw: &[u8]) -> (Stream, Seq) {
    match post(ctx, project, from, raw).await {
        DeliveryReply::Ack { stream, seq, .. } => (stream, seq),
        nack @ DeliveryReply::Nack { .. } => panic!("expected an ACK, got {nack:?}"),
    }
}

/// Every stored copy of a message whose `reply_to` is `id`, by stream.
async fn replies(ctx: &Ctx, id: &str) -> Vec<(String, StampedEnvelope)> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT stream, envelope::text FROM messages
         WHERE envelope->>'reply_to' = $1 ORDER BY stream",
    )
    .bind(id)
    .fetch_all(&ctx.pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(stream, json)| (stream, serde_json::from_str(&json).unwrap()))
        .collect()
}

/// The one hub message answering `id`, as its feed copy, with the streams it
/// was stored in.
async fn answer(ctx: &Ctx, id: &str) -> (StampedEnvelope, BTreeSet<String>) {
    let copies = replies(ctx, id).await;
    let streams: BTreeSet<String> = copies.iter().map(|(stream, _)| stream.clone()).collect();
    let (_, feed) = copies
        .into_iter()
        .find(|(stream, _)| stream == "feed")
        .unwrap_or_else(|| panic!("no feed answer to {id}"));
    assert_eq!(feed.from_kind, IdentityKind::Hub);
    (feed, streams)
}

/// The JSON of a stamped message's verb and data.
fn verb_data(env: &StampedEnvelope) -> (String, Value) {
    let json = serde_json::to_value(env).unwrap();
    (
        json["verb"].as_str().unwrap().to_owned(),
        json["data"].clone(),
    )
}

/// The feed messages with `verb`, in seq order.
async fn feed_verb(ctx: &Ctx, verb: &str) -> Vec<StampedEnvelope> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT envelope::text FROM messages
         WHERE stream = 'feed' AND envelope->>'verb' = $1 ORDER BY seq",
    )
    .bind(verb)
    .fetch_all(&ctx.pool)
    .await
    .unwrap();
    rows.iter()
        .map(|json| serde_json::from_str(json).unwrap())
        .collect()
}

/// Posts a CLAIM for `item` as `from` and returns the hub's answer: its
/// verb, data and streams.
async fn claim_answer(
    ctx: &Ctx,
    project: &Project,
    from: &str,
    item: &str,
) -> (String, Value, BTreeSet<String>) {
    let id = ulid();
    post_ok(ctx, project, from, &claim(&id, item)).await;
    let (env, streams) = answer(ctx, &id).await;
    let (verb, data) = verb_data(&env);
    (verb, data, streams)
}

fn assert_granted(answer: &(String, Value, BTreeSet<String>), item: &str) {
    assert_eq!(answer.0, "GRANT", "{answer:?}");
    assert_eq!(answer.1["item"], item, "{answer:?}");
}

fn assert_denied(answer: &(String, Value, BTreeSet<String>), reason: &str) {
    assert_eq!(answer.0, "DENY", "{answer:?}");
    assert_eq!(answer.1["reason"], reason, "{answer:?}");
}

/// Posts a FOUND as `from` and returns the minted key.
async fn found_key(ctx: &Ctx, project: &Project, from: &str, lane: Option<&str>) -> String {
    let id = ulid();
    post_ok(ctx, project, from, &found(&id, "a problem", lane, None)).await;
    let (env, _) = answer(ctx, &id).await;
    let (verb, data) = verb_data(&env);
    assert_eq!(verb, "ITEM");
    data["key"].as_str().unwrap().to_owned()
}

/// `PUT /v1/projects/{p}/lanes` with `body` of `content_type`.
async fn put_lanes(
    ctx: &Ctx,
    project: &Project,
    token: &str,
    content_type: &str,
    body: String,
) -> (StatusCode, Value) {
    let resp = ctx
        .http
        .put(
            ctx.hub
                .url(&format!("/v1/projects/{}/lanes", project.project)),
        )
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn put_lanes_json(ctx: &Ctx, project: &Project, token: &str, lanes: &Value) -> StatusCode {
    let (status, body) = put_lanes(
        ctx,
        project,
        token,
        "application/json",
        json!({ "lanes": lanes }).to_string(),
    )
    .await;
    assert!(
        status != StatusCode::INTERNAL_SERVER_ERROR,
        "PUT lanes answered {body}"
    );
    status
}

/// `GET /v1/projects/{p}/{what}` with `token`.
async fn get(ctx: &Ctx, project: &Project, token: &str, what: &str) -> (StatusCode, Value) {
    let resp = ctx
        .http
        .get(
            ctx.hub
                .url(&format!("/v1/projects/{}/{what}", project.project)),
        )
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// L-UI holds `ui/`, L-API holds `api/`.
async fn ui_and_api_lanes(ctx: &Ctx, room: &Room) {
    let status = put_lanes_json(
        ctx,
        &room.project,
        &room.andy_token,
        &json!([
            { "name": "L-UI", "globs": ["ui/**"] },
            { "name": "L-API", "globs": ["api/**"] },
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// Active claims on `item`, as holder ids.
async fn holders(ctx: &Ctx, item: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT identity_id FROM claims WHERE item_key = $1 AND released_at IS NULL")
        .bind(item)
        .fetch_all(&ctx.pool)
        .await
        .unwrap()
}

/// Waits for the hub's RELEASE of `item`, failing after [`SWEEP_WAIT`].
async fn wait_for_release(ctx: &Ctx, item: &str) -> StampedEnvelope {
    let deadline = Instant::now() + SWEEP_WAIT;
    loop {
        let released = feed_verb(ctx, "RELEASE")
            .await
            .into_iter()
            .find(|env| env.from_kind == IdentityKind::Hub && verb_data(env).1["item"] == item);
        if let Some(env) = released {
            return env;
        }
        assert!(
            Instant::now() < deadline,
            "no RELEASE of {item} within {SWEEP_WAIT:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn lease_sweeper(ctx: &Ctx) -> tokio::task::JoinHandle<()> {
    let hub = ctx.hub.hub.clone();
    tokio::spawn(async move {
        hub.run_lease_sweeper().await.unwrap();
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn it_twenty_claims_one_grant() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let item = found_key(&ctx, &room.project, &room.sol.id, None).await;
        assert_eq!(item, "B-1");
        let mut agents = Vec::new();
        for n in 0..20 {
            agents
                .push(mint_ok(&ctx, &room.andy_token, &format!("andy/a{n}"), &room.project).await);
        }

        // All 20 CLAIMs enter ingest at once.
        let barrier = Arc::new(Barrier::new(agents.len()));
        let mut claims = JoinSet::new();
        let mut ids = Vec::new();
        for agent in &agents {
            let (hub, topics, barrier) =
                (ctx.hub.hub.clone(), topics(&room.project), barrier.clone());
            let from: agmen_proto::IdentityId = agent.id.parse().unwrap();
            let id = ulid();
            let raw = claim(&id, &item);
            ids.push((agent.id.clone(), id));
            claims.spawn(async move {
                barrier.wait().await;
                hub.ingest(&topics, &from, &raw).await.unwrap()
            });
        }
        while let Some(reply) = claims.join_next().await {
            assert!(matches!(reply.unwrap(), DeliveryReply::Ack { .. }));
        }

        let mut granted = Vec::new();
        for (agent, id) in &ids {
            let (env, streams) = answer(&ctx, id).await;
            assert!(streams.contains(&format!("inbox:{agent}")), "{streams:?}");
            match verb_data(&env) {
                (verb, data) if verb == "GRANT" => {
                    assert_eq!(data["item"], "B-1");
                    granted.push(agent.clone());
                }
                (verb, data) => {
                    assert_eq!(
                        (verb.as_str(), &data["reason"]),
                        ("DENY", &json!("held_by"))
                    );
                }
            }
        }
        assert_eq!(granted.len(), 1, "exactly one GRANT: {granted:?}");
        assert_eq!(feed_verb(&ctx, "GRANT").await.len(), 1);
        assert_eq!(feed_verb(&ctx, "DENY").await.len(), 19);
        assert_eq!(holders(&ctx, "B-1").await, granted);
    })
    .await;
}

#[tokio::test]
async fn it_lease_expiry_releases() {
    run_with_lease(SHORT_LEASE, |ctx| async move {
        let room = room(&ctx).await;
        let sweeper = lease_sweeper(&ctx);
        let item = found_key(&ctx, &room.project, &room.sol.id, None).await;
        let granted = claim_answer(&ctx, &room.project, &room.sol.id, &item).await;
        assert_granted(&granted, &item);
        let lease_until: Timestamp = granted.1["lease_until"].as_str().unwrap().parse().unwrap();
        let ahead = lease_until.duration_since(Timestamp::now());
        assert!(
            ahead > jiff::SignedDuration::from_millis(1000)
                && ahead <= jiff::SignedDuration::from_secs(2),
            "lease ends in {ahead}"
        );

        // No HEARTBEAT: the sweeper releases the claim once the lease ends.
        let started = Instant::now();
        let released = wait_for_release(&ctx, &item).await;
        assert!(started.elapsed() >= Duration::from_millis(900));
        assert_eq!(verb_data(&released).1["reason"], "lease_expired");
        assert_eq!(released.client.items[0].as_str(), item);
        assert!(holders(&ctx, &item).await.is_empty());

        let regranted = claim_answer(&ctx, &room.project, &room.coord.id, &item).await;
        assert_granted(&regranted, &item);
        assert_eq!(
            holders(&ctx, &item).await,
            std::slice::from_ref(&room.coord.id)
        );
        sweeper.abort();
    })
    .await;
}

#[tokio::test]
async fn it_heartbeat_renews() {
    run_with_lease(SHORT_LEASE, |ctx| async move {
        let room = room(&ctx).await;
        let sweeper = lease_sweeper(&ctx);
        let item = found_key(&ctx, &room.project, &room.sol.id, None).await;
        assert_granted(
            &claim_answer(&ctx, &room.project, &room.sol.id, &item).await,
            &item,
        );

        for _ in 0..4 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let heartbeat = envelope(&ulid(), "HEARTBEAT", json!({ "items": [item] }), json!({}));
            // Not posted to the feed: the ACK reports the sender's own inbox.
            let (stream, _) = post_ok(&ctx, &room.project, &room.sol.id, &heartbeat).await;
            assert_eq!(stream, Stream::Inbox(room.sol.id.parse().unwrap()));
        }
        // Four seconds in, twice the lease, the claim still stands.
        assert_eq!(
            holders(&ctx, &item).await,
            std::slice::from_ref(&room.sol.id)
        );
        assert!(feed_verb(&ctx, "RELEASE").await.is_empty());
        assert!(feed_verb(&ctx, "HEARTBEAT").await.is_empty());
        let (status, items) = get(&ctx, &room.project, &room.sol.token, "items").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(items["items"][0]["holder"], room.sol.id.as_str(), "{items}");
        assert_eq!(items["items"][0]["holder_handle"], "andy/sol");

        // Once the heartbeats stop, the lease runs out.
        wait_for_release(&ctx, &item).await;
        sweeper.abort();
    })
    .await;
}

#[tokio::test]
async fn it_overlapping_lanes_rejected() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let toml = r#"
            [[lanes]]
            name = "L-UI"
            globs = ["ui/**"]
            owner = "andy/sol"

            [[lanes]]
            name = "L-API"
            globs = ["api/**", "/Cargo.toml"]
        "#;
        let (status, body) = put_lanes(
            &ctx,
            &room.project,
            &room.andy_token,
            "application/toml",
            toml.to_owned(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        for overlapping in [
            json!([
                { "name": "L-A", "globs": ["ui/**"] },
                { "name": "L-B", "globs": ["ui/src/**"] },
            ]),
            // A glob without a slash matches at any depth, as in gitignore.
            json!([
                { "name": "L-A", "globs": ["*.ts"] },
                { "name": "L-B", "globs": ["ui/**"] },
            ]),
            // A directory covers everything under it.
            json!([
                { "name": "L-A", "globs": ["docs"] },
                { "name": "L-B", "globs": ["docs/adr/*.md"] },
            ]),
        ] {
            let (status, body) = put_lanes(
                &ctx,
                &room.project,
                &room.andy_token,
                "application/json",
                json!({ "lanes": overlapping }).to_string(),
            )
            .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{overlapping}");
            assert_eq!(body["error"], "overlapping_lanes", "{body}");
            let description = body["error_description"].as_str().unwrap();
            assert!(
                description.contains("L-A") && description.contains("L-B"),
                "{description}"
            );
        }
        // Globs that cannot match one path do not overlap.
        let disjoint = json!([
            { "name": "L-A", "globs": ["ui/*.ts"] },
            { "name": "L-B", "globs": ["ui/*.rs", "api/**"] },
        ]);
        let other = seed_project(&ctx.pool).await;
        seed_member(&ctx.pool, &room.andy, &other, "admin").await;
        assert_eq!(
            put_lanes_json(&ctx, &other, &room.andy_token, &disjoint).await,
            StatusCode::OK
        );

        // An agent may not set lanes; the refused sets changed nothing.
        assert_eq!(
            put_lanes_json(&ctx, &room.project, &room.sol.token, &disjoint).await,
            StatusCode::FORBIDDEN
        );
        let (status, lanes) = get(&ctx, &room.project, &room.sol.token, "lanes").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(lanes["v"], 1, "{lanes}");
        assert_eq!(lanes["lanes"][0]["name"], "L-API", "{lanes}");
        assert_eq!(lanes["lanes"][0]["globs"], json!(["api/**", "/Cargo.toml"]));
        assert_eq!(lanes["lanes"][1]["name"], "L-UI");
        assert_eq!(lanes["lanes"][1]["owner"], room.sol.id.as_str());
        assert_eq!(lanes["lanes"][1]["owner_handle"], "andy/sol");
    })
    .await;
}

/// A distinct anchored literal glob of nearly 256 bytes. Its many distinct
/// bytes give it a DFA of about 260 KiB, under the 1 MiB per-glob limit.
fn wide_glob(seed: usize) -> String {
    let chars: Vec<char> = ('0'..='9')
        .chain('a'..='z')
        .chain('A'..='Z')
        .chain('\u{a1}'..='\u{ff}')
        .collect();
    let mut glob = String::from("/");
    for i in seed.. {
        let next = chars[i % chars.len()];
        if glob.len() + next.len_utf8() > 256 {
            break;
        }
        glob.push(next);
    }
    glob
}

#[tokio::test]
async fn it_lanes_over_dfa_budget_rejected() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        ui_and_api_lanes(&ctx, &room).await;

        // Within the lane, glob and per-glob DFA limits, but 96 DFAs of about
        // 260 KiB each pass the 16 MiB budget for one request.
        let lanes: Vec<Value> = (0..3)
            .map(|lane| {
                let globs: Vec<String> = (lane * 32..(lane + 1) * 32).map(wide_glob).collect();
                json!({ "name": format!("L-W{lane}"), "globs": globs })
            })
            .collect();
        let (status, body) = put_lanes(
            &ctx,
            &room.project,
            &room.andy_token,
            "application/json",
            json!({ "lanes": lanes }).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], "lanes_too_complex", "{body}");
        let description = body["error_description"].as_str().unwrap();
        assert!(description.contains("16 MiB"), "{description}");

        // The refused set stored nothing.
        let (status, stored) = get(&ctx, &room.project, &room.sol.token, "lanes").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(stored["v"], 1, "{stored}");
        let names: Vec<&str> = stored["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|lane| lane["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["L-API", "L-UI"]);
    })
    .await;
}

#[tokio::test]
async fn it_check_path() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        ui_and_api_lanes(&ctx, &room).await;
        let ui_item = found_key(&ctx, &room.project, &room.sol.id, Some("L-UI")).await;
        assert_granted(
            &claim_answer(&ctx, &room.project, &room.sol.id, &ui_item).await,
            &ui_item,
        );
        let api_item = found_key(&ctx, &room.project, &room.coord.id, Some("L-API")).await;
        assert_granted(
            &claim_answer(&ctx, &room.project, &room.coord.id, &api_item).await,
            &api_item,
        );

        let check = |token: String, body: Value| {
            let request = ctx
                .http
                .post(
                    ctx.hub
                        .url(&format!("/v1/projects/{}/check-path", room.project.project)),
                )
                .bearer_auth(token)
                .json(&body);
            async move {
                let resp = request.send().await.unwrap();
                (resp.status(), resp.json::<Value>().await.unwrap())
            }
        };
        let (status, answer) = check(
            room.sol.token.clone(),
            json!({
                "identity": room.sol.id,
                "paths": ["ui/x.ts", "api/y.rs", "README.md", "../etc/passwd"],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(
            answer,
            json!([
                { "path": "ui/x.ts", "lane": "L-UI", "allowed": true,
                  "reason": "lane L-UI held by andy/sol" },
                { "path": "api/y.rs", "lane": "L-API", "allowed": false,
                  "reason": "lane L-API held by adam/coord" },
                { "path": "README.md", "lane": null, "allowed": true, "reason": "unowned" },
                { "path": "../etc/passwd", "lane": null, "allowed": false,
                  "reason": "not a repository path" },
            ])
        );

        // Once the claim is released, nobody holds the lane.
        let release = envelope(&ulid(), "RELEASE", json!({ "item": api_item }), json!({}));
        post_ok(&ctx, &room.project, &room.coord.id, &release).await;
        let (_, answer) = check(
            room.andy_token.clone(),
            json!({ "identity": room.sol.id, "paths": ["api/y.rs"] }),
        )
        .await;
        assert_eq!(answer[0]["reason"], "not claimed", "{answer}");
        assert_eq!(answer[0]["allowed"], false);

        let (status, _) = check(
            room.sol.token.clone(),
            json!({ "identity": "nosuchidentity", "paths": ["ui/x.ts"] }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    })
    .await;
}

#[tokio::test]
async fn it_done_out_of_lane_alerts() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        ui_and_api_lanes(&ctx, &room).await;
        let item = found_key(&ctx, &room.project, &room.sol.id, Some("L-UI")).await;
        assert_granted(
            &claim_answer(&ctx, &room.project, &room.sol.id, &item).await,
            &item,
        );

        let id = ulid();
        let raw = done(&id, &item, &["ui/x.ts", "api/y.rs", "README.md"]);
        assert_eq!(
            post_ok(&ctx, &room.project, &room.sol.id, &raw).await.0,
            Stream::Feed
        );
        let (alert, streams) = answer(&ctx, &id).await;
        assert_eq!(streams, BTreeSet::from(["feed".to_owned()]));
        let (verb, data) = verb_data(&alert);
        assert_eq!(verb, "ALERT");
        assert!(data["need"].as_str().unwrap().contains(&item), "{data}");
        let body = alert.client.body.as_str();
        assert!(body.contains("\"api/y.rs\" (L-API)"), "{body}");
        assert!(
            !body.contains("ui/x.ts") && !body.contains("README.md"),
            "{body}"
        );
        assert_eq!(alert.client.items[0].as_str(), item);
        let mentioned: BTreeSet<String> = alert.mentioned.iter().map(ToString::to_string).collect();
        assert_eq!(
            mentioned,
            BTreeSet::from([room.coord.id.clone(), room.adam.clone()]),
            "coordinators(project): the coordinator identity and the coordinator human"
        );

        // A DONE inside the claimed lanes raises nothing, and closes the item.
        let quiet = ulid();
        post_ok(
            &ctx,
            &room.project,
            &room.sol.id,
            &done(&quiet, &item, &["ui/y.ts"]),
        )
        .await;
        assert!(replies(&ctx, &quiet).await.is_empty());
        assert_eq!(feed_verb(&ctx, "ALERT").await.len(), 1);
        let (_, items) = get(&ctx, &room.project, &room.sol.token, "items").await;
        assert_eq!(items["items"][0]["state"], "done", "{items}");
    })
    .await;
}

#[tokio::test]
async fn it_done_alert_falls_back_to_owner() {
    run(|ctx| async move {
        // No coordinators: the ALERT goes to the owning human (ruling R35).
        let project = seed_project(&ctx.pool).await;
        let andy = seed_human(&ctx.pool, ANDY, "andy").await;
        seed_member(&ctx.pool, &andy, &project, "admin").await;
        let andy_token = login(&ctx, ANDY, "andy").await;
        let sol = mint_ok(&ctx, &andy_token, "andy/sol", &project).await;
        let status = put_lanes_json(
            &ctx,
            &project,
            &andy_token,
            &json!([{ "name": "L-API", "globs": ["api/"] }]),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Paths past the 50th are counted, not listed.
        let paths: Vec<String> = (0..60).map(|n| format!("api/f{n:02}.rs")).collect();
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let id = ulid();
        post_ok(&ctx, &project, &sol.id, &done(&id, "B-7", &paths)).await;
        let (alert, _) = answer(&ctx, &id).await;
        let mentioned: Vec<String> = alert.mentioned.iter().map(ToString::to_string).collect();
        assert_eq!(mentioned, [andy]);
        let body = alert.client.body.as_str();
        assert!(
            body.contains("\"api/f49.rs\"") && !body.contains("api/f50.rs"),
            "{body}"
        );
        assert!(body.ends_with("and 10 more"), "{body}");
    })
    .await;
}

#[tokio::test]
async fn it_found_mints_item() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let first = ulid();
        post_ok(
            &ctx,
            &room.project,
            &room.sol.id,
            &found(&first, "first", Some("L-UI"), None),
        )
        .await;
        let (item, streams) = answer(&ctx, &first).await;
        assert_eq!(
            verb_data(&item),
            ("ITEM".to_owned(), json!({ "key": "B-1" }))
        );
        assert_eq!(
            streams,
            BTreeSet::from(["feed".to_owned(), format!("inbox:{}", room.sol.id)])
        );
        assert_eq!(item.client.lane.as_ref().unwrap().as_str(), "L-UI");
        assert_eq!(
            found_key(&ctx, &room.project, &room.coord.id, None).await,
            "B-2"
        );

        // A security FOUND stays in the finder's and its owner's inboxes;
        // the feed gets only the key and lane.
        let secret = ulid();
        let title = "auth tokens logged in plain text";
        let (stream, _) = post_ok(
            &ctx,
            &room.project,
            &room.sol.id,
            &found(&secret, title, Some("L-API"), Some("security")),
        )
        .await;
        assert_eq!(stream, Stream::Inbox(room.sol.id.parse().unwrap()));
        let copies: Vec<String> = sqlx::query_scalar("SELECT stream FROM messages WHERE id = $1")
            .bind(&secret)
            .fetch_all(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(
            copies.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                format!("inbox:{}", room.sol.id),
                format!("inbox:{}", room.andy)
            ])
        );
        let (item, _) = answer(&ctx, &secret).await;
        assert_eq!(verb_data(&item).1, json!({ "key": "B-3" }));
        assert_eq!(item.client.lane.as_ref().unwrap().as_str(), "L-API");
        let feed: Vec<String> =
            sqlx::query_scalar("SELECT envelope::text FROM messages WHERE stream = 'feed'")
                .fetch_all(&ctx.pool)
                .await
                .unwrap();
        assert!(feed.iter().all(|env| !env.contains(title)), "{feed:?}");

        let (status, items) = get(&ctx, &room.project, &room.coord.token, "items").await;
        assert_eq!(status, StatusCode::OK);
        let items = items["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["key"], "B-1");
        assert_eq!(items[0]["title"], "first");
        assert_eq!(items[0]["created_by"], room.sol.id.as_str());
        assert_eq!(items[2]["key"], "B-3");
        assert_eq!(items[2]["title"], Value::Null);
        assert_eq!(items[2]["severity"], "security");
    })
    .await;
}

#[tokio::test]
async fn it_claim_valid_until_uses_hub_clock() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let item = found_key(&ctx, &room.project, &room.sol.id, None).await;
        // The laptop's clock is 10 minutes fast; the hub judges valid_until
        // by its own clock with 120 s of skew allowed.
        let now = Timestamp::now();
        let skewed = |id: &str, until: Timestamp| {
            envelope(
                id,
                "CLAIM",
                json!({ "item": item }),
                json!({
                    "sent_at": (now + Duration::from_secs(600)).to_string(),
                    "valid_until": until.to_string(),
                }),
            )
        };
        // Past valid_until by more than the skew: recorded, then DENY expired.
        let stale = ulid();
        post_ok(
            &ctx,
            &room.project,
            &room.sol.id,
            &skewed(&stale, now - Duration::from_secs(180)),
        )
        .await;
        let (deny, _) = answer(&ctx, &stale).await;
        assert_eq!(
            verb_data(&deny).1,
            json!({ "item": item, "reason": "expired" })
        );
        assert!(holders(&ctx, &item).await.is_empty());

        let recent = ulid();
        post_ok(
            &ctx,
            &room.project,
            &room.sol.id,
            &skewed(&recent, now - Duration::from_secs(60)),
        )
        .await;
        let (grant, _) = answer(&ctx, &recent).await;
        assert_eq!(verb_data(&grant).0, "GRANT");
    })
    .await;
}

#[tokio::test]
async fn it_claim_denials() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        // L-API is adam/coord's lane.
        let status = put_lanes_json(
            &ctx,
            &room.project,
            &room.andy_token,
            &json!([{ "name": "L-API", "globs": ["api/**"], "owner": room.coord.id }]),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_denied(
            &claim_answer(&ctx, &room.project, &room.sol.id, "B-99").await,
            "closed",
        );

        let item = found_key(&ctx, &room.project, &room.sol.id, Some("L-API")).await;
        assert_denied(
            &claim_answer(&ctx, &room.project, &room.sol.id, &item).await,
            "not_in_lane",
        );
        // The coordinator assigns it to sol, which may then claim it, twice.
        let assign = envelope(
            &ulid(),
            "ASSIGN",
            json!({ "item": item, "to": room.sol.id }),
            json!({}),
        );
        post_ok(&ctx, &room.project, &room.coord.id, &assign).await;
        assert_granted(
            &claim_answer(&ctx, &room.project, &room.sol.id, &item).await,
            &item,
        );
        assert_granted(
            &claim_answer(&ctx, &room.project, &room.sol.id, &item).await,
            &item,
        );
        assert_eq!(
            holders(&ctx, &item).await,
            std::slice::from_ref(&room.sol.id)
        );
        assert_denied(
            &claim_answer(&ctx, &room.project, &room.coord.id, &item).await,
            "held_by",
        );

        // A human may not claim (protocol 4).
        let human_claim = claim(&ulid(), &item);
        match post(&ctx, &room.project, &room.andy, &human_claim).await {
            DeliveryReply::Nack { reason, .. } => assert_eq!(reason, NackReason::ForbiddenVerb),
            ack @ DeliveryReply::Ack { .. } => panic!("a human claimed: {ack:?}"),
        }

        // DONE closes the item; after the release nobody can claim it.
        post_ok(
            &ctx,
            &room.project,
            &room.sol.id,
            &done(&ulid(), &item, &["api/x.rs"]),
        )
        .await;
        let release = envelope(&ulid(), "RELEASE", json!({ "item": item }), json!({}));
        post_ok(&ctx, &room.project, &room.sol.id, &release).await;
        assert!(holders(&ctx, &item).await.is_empty());
        assert_denied(
            &claim_answer(&ctx, &room.project, &room.coord.id, &item).await,
            "closed",
        );
    })
    .await;
}

#[tokio::test]
async fn it_release_frees_item() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let item = found_key(&ctx, &room.project, &room.sol.id, None).await;
        assert_granted(
            &claim_answer(&ctx, &room.project, &room.sol.id, &item).await,
            &item,
        );

        // A RELEASE from someone who neither holds the item nor coordinates
        // changes nothing.
        let other = mint_ok(&ctx, &room.andy_token, "andy/other", &room.project).await;
        let release = |id: &str| {
            envelope(
                id,
                "RELEASE",
                json!({ "item": item, "reason": "done for now" }),
                json!({}),
            )
        };
        assert_eq!(
            post_ok(&ctx, &room.project, &other.id, &release(&ulid()))
                .await
                .0,
            Stream::Feed
        );
        assert_eq!(
            holders(&ctx, &item).await,
            std::slice::from_ref(&room.sol.id)
        );

        post_ok(&ctx, &room.project, &room.sol.id, &release(&ulid())).await;
        assert!(holders(&ctx, &item).await.is_empty());
        assert_granted(
            &claim_answer(&ctx, &room.project, &other.id, &item).await,
            &item,
        );
    })
    .await;
}

/// Makes every write to `items` fail (`on`) or work again, through a
/// statement trigger in the test's own database.
async fn fail_item_writes(ctx: &Ctx, on: bool) {
    let sql = if on {
        "CREATE FUNCTION it_fail() RETURNS trigger LANGUAGE plpgsql AS
             $$ BEGIN RAISE EXCEPTION 'injected ledger failure'; END $$;
         CREATE TRIGGER it_fail BEFORE INSERT OR UPDATE ON items
             FOR EACH STATEMENT EXECUTE FUNCTION it_fail();"
    } else {
        "DROP TRIGGER it_fail ON items; DROP FUNCTION it_fail();"
    };
    sqlx::raw_sql(sql).execute(&ctx.pool).await.unwrap();
}

fn assert_busy(reply: &DeliveryReply) {
    assert!(
        matches!(
            reply,
            DeliveryReply::Nack {
                reason: NackReason::HubBusy,
                retryable: true,
                ..
            }
        ),
        "expected a retryable hub_busy NACK, got {reply:?}"
    );
}

#[tokio::test]
async fn it_ledger_failure_records_nothing() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        ui_and_api_lanes(&ctx, &room).await;
        let item = found_key(&ctx, &room.project, &room.sol.id, Some("L-UI")).await;
        assert_granted(
            &claim_answer(&ctx, &room.project, &room.sol.id, &item).await,
            &item,
        );

        // While the ledger fails, a FOUND or DONE is refused whole: never an
        // ACK without the ITEM, or a DONE that skipped its lane check.
        fail_item_writes(&ctx, true).await;
        let found_id = ulid();
        let found_raw = found(&found_id, "another problem", None, None);
        assert_busy(&post(&ctx, &room.project, &room.sol.id, &found_raw).await);
        let done_id = ulid();
        let done_raw = done(&done_id, &item, &["api/y.rs"]);
        assert_busy(&post(&ctx, &room.project, &room.sol.id, &done_raw).await);
        let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM messages WHERE id = ANY($1)")
            .bind([found_id.as_str(), done_id.as_str()])
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(stored, 0, "a refused message leaves nothing behind");

        // The client's resend runs the ledger again.
        fail_item_writes(&ctx, false).await;
        post_ok(&ctx, &room.project, &room.sol.id, &found_raw).await;
        let (env, _) = answer(&ctx, &found_id).await;
        assert_eq!(
            verb_data(&env),
            ("ITEM".to_owned(), json!({ "key": "B-2" }))
        );
        post_ok(&ctx, &room.project, &room.sol.id, &done_raw).await;
        let (alert, _) = answer(&ctx, &done_id).await;
        assert_eq!(verb_data(&alert).0, "ALERT");
        let body = alert.client.body.as_str();
        assert!(body.contains("\"api/y.rs\" (L-API)"), "{body}");
    })
    .await;
}
