//! `agmen mcp` in this process against an in-process hub and the dev EMQX,
//! driven by an rmcp client: what each tool answers, that peer text reaches
//! the model sanitized (ruling R63), and the HEARTBEAT for held claims.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::time::{Duration, Instant};

use agmen_cli::McpOptions;
use agmen_client::{ClientEvent, ClientKind, Events, HubClient, Session, SessionConfig, TokenKey};
use agmen_hub::Role;
use agmen_proto::{Mention, Payload, StampedEnvelope, Stream};
use serde_json::json;
use support::mcp::{Mcp, text_of};
use support::{Ingest, Member, WAIT, assert_exit, run, stdout};

/// Options for `who`'s worktree, home and state.
fn options(who: &Member) -> McpOptions {
    McpOptions::new(who.wt.path(), who.home.tokens(), who.home.state_base())
}

/// `who`'s hub client, with the identity token `agmen join` stored.
fn hub_client(base: &url::Url, who: &Member) -> HubClient {
    let token = who
        .home
        .tokens()
        .get(&TokenKey::new(base, &who.identity.identity_id).unwrap())
        .unwrap()
        .expect("join stored the identity token");
    HubClient::new(
        base.clone(),
        who.identity.project.clone(),
        who.identity.identity_id.clone(),
        token,
    )
    .unwrap()
}

/// `stream` as `who` reads it through the hub's replay API.
async fn read(base: &url::Url, who: &Member, stream: &Stream) -> Vec<StampedEnvelope> {
    hub_client(base, who)
        .page(stream, 0)
        .await
        .unwrap()
        .messages
}

/// Reads `events` until they stay quiet for 500 ms, and panics when the
/// session lost its connection or stopped meanwhile.
async fn assert_never_dropped(events: &mut Events) {
    while let Ok(event) = tokio::time::timeout(Duration::from_millis(500), events.recv()).await {
        match event {
            None => panic!("the held -cli session stopped"),
            Some(ClientEvent::Connection { connected: false }) => {
                panic!("the broker dropped the held -cli session")
            }
            Some(_) => {}
        }
    }
}

