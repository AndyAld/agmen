//! `agmen mcp` after its `<id>-mcp` broker session stopped on its own: the
//! next tool call reports it once and the one after reconnects, and the
//! HEARTBEAT for held claims goes on without any tool call.
//!
//! A kick alone does not stop a session (the link reconnects), so the
//! tests stop it the way another process can: they hold the write lock on
//! the identity's SQLite file and kick the session. On the reconnect the
//! session's outbox write waits out its 5 s busy timeout, fails, and the
//! session stops.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::time::{Duration, Instant};

use agmen_cli::McpOptions;
use agmen_client::{HubClient, STATE_DB, TokenKey};
use agmen_hub::Role;
use agmen_proto::{Payload, Stream};
use serde_json::{Value, json};
use support::mcp::{Mcp, text_of};
use support::{Ingest, Member, WAIT, emqx_client, kick, run};

/// Options for `who`'s worktree, home and state.
fn options(who: &Member) -> McpOptions {
    McpOptions::new(who.wt.path(), who.home.tokens(), who.home.state_base())
}

/// Waits up to [`WAIT`] until `ready` accepts what EMQX lists for
/// `client_id`, checking every 100 ms.
async fn wait_for_client(client_id: &str, what: &str, ready: impl Fn(Option<&Value>) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let client = emqx_client(client_id).await;
        if ready(client.as_ref()) {
            return;
        }
        assert!(Instant::now() < deadline, "{client_id} {what}: {client:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Stops `who`'s running `-mcp` session on its own (see the module docs).
/// Returns the connection that holds the write lock; dropping it lets the
/// file be written again.
async fn stop_mcp_session(who: &Member) -> rusqlite::Connection {
    let id = who.identity.identity_id.as_str();
    let client_id = format!("{id}-mcp");
    let connected_at = |client: Option<&Value>| {
        client
            .filter(|client| client["connected"] == true)
            .map(|client| client["connected_at"].clone())
    };
    let before = connected_at(emqx_client(&client_id).await.as_ref())
        .expect("the -mcp session is connected");

    let lock = rusqlite::Connection::open(who.home.state_base().join(id).join(STATE_DB)).unwrap();
    lock.busy_timeout(Duration::from_secs(5)).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();

    kick(&client_id).await;
    wait_for_client(&client_id, "never reconnected", |client| {
        connected_at(client).is_some_and(|at| at != before)
    })
    .await;
    wait_for_client(&client_id, "never stopped", |client| {
        connected_at(client).is_none()
    })
    .await;
    lock
}

/// The HEARTBEATs naming `item` in `who`'s inbox, as the hub recorded them.
async fn heartbeats(base: &url::Url, who: &Member, item: &str) -> usize {
    let token = who
        .home
        .tokens()
        .get(&TokenKey::new(base, &who.identity.identity_id).unwrap())
        .unwrap()
        .expect("join stored the identity token");
    let hub = HubClient::new(
        base.clone(),
        who.identity.project.clone(),
        who.identity.identity_id.clone(),
        token,
    )
    .unwrap();
    let inbox = Stream::Inbox(who.identity.identity_id.clone());
    hub.page(&inbox, 0)
        .await
        .unwrap()
        .messages
        .iter()
        .filter(|message| {
            matches!(&message.client.payload,
                Payload::Heartbeat { items } if items.iter().any(|i| i.as_str() == item))
        })
        .count()
}

/// A session that stopped on its own is one tool error that says so; the
/// next call connects a new session and works.
#[tokio::test(flavor = "multi_thread")]
async fn it_stopped_session_is_one_error_then_reconnects() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let mcp = Mcp::start(options(&sol)).await;
        let text = mcp
            .text("post", json!({ "verb": "FYI", "body": "before" }))
            .await;
        assert_eq!(text, "recorded #1 (feed)");

        let lock = stop_mcp_session(&sol).await;
        drop(lock);

        let result = mcp
            .call("post", json!({ "verb": "FYI", "body": "lost" }))
            .await;
        let text = text_of(&result);
        assert_eq!(result.is_error, Some(true), "{text}");
        assert!(
            text.starts_with("agmen: the broker session stopped: "),
            "{text}"
        );
        let text = mcp
            .text("post", json!({ "verb": "FYI", "body": "after" }))
            .await;
        assert_eq!(text, "recorded #2 (feed)");
        mcp.stop().await;
    })
    .await;
}

/// After the session stopped on its own, the held claim still gets its
/// HEARTBEAT, with no tool call to reconnect.
#[tokio::test(flavor = "multi_thread")]
async fn it_heartbeat_reconnects_a_stopped_session() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let mcp = Mcp::start(options(&sol).with_heartbeat(Duration::from_secs(1))).await;
        mcp.text("found", json!({ "title": "the build is slow" }))
            .await;
        let text = mcp.text("claim", json!({ "item": "B-1" })).await;
        assert!(text.contains("\ngranted B-1 until "), "{text}");

        let lock = stop_mcp_session(&sol).await;
        // While the lock holds, no HEARTBEAT can be queued, so the count
        // stays put until the server has connected again.
        let before = heartbeats(&ctx.base, &sol, "B-1").await;
        drop(lock);

        let deadline = Instant::now() + WAIT;
        loop {
            let beats = heartbeats(&ctx.base, &sol, "B-1").await;
            if beats > before {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no HEARTBEAT in {WAIT:?} after the session stopped ({before} before)"
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        mcp.stop().await;
    })
    .await;
}
