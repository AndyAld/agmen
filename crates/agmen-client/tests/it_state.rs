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
use serde_json::json;
use support::{
    Ctx, PresenceLog, ShortTokens, TestIdentity, close, delivered, disconnected, kick,
    new_message_id, run, subscribed_to, subscriptions,
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

/// How many roster reads `roster_status` makes before it gives up.
const ROSTER_POLLS: usize = 100;

/// Reads the roster over HTTP as `reader` until it gives `id` `status`;
/// panics after 20 s or [`ROSTER_POLLS`] reads.
async fn roster_status(ctx: &Ctx, reader: &TestIdentity, id: &IdentityId, status: &str) {
    let hub = ctx.hub_client(reader);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut last = None;
    for _ in 0..ROSTER_POLLS {
        let roster = hub.roster().await.unwrap();
        let entry = roster.as_array().and_then(|entries| {
            entries
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
        last = Some(roster);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{id} is not {status} after {ROSTER_POLLS} reads; last read: {last:?}");
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

        let mut head = None;
        for _ in 0..ROSTER_READS {
            if let Some(state) = read_state(&file, &hub, "roster").await.unwrap() {
                head = Some(state);
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let head = head.expect("the hub published the roster head");
        assert!(head["count"].as_u64().is_some(), "{head}");
        assert!(head["online"].as_u64().is_some(), "{head}");
        let etag = head["etag"].as_str().expect("etag");
        assert_eq!(etag.len(), 16, "{head}");
        assert!(
            etag.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{head}"
        );
        let roster = hub.roster().await.unwrap();
        let entries = roster.as_array().expect("the roster is an array");
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

/// Ruling R68: a session reads retained state through its own connection,
/// so it never takes the `-cli` client id from a one-shot command. A read
/// leaves the session's subscriptions as it found them (a watch keeps
/// `state/head`) and never drops the connection.
#[tokio::test(flavor = "multi_thread")]
async fn it_session_reads_state_on_its_own_connection() {
    run(|ctx| async move {
        let who = ctx.identity().await;
        let id = who.identity.id.clone();
        let (mcp, mut events) = Session::connect(ctx.config(ClientKind::Mcp, &who, &ctx.broker))
            .await
            .unwrap();
        // Nothing is retained in a fresh project.
        assert_eq!(mcp.read_state("roster").await.unwrap(), None);
        for bad in ["", "a/b", "+", "#", "head/#"] {
            assert!(
                matches!(mcp.read_state(bad).await, Err(ClientError::StateKey(_))),
                "{bad:?}"
            );
        }

        let greeting = hello();
        mcp.publish(greeting.clone()).await.unwrap();
        delivered(&mut events, &greeting.id).await;
        let mut head = None;
        for _ in 0..ROSTER_READS {
            if let Some(state) = mcp.read_state("roster").await.unwrap() {
                head = Some(state);
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let head = head.expect("the hub published the roster head");
        assert!(
            head["count"].is_number() && head["etag"].is_string(),
            "{head}"
        );
        let roster = ctx.hub_client(&who).roster().await.unwrap();
        assert_eq!(
            resolve_handle(&roster, &who.identity.handle).unwrap(),
            Some(id.clone()),
            "{roster}"
        );
        let head = mcp
            .read_state("head")
            .await
            .unwrap()
            .expect("the hub published state/head");
        assert_eq!(head["feed_seq"], 1, "{head}");
        subscribed_to(&format!("{id}-mcp"), &[ctx.topics.ack(&id)]).await;

        // A watch subscribes to state/head itself and keeps it, at QoS 1.
        let (watch, watch_events) =
            Session::connect(ctx.config(ClientKind::Watch, &who, &ctx.broker))
                .await
                .unwrap();
        drop(watch_events);
        let head = watch.read_state("head").await.unwrap().expect("state/head");
        assert_eq!(head["feed_seq"], 1, "{head}");
        let roster = watch.read_state("roster").await.unwrap();
        assert!(
            roster
                .as_ref()
                .is_some_and(|head| head["count"].is_number() && head["etag"].is_string()),
            "{roster:?}"
        );
        let mut want = vec![
            ctx.topics.ack(&id),
            ctx.topics.feed(),
            ctx.topics.inbox(&id),
            ctx.topics.state_head(),
        ];
        want.sort();
        subscribed_to(&format!("{id}-watch"), &want).await;
        close(watch).await;

        while let Ok(event) = tokio::time::timeout(Duration::from_millis(300), events.recv()).await
        {
            assert!(event.is_some(), "the mcp session stopped");
            assert_ne!(
                event,
                Some(ClientEvent::Connection { connected: false }),
                "the mcp session dropped"
            );
        }
        close(mcp).await;
    })
    .await;
}

/// A session's state read whose connection drops after the broker granted
/// the subscription, and before any retained message came, fails: it does
/// not say that nothing is retained, which `resolve` would turn into "no
/// identity has that handle".
#[tokio::test(flavor = "multi_thread")]
async fn it_session_state_read_fails_when_the_connection_drops() {
    run(|ctx| async move {
        let who = ctx.identity().await;
        let client_id = format!("{}-mcp", who.identity.id);
        let (mcp, events) = Session::connect(ctx.config(ClientKind::Mcp, &who, &ctx.broker))
            .await
            .unwrap();
        drop(events);
        // Nothing is retained on state/roster in a fresh project, so the
        // read waits its 3 s after the SUBACK.
        let topic = ctx.topics.state("roster");
        let (read, ()) = tokio::join!(mcp.read_state("roster"), async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while !subscriptions(&client_id)
                .await
                .iter()
                .any(|(subscribed, _)| *subscribed == topic)
            {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the read never subscribed"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // The SUBACK has reached the client well before this.
            tokio::time::sleep(Duration::from_millis(300)).await;
            kick(&client_id).await;
        });
        // The session reconnects, so this is not Closed, and the broker
        // answered the subscription, so the timeout is the 3 s state wait,
        // not the 10 s subscribe wait.
        assert!(
            matches!(read, Err(ClientError::Timeout(wait)) if wait == Duration::from_secs(3)),
            "{read:?}"
        );
        close(mcp).await;
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