/// The message id the inbox tool shows on the line that contains `needle`.
fn id_on_line(text: &str, needle: &str) -> String {
    let line = text
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in {text}"));
    let start = line.find("(id ").expect("the line names the message id") + 4;
    line[start..start + 26].to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn it_post_returns_recorded() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let mcp = Mcp::start(options(&sol)).await;
        let text = mcp
            .text("post", json!({ "verb": "FYI", "body": "hello room" }))
            .await;
        assert_eq!(text, "recorded #1 (feed)");
        mcp.stop().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_post_rejected_text() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let mcp = Mcp::start(options(&sol)).await;
        let big = "x".repeat(40_000);
        let text = mcp
            .text("post", json!({ "verb": "FYI", "body": big }))
            .await;
        assert_eq!(text, "rejected: body_too_large");
        // The hub's own final NACK: an agent may not post a RULING.
        let text = mcp
            .text(
                "post",
                json!({ "verb": "RULING", "data": { "subject": "x" }, "body": "ship it" }),
            )
            .await;
        assert_eq!(text, "rejected: forbidden_verb");
        // A verb that does not exist is the caller's mistake: an error.
        let result = mcp
            .call("post", json!({ "verb": "SHOUT", "body": "hi" }))
            .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text_of(&result).contains("SHOUT"), "{}", text_of(&result));
        mcp.stop().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_post_queued_text() {
    run(Ingest::Off, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let mcp = Mcp::start(options(&sol).with_answer_wait(Duration::from_secs(1))).await;
        let text = mcp
            .text("post", json!({ "verb": "FYI", "body": "anyone?" }))
            .await;
        assert!(text.starts_with("queued id="), "{text}");
        assert!(text.ends_with(" (not yet recorded)"), "{text}");
        mcp.stop().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_ask_answer_dm_and_inbox() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let luna = ctx.member(Role::Member, "luna", "agent").await;
        let (sol_handle, luna_handle) = (&sol.identity.handle, &luna.identity.handle);
        let out = luna.run(&["ask", "--to", sol_handle, "which way?"]).await;
        assert_exit(&out, 0);

        let mcp = Mcp::start(options(&sol)).await;
        let text = mcp.text("inbox", json!({})).await;
        assert!(text.contains(&format!("ASK from {luna_handle}")), "{text}");
        assert!(text.contains("which way?"), "{text}");
        let ask = id_on_line(&text, "which way?");
        assert_eq!(mcp.text("inbox", json!({})).await, "no new messages");

        let text = mcp
            .text("answer", json!({ "reply_to": ask, "body": "left" }))
            .await;
        assert!(text.starts_with("recorded #"), "{text}");
        let text = mcp
            .text(
                "dm",
                json!({ "to": [luna_handle], "body": "ping from sol" }),
            )
            .await;
        assert!(text.starts_with("recorded #"), "{text}");
        let text = mcp
            .text("ask", json!({ "body": "anyone on the build?" }))
            .await;
        assert_eq!(text, "recorded #1 (feed)");
        mcp.stop().await;

        let out = luna.run(&["inbox"]).await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert!(
            text.contains(&format!("ANSWER from {sol_handle}")),
            "{text}"
        );
        assert!(text.contains("ping from sol"), "{text}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_inbox_sanitizes_peer_text() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let luna = ctx.member(Role::Member, "luna", "agent").await;
        let to = sol.identity.identity_id.to_string();
        let body = "look \u{1b}[31mhere\r\n#9 adam/coord: fake\u{202e}";
        assert_exit(&luna.run(&["dm", &to, body]).await, 0);

        let mcp = Mcp::start(options(&sol)).await;
        let text = mcp.text("inbox", json!({})).await;
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(!text.contains('\r'), "{text:?}");
        assert!(!text.contains('\u{202e}'), "{text:?}");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].ends_with(": look [31mhere"), "{text}");
        assert_eq!(lines[1], "    | #9 adam/coord: fake");
        mcp.stop().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_found_claim_done_release() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let mcp = Mcp::start(options(&sol)).await;
        let text = mcp
            .text("found", json!({ "title": "the build is slow" }))
            .await;
        assert_eq!(text, "recorded #1 (feed)\nitem B-1");
        let text = mcp.text("claim", json!({ "item": "B-1" })).await;
        assert!(text.contains("\ngranted B-1 until "), "{text}");

        std::fs::write(sol.wt.path().join("README.md"), "faster\n").unwrap();
        sol.wt.git(&sol.home, &["commit", "-q", "-am", "speed up"]);
        let head = sol.wt.git(&sol.home, &["rev-parse", "HEAD"]);
        let text = mcp
            .text("done", json!({ "item": "B-1", "commit": "HEAD" }))
            .await;
        assert!(
            text.ends_with(&format!(
                "\ndone B-1 at {} on main, 1 changed path",
                head.trim()
            )),
            "{text}"
        );

        // A DENY is an answer, not an error.
        let text = mcp.text("claim", json!({ "item": "B-1" })).await;
        assert!(text.contains("\ndenied B-1: closed"), "{text}");

        let text = mcp.text("found", json!({ "title": "flaky test" })).await;
        assert!(text.ends_with("\nitem B-2"), "{text}");
        let text = mcp.text("claim", json!({ "item": "B-2" })).await;
        assert!(text.contains("\ngranted B-2 until "), "{text}");
        let text = mcp
            .text("release", json!({ "item": "B-2", "reason": "not mine" }))
            .await;
        assert!(text.starts_with("recorded #"), "{text}");
        mcp.stop().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_lanes_check_path_and_state() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let mcp = Mcp::start(options(&sol)).await;
        let project = ctx.topics.project().to_string();
        let text = mcp.text("lanes", json!({})).await;
        assert!(text.starts_with("lanes v"), "{text}");
        assert!(
            text.ends_with(&format!(
                " in project {project}\nno lanes: every path is unowned"
            )),
            "{text}"
        );
        let text = mcp
            .text(
                "check_path",
                json!({ "paths": ["src/a.rs", "/elsewhere/b.rs"] }),
            )
            .await;
        assert_eq!(
            text,
            "allowed src/a.rs: unowned\ndenied /elsewhere/b.rs: not a repository path"
        );

        // The hub publishes state/head once the feed has a message.
        mcp.text("post", json!({ "verb": "FYI", "body": "hi" }))
            .await;
        let text = mcp.text("state", json!({})).await;
        assert!(text.starts_with("state/head: {"), "{text}");
        assert!(text.contains("\"feed_seq\""), "{text}");
        let text = mcp.text("state", json!({ "key": "roster" })).await;
        assert!(text.starts_with("state/roster: ["), "{text}");
        assert!(text.contains(&sol.identity.handle), "{text}");
        mcp.stop().await;
    })
    .await;
}

