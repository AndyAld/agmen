//! The retained `state/roster` head stays small, and the full roster is
//! `GET /v1/projects/{p}/state?key=roster`. Against an in-process hub, a
//! database of the test's own on `AGMEN_IT_PG`, and the dev EMQX
//! (`AGMEN_IT_MQTT`) for the retained head.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use reqwest::StatusCode;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{LastWill, SubscribeReasonCode};
use serde_json::{Value, json};
use support::mqtt::{self, Client, connect_ok};
use support::{
    Ctx, Minted, Project, clearing, login, mint_ok, run, seed_human, seed_member, seed_project,
    topics,
};

const ANDY: u64 = 4242;
const CAROL: u64 = 77;

/// How long a retained topic may take to show what the hub just did.
const WAIT: Duration = Duration::from_secs(10);

/// Heads one wait reads before it gives up.
const MAX_HEADS: usize = 1_000;

/// Agents beside the one human in [`it_full_roster_over_http`]. 1 + 24 = 25.
const EXTRA_AGENTS: usize = 24;

/// The retained `state/{key}` payload limit (protocol section 8), in bytes.
const STATE_LIMIT: usize = 4 * 1024;

const OFFLINE: &[u8] = br#"{"status":"offline"}"#;

/// `GET /v1/projects/{project}/state?key=roster`.
async fn roster_get(ctx: &Ctx, token: &str, project: &str) -> (StatusCode, Option<String>, Value) {
    let mut url = ctx.hub.url(&format!("/v1/projects/{project}/state"));
    url.query_pairs_mut().append_pair("key", "roster");
    let resp = ctx.http.get(url).bearer_auth(token).send().await.unwrap();
    let status = resp.status();
    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = resp.json().await.unwrap_or(Value::Null);
    (status, etag, body)
}

/// Mints `handle` with an `agent` name near the 128-byte limit, so each roster
/// entry is big enough that 25 of them pass [`STATE_LIMIT`].
async fn mint_long(ctx: &Ctx, token: &str, handle: &str, project: &Project) -> Minted {
    let agent = format!("claude-code/2.1.310 {}", "x".repeat(100));
    let (status, body) = support::mint(
        ctx,
        token,
        &json!({ "handle": handle, "project": project.project, "kind": "agent", "agent": agent }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "mint answered {body}");
    Minted {
        id: body["id"].as_str().unwrap().to_owned(),
        token: body["token"].as_str().unwrap().to_owned(),
        body,
    }
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

/// Reads `topic` until `done` holds, at most [`MAX_HEADS`] publishes or [`WAIT`].
async fn retained_until(client: &mut Client, topic: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut last = None;
    for _ in 0..MAX_HEADS {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let Some(publish) = client.recv_on(topic, left).await else {
            break;
        };
        let value: Value = serde_json::from_slice(&publish.payload).unwrap();
        if done(&value) {
            return value;
        }
        last = Some(value);
    }
    panic!("{topic} never got there; last seen: {last:?}");
}

fn retained_of(project: &Project, ids: &[&Minted]) -> Vec<String> {
    let topics = topics(project);
    let mut retained = vec![topics.state("roster"), topics.state_head()];
    retained.extend(
        ids.iter()
            .map(|minted| topics.presence(&minted.id.parse().unwrap())),
    );
    retained
}

#[tokio::test]
async fn it_full_roster_over_http() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        let andy = seed_human(&ctx.pool, ANDY, "andy").await;
        seed_member(&ctx.pool, &andy, &project, "admin").await;
        let token = login(&ctx, ANDY, "andy").await;
        let mut agents = Vec::with_capacity(EXTRA_AGENTS);
        for n in 0..EXTRA_AGENTS {
            let handle = format!("andy/a{n:02}");
            agents.push(mint_long(&ctx, &token, &handle, &project).await);
        }
        let watcher = agents[0].clone();
        let want: BTreeSet<String> = std::iter::once(andy)
            .chain(agents.iter().map(|agent| agent.id.clone()))
            .collect();
        assert_eq!(want.len(), EXTRA_AGENTS + 1);
        let retained = retained_of(&project, &[&watcher]);
        clearing(ctx, retained, |ctx| async move {
            full_roster(&ctx, &project, &token, &watcher, &want).await;
        })
        .await;
    })
    .await;
}

