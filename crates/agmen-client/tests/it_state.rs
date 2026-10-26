//! `read_state` and `resolve_handle` (ruling R60), and the presence a
//! watch session publishes, against the dev EMQX and an in-process hub.
//! Each test's project is fresh, and `support::run` deletes the retained
//! topics the test left, even when it fails.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_client::{ClientError, ClientEvent, ClientKind, Session, read_state, resolve_handle};
use agmen_proto::{ClientEnvelope, IdentityId, Timestamp};
use serde_json::{Value, json};
use support::{
    Ctx, PresenceLog, ShortTokens, TestIdentity, close, delivered, disconnected, new_message_id,
    run,
};

/// How many times a test reads the roster before giving up on the hub
/// publishing it.
const ROSTER_READS: usize = 10;

const ONLINE: &str = r#"{"status":"online"}"#;

/// A HELLO with a fresh id: the hub publishes the roster when it records
/// one.
fn hello() -> ClientEnvelope {
    let raw = json!({
        "v": 1, "id": new_message_id(), "verb": "HELLO",
        "data": { "client": "agmen-client-tests/0" },
        "body": "", "sent_at": Timestamp::now(),
    });
    ClientEnvelope::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap()
}

/// Reads the retained roster as `reader` until it gives `id` `status`;
/// panics after 20 s.
async fn roster_status(ctx: &Ctx, reader: &TestIdentity, id: &IdentityId, status: &str) {
    let (file, hub) = (ctx.identity_file(reader), ctx.hub_client(reader));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut last = None;
    loop {
        let roster = read_state(&file, &hub, "roster").await.unwrap();
        let entry = roster.as_ref().and_then(|roster| {
            roster
                .as_array()?
                .iter()
                .find(|entry| entry["identity_id"] == id.as_str())
                .cloned()
        });
        if entry
            .as_ref()
            .is_some_and(|entry| entry["status"] == status)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{id} is not {status} in the roster; last read: {last:?}"
        );
        last = roster;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn it_read_state_roster_and_head() {
    run(|ctx| async move {
        let who = ctx.identity().await;
        let file = ctx.identity_file(&who);
        let hub = ctx.hub_client(&who);

        // Nothing is retained in a fresh project.
        assert_eq!(read_state(&file, &hub, "roster").await.unwrap(), None);

        let (session, mut events) =
            Session::connect(ctx.config(ClientKind::Cli, &who, &ctx.broker))
                .await
                .unwrap();
        let greeting = hello();
        session.publish(greeting.clone()).await.unwrap();
        delivered(&mut events, &greeting.id).await;
        // read_state connects as the same `-cli` client id.
        close(session).await;

        let mut roster = Value::Null;
        for _ in 0..ROSTER_READS {
            if let Some(state) = read_state(&file, &hub, "roster").await.unwrap() {
                roster = state;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let entries = roster.as_array().expect("the hub published the roster");
        assert!(
            entries
                .iter()
                .any(|entry| entry["identity_id"] == who.identity.id.as_str()),
            "{roster}"
        );
        assert_eq!(
            resolve_handle(&roster, &who.identity.handle).unwrap(),
            Some(who.identity.id.clone())
        );
        assert_eq!(resolve_handle(&roster, "nobody/here").unwrap(), None);

        let head = read_state(&file, &hub, "head")
            .await
            .unwrap()
            .expect("the hub published state/head");
        assert_eq!(head["feed_seq"], 1, "{head}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_watch_presence_follows_the_connection() {
    run(|ctx| async move {
        let watcher = ctx.identity().await;
        let reader = ctx.identity().await;
        let id = watcher.identity.id.clone();
        let watch = || Session::connect(ctx.config(ClientKind::Watch, &watcher, &ctx.broker));

        // No HELLO: connecting publishes the retained presence, and the hub
        // lists the identity online.
        let (session, events) = watch().await.unwrap();
        drop(events);
        roster_status(&ctx, &reader, &id, "online").await;

        // The socket drops without DISCONNECT: the broker sends the will.
        drop(session);
        roster_status(&ctx, &reader, &id, "offline").await;

        // Online again on the next connect, and a clean close sends the
        // will too.
        let (session, events) = watch().await.unwrap();
        drop(events);
        roster_status(&ctx, &reader, &id, "online").await;
        close(session).await;
        roster_status(&ctx, &reader, &id, "offline").await;
    })
    .await;
}

/// Only the watch session publishes presence (ruling R65): an MCP server
/// that connects and drops without DISCONNECT leaves no last will, so the
/// identity stays online while its watch runs.
#[tokio::test(flavor = "multi_thread")]
async fn it_mcp_session_leaves_presence_alone() {
    run(|ctx| async move {
        let watcher = ctx.identity().await;
        let reader = ctx.identity().await;
        let id = watcher.identity.id.clone();
        let (watch, events) =
            Session::connect(ctx.config(ClientKind::Watch, &watcher, &ctx.broker))
                .await
                .unwrap();
        drop(events);
        roster_status(&ctx, &reader, &id, "online").await;
        let presence = PresenceLog::start(&ctx, &reader, &id).await;

        let (mcp, events) = Session::connect(ctx.config(ClientKind::Mcp, &watcher, &ctx.broker))
            .await
            .unwrap();
        drop(events);
        drop(mcp);
        disconnected(&format!("{id}-mcp")).await;
        // Time for a will, and for the hub to republish the roster.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let seen = presence.seen();
        assert!(seen.iter().all(|payload| payload == ONLINE), "{seen:?}");
        roster_status(&ctx, &reader, &id, "online").await;
        close(watch).await;
    })
    .await;
}

/// A token rotation ends the old connection with a normal DISCONNECT, which
/// makes the broker discard the last will: across two rotations (12 s
/// tokens, swapped every 6 s) the presence never says offline, and the
/// link never reports a drop.
#[tokio::test(flavor = "multi_thread")]
async fn it_token_rotation_keeps_presence_online() {
    run(|ctx| async move {
        let watcher = ctx.identity().await;
        let reader = ctx.identity().await;
        let id = watcher.identity.id.clone();
        let presence = PresenceLog::start(&ctx, &reader, &id).await;
        let source = ShortTokens::new(&watcher);
        let config = ctx
            .config(ClientKind::Watch, &watcher, &ctx.broker)
            .with_token_source(source.clone());
        let (session, mut events) = Session::connect(config).await.unwrap();
        let until = tokio::time::Instant::now() + Duration::from_secs(16);
        while let Ok(event) = tokio::time::timeout_at(until, events.recv()).await {
            assert_ne!(
                event,
                Some(ClientEvent::Connection { connected: false }),
                "the link dropped"
            );
        }
        assert!(
            source.minted() >= 3,
            "a token at connect, then one every 6 s"
        );
        let seen = presence.seen();
        assert!(
            seen.len() >= 3,
            "online at connect and after each rotation: {seen:?}"
        );
        assert!(seen.iter().all(|payload| payload == ONLINE), "{seen:?}");
        roster_status(&ctx, &reader, &id, "online").await;
        close(session).await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_read_state_refuses_a_bad_key() {
    run(|ctx| async move {
        let who = ctx.identity().await;
        let file = ctx.identity_file(&who);
        let hub = ctx.hub_client(&who);
        for bad in ["", "a/b", "+", "#", "head/#"] {
            assert!(
                matches!(
                    read_state(&file, &hub, bad).await,
                    Err(ClientError::StateKey(_))
                ),
                "{bad:?}"
            );
        }
    })
    .await;
}
