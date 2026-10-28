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

        // EMQX can list the client gone a moment before the server has
        // seen its session stop. Until then a state read, which changes
        // nothing, fails on the dead session; once the server has seen
        // the stop, the call answers that, once.
        let deadline = Instant::now() + WAIT;
        loop {
            let result = mcp.call("state", json!({})).await;
            let text = text_of(&result);
            assert_eq!(result.is_error, Some(true), "{text}");
            if text.starts_with("agmen: the broker session stopped: ") {
                break;
            }
            assert_eq!(
                text, "agmen: reading the project's state failed: the session has stopped",
                "the server has not seen the stop yet"
            );
            assert!(Instant::now() < deadline, "the server never saw the stop");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
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

/// A startup that cannot connect does not push the first HEARTBEAT back by
/// a period: the round that connects after the beat fell due sends it.
#[tokio::test(flavor = "multi_thread")]
async fn it_heartbeat_is_not_late_after_a_failed_startup() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let mcp = Mcp::start(options(&sol)).await;
        mcp.text("found", json!({ "title": "the build is slow" }))
            .await;
        let text = mcp.text("claim", json!({ "item": "B-1" })).await;
        assert!(text.contains("\ngranted B-1 until "), "{text}");
        mcp.stop().await;
        let before = heartbeats(&ctx.base, &sol, "B-1").await;

        // A broken identity file makes every round fail until it is put
        // back: rounds at about 0, 1, 3, 7 and 15 s with this period.
        let every = Duration::from_secs(10);
        let file = sol.wt.path().join(".agmen/identity.json");
        let good = std::fs::read(&file).unwrap();
        std::fs::write(&file, "{ not json").unwrap();
        let started = Instant::now();
        let mcp = Mcp::start(options(&sol).with_heartbeat(every)).await;
        tokio::time::sleep(Duration::from_secs(8)).await;
        std::fs::write(&file, good).unwrap();

        let client_id = format!("{}-mcp", sol.identity.identity_id.as_str());
        wait_for_client(&client_id, "never connected", |client| {
            client.is_some_and(|client| client["connected"] == true)
        })
        .await;
        // The beat is due a period after startup, or at the connect if that
        // came later. Before the fix it came a whole period after the
        // connect.
        let deadline = (started + every).max(Instant::now()) + Duration::from_secs(3);
        loop {
            let beats = heartbeats(&ctx.base, &sol, "B-1").await;
            if beats > before {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no HEARTBEAT within 3 s of it falling due ({before} before)"
            );
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        mcp.stop().await;
    })
    .await;
}
