//! `GET /v1/projects/{p}/state?key=roster|head`: the retained JSON the hub
//! publishes to `state/{key}`, over HTTP with the feed seq it reflects
//! (protocol sections 2 and 6). Against an in-process hub, a database of
//! the test's own on `AGMEN_IT_PG`, and the dev EMQX (`AGMEN_IT_MQTT`) for
//! the retained topics the answers are compared with.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::time::Duration;

use reqwest::StatusCode;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{LastWill, SubscribeReasonCode};
use serde_json::{Value, json};
use support::mqtt::{self, Client, connect_ok};
use support::{
    Ctx, Minted, Project, clearing, envelope, login, mint_ok, run, seed_human, seed_member,
    seed_project, topics, ulid,
};

const ANDY: u64 = 4242;
const CAROL: u64 = 77;

/// How long a retained topic may take to show what the hub just did.
const WAIT: Duration = Duration::from_secs(10);

const ONLINE: &[u8] = br#"{"status":"online"}"#;
const OFFLINE: &[u8] = br#"{"status":"offline"}"#;

#[derive(Debug)]
struct Room {
    project: Project,
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
        andy_token,
        sol,
    }
}

/// `GET /v1/projects/{project}/state` with `token` and an optional `key`.
async fn state(ctx: &Ctx, token: &str, project: &str, key: Option<&str>) -> (StatusCode, Value) {
    let mut url = ctx.hub.url(&format!("/v1/projects/{project}/state"));
    if let Some(key) = key {
        url.query_pairs_mut().append_pair("key", key);
    }
    let resp = ctx.http.get(url).bearer_auth(token).send().await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// Posts an FYI as andy; returns once the hub stored it.
async fn say(ctx: &Ctx, token: &str, project: &str) {
    let resp = ctx
        .http
        .post(ctx.hub.url(&format!("/v1/projects/{project}/messages")))
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(envelope(&ulid(), "FYI", json!({}), json!({ "body": "hi" })))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// `minted` on the broker, with a last will that marks it offline.
async fn connect_with_will(ctx: &Ctx, project: &Project, minted: &Minted) -> Client {
    let (status, body) =
        support::broker_token(&ctx.http, &ctx.hub, &minted.id, &minted.token).await;
    assert_eq!(status, StatusCode::OK, "broker-token answered {body}");
    let token = body["token"].as_str().unwrap();
    let mut options = mqtt::options(&format!("{}-mcp", minted.id), &minted.id, token);
    options
        .set_keep_alive(Duration::from_secs(5))
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

async fn subscribe(client: &mut Client, filter: &str) {
    assert_eq!(
        client.subscribe(filter).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce),
        "{filter}"
    );
}

/// Reads `topic` on `client` until `done` holds for its JSON payload.
async fn retained_until(client: &mut Client, topic: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut last = None;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Some(publish) = client.recv_on(topic, left).await else {
            panic!("{topic} never got there; last seen: {last:?}");
        };
        let value: Value = serde_json::from_slice(&publish.payload).unwrap();
        if done(&value) {
            return value;
        }
        last = Some(value);
    }
}

fn retained_of(project: &Project, ids: &[&Minted]) -> Vec<String> {
    let topics = topics(project);
    let mut retained = vec![topics.state("roster"), topics.state_head()];
    retained.extend(ids.iter().map(|m| topics.presence(&m.id.parse().unwrap())));
    retained
}

#[tokio::test]
async fn it_state_roster_matches_the_retained_topic() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let watcher = mint_ok(&ctx, &room.andy_token, "andy/watch", &room.project).await;
        let retained = retained_of(&room.project, &[&room.sol, &watcher]);
        clearing(ctx, retained, |ctx| async move {
            let p = room.project.project.as_str();
            let topic = topics(&room.project).state("roster");
            let messaging = ctx.hub.start_messaging().await;
            let mut observer = connect_with_will(&ctx, &room.project, &watcher).await;
            subscribe(&mut observer, &topic).await;
            let mut sol = connect_with_will(&ctx, &room.project, &room.sol).await;
            sol.publish_retained(
                &topics(&room.project).presence(&room.sol.id.parse().unwrap()),
                ONLINE,
            )
            .await;
            say(&ctx, &room.andy_token, p).await;
            let retained = retained_until(&mut observer, &topic, |roster| {
                roster
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|e| e["identity_id"] == room.sol.id && e["status"] == "online")
            })
            .await;

            let (status, body) = state(&ctx, &room.andy_token, p, Some("roster")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["key"], "roster");
            assert_eq!(body["value"], retained, "HTTP differs from the topic");
            assert_eq!(body["as_of_seq"], 1, "the feed holds one message");

            sol.disconnect().await;
            observer.disconnect().await;
            drop(messaging);
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn it_state_head_matches_the_retained_topic() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let watcher = mint_ok(&ctx, &room.andy_token, "andy/watch", &room.project).await;
        let retained = retained_of(&room.project, &[&room.sol, &watcher]);
        clearing(ctx, retained, |ctx| async move {
            let p = room.project.project.as_str();
            let topic = topics(&room.project).state_head();
            let messaging = ctx.hub.start_messaging().await;
            let mut observer = connect_with_will(&ctx, &room.project, &watcher).await;
            subscribe(&mut observer, &topic).await;
            say(&ctx, &room.andy_token, p).await;
            say(&ctx, &room.andy_token, p).await;
            let retained =
                retained_until(&mut observer, &topic, |head| head["feed_seq"] == 2).await;

            let (status, body) = state(&ctx, &room.andy_token, p, Some("head")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["key"], "head");
            assert_eq!(body["as_of_seq"], 2);
            // Same fields; hub_at is the time of each build.
            let (mut got, mut want) = (body["value"].clone(), retained);
            assert!(got["hub_at"].is_string());
            got["hub_at"] = Value::Null;
            want["hub_at"] = Value::Null;
            assert_eq!(got, want);

            observer.disconnect().await;
            drop(messaging);
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn it_state_an_empty_project_is_at_seq_zero() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let p = room.project.project.as_str();
        let (status, body) = state(&ctx, &room.andy_token, p, Some("head")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["as_of_seq"], 0);
        assert_eq!(body["value"]["feed_seq"], 0);
        let (status, body) = state(&ctx, &room.andy_token, p, Some("roster")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["as_of_seq"], 0);
        // Offline until its presence arrives, but listed: andy and sol.
        assert_eq!(body["value"].as_array().unwrap().len(), 2);
    })
    .await;
}

#[tokio::test]
async fn it_state_refuses_non_members_and_unknown_keys() {
    run(|ctx| async move {
        let room = room(&ctx).await;
        let p = room.project.project.as_str();

        // A member of another project.
        let other = seed_project(&ctx.pool).await;
        let carol = seed_human(&ctx.pool, CAROL, "carol").await;
        seed_member(&ctx.pool, &carol, &other, "member").await;
        let carol_token = login(&ctx, CAROL, "carol").await;
        let (status, body) = state(&ctx, &carol_token, p, Some("roster")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "not_a_member");

        // An identity of the project reads; one of another project does not.
        let (status, _) = state(&ctx, &room.sol.token, p, Some("head")).await;
        assert_eq!(status, StatusCode::OK);
        let stranger = mint_ok(&ctx, &carol_token, "carol/sol", &other).await;
        let (status, body) = state(&ctx, &stranger.token, p, Some("head")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "not_a_member");

        // No token.
        let resp = ctx
            .http
            .get(ctx.hub.url(&format!("/v1/projects/{p}/state?key=head")))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // No key, an unknown key, and a key the hub does not serve yet.
        for key in [None, Some(""), Some("nope"), Some("lanes"), Some("ROSTER")] {
            let (status, body) = state(&ctx, &room.andy_token, p, key).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{key:?}: {body}");
            assert_eq!(body["error"], "invalid_request", "{key:?}");
        }
        // A bad key is checked after membership, so a stranger learns nothing.
        let (status, _) = state(&ctx, &carol_token, p, Some("nope")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    })
    .await;
}