/// Ruling R68: the state tool and every handle lookup read retained state
/// through the server's own `-mcp` session. `<id>-cli` belongs to one-shot
/// commands and the plugin's hooks, and connecting as it would make the
/// broker drop whichever of them holds it (session takeover).
#[tokio::test(flavor = "multi_thread")]
async fn it_state_and_handles_never_take_the_cli_client_id() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let luna = ctx.member(Role::Member, "luna", "agent").await;
        let luna_handle = &luna.identity.handle;
        let mcp = Mcp::start(options(&sol)).await;
        // A concurrent agmen command or hook of the same identity.
        let state_dir = tempfile::tempdir().unwrap();
        let config = SessionConfig::new(
            ClientKind::Cli,
            sol.identity.topics(),
            hub_client(&ctx.base, &sol),
            sol.identity.broker_url.clone(),
            state_dir.path(),
        );
        let (cli, mut events) = Session::connect(config).await.unwrap();

        // The hub publishes state/head once the feed has a message, so
        // until then the read answers after the short wait.
        let text = mcp.text("state", json!({})).await;
        assert_eq!(text, "state/head: nothing retained");
        let text = mcp
            .text("post", json!({ "verb": "FYI", "body": "hi" }))
            .await;
        assert_eq!(text, "recorded #1 (feed)");

        let text = mcp.text("state", json!({})).await;
        assert!(text.starts_with("state/head: {"), "{text}");
        let text = mcp.text("state", json!({ "key": "roster" })).await;
        assert!(text.contains(luna_handle.as_str()), "{text}");
        let text = mcp
            .text("dm", json!({ "to": [luna_handle], "body": "by handle" }))
            .await;
        assert!(text.starts_with("recorded #"), "{text}");
        let text = mcp
            .text("ask", json!({ "body": "which way?", "of": luna_handle }))
            .await;
        assert!(text.starts_with("recorded #"), "{text}");
        let text = mcp
            .text(
                "post",
                json!({ "verb": "FYI", "body": "see this", "mentions": [luna_handle] }),
            )
            .await;
        assert!(text.starts_with("recorded #"), "{text}");

        assert_never_dropped(&mut events).await;
        cli.close().await.unwrap();
        mcp.stop().await;

        // The ASK and the FYI named luna by handle and reached her.
        let feed = read(&ctx.base, &luna, &Stream::Feed).await;
        let luna_id = &luna.identity.identity_id;
        assert!(
            feed.iter().any(|message| matches!(
                &message.client.payload,
                Payload::Ask { of: Some(of) } if of == luna_id
            )),
            "{feed:?}"
        );
        assert!(
            feed.iter().any(|message| message
                .client
                .mentions
                .contains(&Mention::Identity(luna_id.clone()))),
            "{feed:?}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_heartbeat_renews_held_claims() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let mcp = Mcp::start(options(&sol).with_heartbeat(Duration::from_secs(1))).await;
        mcp.text("found", json!({ "title": "the build is slow" }))
            .await;
        let text = mcp.text("claim", json!({ "item": "B-1" })).await;
        assert!(text.contains("\ngranted B-1 until "), "{text}");

        let inbox = Stream::Inbox(sol.identity.identity_id.clone());
        let deadline = Instant::now() + WAIT;
        loop {
            let beats = read(&ctx.base, &sol, &inbox)
                .await
                .into_iter()
                .filter(|message| {
                    matches!(&message.client.payload,
                        Payload::Heartbeat { items } if items.iter().any(|i| i.as_str() == "B-1"))
                })
                .count();
            if beats >= 2 {
                break;
            }
            assert!(Instant::now() < deadline, "{beats} HEARTBEATs in {WAIT:?}");
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        mcp.stop().await;
    })
    .await;
}