async fn full_roster(
    ctx: &Ctx,
    project: &Project,
    token: &str,
    watcher: &Minted,
    want: &BTreeSet<String>,
) {
    let p = project.project.as_str();
    let topic = topics(project).state("roster");
    let messaging = ctx.hub.start_messaging().await;
    let mut observer = connect_with_will(ctx, project, watcher).await;
    subscribe(&mut observer, &topic).await;
    let head = retained_until(&mut observer, &topic, |head| {
        head["count"].as_u64() == Some(u64::try_from(EXTRA_AGENTS + 1).unwrap())
    })
    .await;
    let bytes = serde_json::to_vec(&head).unwrap();
    assert!(bytes.len() < STATE_LIMIT, "{} bytes: {head}", bytes.len());
    assert_eq!(
        head["count"].as_u64(),
        Some(u64::try_from(EXTRA_AGENTS + 1).unwrap())
    );
    assert_eq!(head["online"], 0, "{head}");
    let etag = head["etag"].as_str().expect("etag");
    assert_eq!(etag.len(), 16, "{head}");

    let (status, raw, body) = roster_get(ctx, token, p).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = body["value"].as_array().expect("roster array");
    assert_eq!(entries.len(), EXTRA_AGENTS + 1, "{body}");
    // The premise: this roster is too big for the retained topic, so only
    // HTTP can carry it whole.
    let full = serde_json::to_vec(entries).unwrap();
    assert!(
        full.len() > STATE_LIMIT,
        "the roster is {} bytes",
        full.len()
    );
    let got: BTreeSet<_> = entries
        .iter()
        .map(|entry| entry["identity_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(got, *want);
    assert_eq!(raw.as_deref(), Some(format!("\"{etag}\"")).as_deref());

    observer.disconnect().await;
    drop(messaging);
}

#[tokio::test]
async fn it_head_changes_with_roster() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        let andy = seed_human(&ctx.pool, ANDY, "andy").await;
        seed_member(&ctx.pool, &andy, &project, "admin").await;
        let token = login(&ctx, ANDY, "andy").await;
        let watcher = mint_ok(&ctx, &token, "andy/watch", &project).await;
        let retained = retained_of(&project, &[&watcher]);
        clearing(ctx, retained, |ctx| async move {
            head_changes(&ctx, &project, &token, &watcher).await;
        })
        .await;
    })
    .await;
}

async fn head_changes(ctx: &Ctx, project: &Project, token: &str, watcher: &Minted) {
    let p = project.project.as_str();
    let topic = topics(project).state("roster");
    let messaging = ctx.hub.start_messaging().await;
    let mut observer = connect_with_will(ctx, project, watcher).await;
    subscribe(&mut observer, &topic).await;
    let first = retained_until(&mut observer, &topic, |head| head["etag"].is_string()).await;
    let before = first["etag"].as_str().unwrap().to_owned();
    let _extra = mint_ok(ctx, token, "andy/extra", project).await;
    let next = retained_until(&mut observer, &topic, |head| {
        head["etag"].as_str().is_some_and(|etag| etag != before)
    })
    .await;

    let (status, raw, body) = roster_get(ctx, token, p).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let raw = raw.expect("ETag");
    let bare = raw.trim_matches('"');
    assert_eq!(raw, format!("\"{bare}\""));
    assert_eq!(next["etag"], bare);
    assert_ne!(bare, before);

    observer.disconnect().await;
    drop(messaging);
}

#[tokio::test]
async fn it_roster_members_only() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        let andy = seed_human(&ctx.pool, ANDY, "andy").await;
        seed_member(&ctx.pool, &andy, &project, "admin").await;
        let token = login(&ctx, ANDY, "andy").await;
        let sol = mint_ok(&ctx, &token, "andy/sol", &project).await;

        let other = seed_project(&ctx.pool).await;
        let carol = seed_human(&ctx.pool, CAROL, "carol").await;
        seed_member(&ctx.pool, &carol, &other, "member").await;
        let carol_token = login(&ctx, CAROL, "carol").await;
        let p = project.project.as_str();

        let (status, _, body) = roster_get(&ctx, &carol_token, p).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "not_a_member");

        let (status, _, body) = roster_get(&ctx, &sol.token, p).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["value"].is_array(), "{body}");
    })
    .await;
}
