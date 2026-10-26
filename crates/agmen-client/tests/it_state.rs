//! `read_state` and `resolve_handle` (ruling R60), and the presence a
//! watch session publishes, against the dev EMQX and an in-process hub.
//! Each test's project is fresh, and `support::run` deletes the retained
//! topics the test left, even when it fails.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_client::{ClientError, ClientKind, Session, read_state, resolve_handle};
use agmen_proto::{ClientEnvelope, IdentityId, Timestamp};
use serde_json::{Value, json};
use support::{Ctx, TestIdentity, close, delivered, new_message_id, run};

/// How many times a test reads the roster before giving up on the hub
/// publishing it.
const ROSTER_READS: usize = 10;

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
