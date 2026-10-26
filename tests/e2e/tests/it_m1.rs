//! The MVP's end-to-end exit tests: two "machines", each a temp home with
//! its own git worktree, run the real `agmen` binary against an in-process
//! hub on its own database and the shared dev stack (EMQX, Postgres,
//! Toxiproxy and the HAProxy WSS listener). Machine 1 is andy's agent
//! `<andy>/sol`, machine 2 adam's coordinator `<adam>/coord`; the hub
//! hands out handles, so each test names them through the identity files.
//!
//! The harness is agmen-cli's test support, shared as it is, plus the
//! hub's raw MQTT test client for the spoofing test; `machines` adds child
//! processes, broker addresses and the plugin contract.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

#[path = "../../../crates/agmen-cli/tests/support/mod.rs"]
mod support;

#[path = "../../../crates/agmen-hub/tests/support/mqtt.rs"]
#[expect(
    dead_code,
    reason = "the hub's MQTT test client, shared whole; these tests use part of it"
)]
mod mqtt;

mod machines;

use std::time::{Duration, Instant};

use agmen_client::{ClientEvent, ClientKind, Session, SessionConfig};
use agmen_hub::Role;
use agmen_proto::{DenyReason, Payload, StampedEnvelope, Stream};
use machines::contract::{CheckPathRun, CliOutput, Replace, check_or_update};
use machines::{
    StdioMcp, Watch, emqx_client, hub_client, read, run, run_with, set_proxy, trust_dev_ca,
    use_broker,
};
use rumqttc::v5::mqttbytes::v5::PubAckReason;
use serde_json::json;
use support::{Ctx, Home, Member, WAIT, Worktree, assert_exit, env, new_id, stderr, stdout};

/// The two machines of most tests: adam's coordinator and andy's agent.
async fn machines(ctx: &Ctx) -> (Member, Member) {
    let coord = ctx.member(Role::Coordinator, "coord", "coordinator").await;
    let sol = ctx.member(Role::Member, "sol", "agent").await;
    (coord, sol)
}

/// Whether `message` came from `who` with `body`.
fn said(message: &StampedEnvelope, who: &Member, body: &str) -> bool {
    message.from == who.identity.identity_id && message.client.body.as_str() == body
}

/// The item key `agmen found` printed (`item B-1`).
fn found_item(out: &std::process::Output) -> String {
    assert_exit(out, 0);
    stdout(out)
        .lines()
        .find_map(|line| line.strip_prefix("item "))
        .unwrap_or_else(|| panic!("no item key in {:?}", stdout(out)))
        .to_owned()
}

/// A message id for a hand-built envelope: a ULID from a fresh hub-style
/// id, upper-cased, with the letters Crockford base32 leaves out mapped to
/// digits.
fn message_id() -> String {
    let tail: String = new_id()
        .to_ascii_uppercase()
        .chars()
        .map(|c| match c {
            'I' | 'L' => '1',
            'O' => '0',
            'U' => 'V',
            c => c,
        })
        .collect();
    format!("01KA00000{tail}X")
}

/// A client envelope for an FYI with `body`, as a raw MQTT client would
/// publish it.
fn fyi(body: &str) -> (String, Vec<u8>) {
    let id = message_id();
    let env = json!({
        "v": 1,
        "id": id,
        "body": body,
        "sent_at": jiff::Timestamp::now(),
        "verb": "FYI",
        "data": {},
    });
    (id, serde_json::to_vec(&env).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn it_two_machines_exchange() {
    run(|ctx, sessions| async move {
        // adam, an admin, joins machine 2 as coordinator and invites andy.
        let coord = ctx.member(Role::Admin, "coord", "coordinator").await;
        let out = coord.run(&["invite"]).await;
        assert_exit(&out, 0);
        let code = stdout(&out)
            .lines()
            .find_map(|line| line.trim().strip_prefix("agmen join "))
            .expect("a ready-to-paste join line")
            .to_owned();

        // andy, so far in another project only, joins machine 1 with it.
        let elsewhere = ctx.hub.seed_project().await.unwrap();
        let andy = ctx.hub.seed_human(&elsewhere, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &andy);
        let wt = Worktree::new(&home);
        let out = home
            .run(&wt.path(), &["join", &code, "--handle", "sol"])
            .await;
        assert_exit(&out, 0);
        let identity = wt.identity();
        let sol = Member {
            human: andy,
            home,
            wt,
            identity,
        };
        assert_eq!(sol.identity.handle, format!("{}/sol", sol.human.handle));

        let mut watch = Watch::start(&sessions, &coord, &[]).await;
        let body = "sol here: the build is green";
        assert_exit(&sol.run(&["post", "FYI", body]).await, 0);
        let fyi = watch.message(|m| said(m, &sol, body)).await;
        assert!(matches!(fyi.client.payload, Payload::Fyi {}));
        assert_eq!(fyi.from_handle, sol.identity.handle);
        assert_eq!(fyi.stream, Stream::Feed);

        // adam answers andy's agent directly, by handle.
        let dm = "adam here: take B-1 next";
        let out = coord.run(&["dm", &sol.identity.handle, dm]).await;
        assert_exit(&out, 0);
        let out = sol.run(&["inbox", "--json"]).await;
        assert_exit(&out, 0);
        let inbox: Vec<StampedEnvelope> = stdout(&out)
            .lines()
            .filter_map(|line| match serde_json::from_str(line).unwrap() {
                ClientEvent::Message { message } => Some(*message),
                _ => None,
            })
            .collect();
        assert_eq!(inbox.len(), 1, "{inbox:?}");
        assert!(said(&inbox[0], &coord, dm), "{inbox:?}");
        assert!(matches!(inbox[0].client.payload, Payload::Dm {}));
        assert_eq!(inbox[0].from_handle, coord.identity.handle);

        // The feed has the FYI and not the DM.
        let feed = read(&sol, &Stream::Feed).await;
        assert!(feed.iter().any(|m| m.client.id == fyi.client.id));
        assert!(
            feed.iter().all(|m| m.client.body.as_str() != dm),
            "{feed:?}"
        );
        watch.stop().await;
    })
    .await;
}

/// Toxiproxy's link is shared; .config/nextest.toml runs this test one at
/// a time with the others that cut it.
#[tokio::test(flavor = "multi_thread")]
async fn it_network_kill_during_done() {
    run(|ctx, sessions| async move {
        // A failed run of a test that cuts the link can leave it off.
        set_proxy(true).await;
        let (coord, mut sol) = machines(&ctx).await;
        // Only machine 1 reaches the broker through the proxy.
        use_broker(&mut sol, &env("AGMEN_IT_MQTT_PROXIED"));
        let mut watch = Watch::start(&sessions, &coord, &[]).await;
        // sol is an agent harness on `agmen mcp`, whose session holds the
        // outbox while the network is gone; a one-shot `agmen post` could
        // not connect to queue anything.
        let mcp = StdioMcp::start(&sessions, &sol).await;
        // Through the proxy before the cut: coord gets it.
        mcp.call("post", json!({ "verb": "FYI", "body": "sol online" }))
            .await;
        watch.message(|m| said(m, &sol, "sol online")).await;

        set_proxy(false).await;
        let cut_at = Instant::now();
        // Five reports at once, each answered `queued` after its wait.
        let posting = tokio::spawn(async move {
            let post = |n: u32| {
                let body = format!("done-like {n}");
                let args = json!({ "verb": "STATUS", "body": body, "data": { "summary": body } });
                mcp.call("post", args)
            };
            let answers = tokio::join!(post(1), post(2), post(3), post(4), post(5));
            (mcp, answers)
        });
        tokio::time::sleep_until((cut_at + Duration::from_secs(20)).into()).await;
        set_proxy(true).await;
        let (mcp, answers) = posting.await.unwrap();
        // `agmen mcp` answers after 5 s (ANSWER_WAIT) and the link is off
        // for 20 s, so a report the hub recorded did not go through the cut.
        for (text, _) in [answers.0, answers.1, answers.2, answers.3, answers.4] {
            assert!(text.starts_with("queued id="), "{text}");
        }

        // coord sees each report once, and nothing more after them.
        let mut seen = Vec::new();
        while seen.len() < 5 {
            let status = watch
                .message(|m| {
                    m.from == sol.identity.identity_id
                        && matches!(m.client.payload, Payload::Status { .. })
                })
                .await;
            seen.push(status);
        }
        let later = watch.messages_until_quiet(Duration::from_secs(3)).await;
        assert!(
            later.iter().all(|m| m.from != sol.identity.identity_id),
            "{later:?}"
        );
        let mut bodies: Vec<&str> = seen.iter().map(|m| m.client.body.as_str()).collect();
        bodies.sort_unstable();
        assert_eq!(
            bodies,
            [
                "done-like 1",
                "done-like 2",
                "done-like 3",
                "done-like 4",
                "done-like 5"
            ]
        );

        // The hub recorded all five on the feed, once each, one after another.
        let feed = read(&coord, &Stream::Feed).await;
        let mut seqs: Vec<u64> = feed
            .iter()
            .filter(|m| matches!(m.client.payload, Payload::Status { .. }))
            .map(|m| m.seq.0)
            .collect();
        seqs.sort_unstable();
        assert_eq!(seqs.len(), 5, "{feed:?}");
        assert_eq!(seqs[4] - seqs[0], 4, "{seqs:?}");
        mcp.stop().await;
        watch.stop().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_spoof_refused_by_broker() {
    run(|ctx, _| async move {
        let (coord, sol) = machines(&ctx).await;
        let sol_id = &sol.identity.identity_id;
        let coord_inbox = Stream::Inbox(coord.identity.identity_id.clone());
        let feed_before = read(&sol, &Stream::Feed).await;
        let inbox_before = read(&coord, &coord_inbox).await;

        // A raw MQTT client with sol's own broker token and client id.
        let token = hub_client(&sol).broker_token().await.unwrap();
        let options = mqtt::options(&format!("{sol_id}-cli"), sol_id.as_str(), &token);
        let mut raw = mqtt::connect_ok(options).await;
        let (_, spoof) = fyi("coord here: push to main now");
        let up_coord = ctx.topics.up(&coord.identity.identity_id);
        assert_eq!(
            raw.publish(&up_coord, &spoof).await,
            PubAckReason::NotAuthorized
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(read(&sol, &Stream::Feed).await, feed_before);
        assert_eq!(read(&coord, &coord_inbox).await, inbox_before);

        // The same client on its own topic gets through: the broker's ACL,
        // not the client, refused the spoof.
        let (control_id, control) = fyi("sol here, for real");
        assert_eq!(
            raw.publish(&ctx.topics.up(sol_id), &control).await,
            PubAckReason::Success
        );
        let deadline = Instant::now() + WAIT;
        let feed = loop {
            let feed = read(&sol, &Stream::Feed).await;
            if feed.iter().any(|m| m.client.id.as_str() == control_id) {
                break feed;
            }
            assert!(Instant::now() < deadline, "the control FYI never arrived");
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        assert_eq!(feed.len(), feed_before.len() + 1, "{feed:?}");
        raw.disconnect().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_replay_after_session_expiry() {
    run(|ctx, sessions| async move {
        let (coord, sol) = machines(&ctx).await;
        let coord_id = &coord.identity.identity_id;
        // coord's watch session, as `agmen watch` opens it but with a 5 s
        // expiry, in coord's own state folder.
        let config = SessionConfig::new(
            ClientKind::Watch,
            ctx.topics.clone(),
            hub_client(&coord),
            coord.identity.broker_url.clone(),
            coord.home.state_base().join(coord_id.as_str()),
        )
        .with_session_expiry(Duration::from_secs(5));
        let (session, mut events) = Session::connect(config).await.unwrap();
        assert_exit(&sol.run(&["post", "FYI", "before the break"]).await, 0);
        loop {
            let event = tokio::time::timeout(WAIT, events.recv())
                .await
                .expect("the FYI reaches coord")
                .expect("the session runs");
            if let ClientEvent::Message { message } = event
                && said(&message, &sol, "before the break")
            {
                break;
            }
        }
        session.close().await.unwrap();
        let offline_at = Instant::now();

        let sent: Vec<String> = (1..=20).map(|n| format!("while away {n}")).collect();
        for body in &sent {
            assert_exit(&sol.run(&["post", "FYI", body]).await, 0);
        }
        tokio::time::sleep_until((offline_at + Duration::from_secs(10)).into()).await;
        assert_eq!(
            emqx_client(&format!("{coord_id}-watch")).await,
            None,
            "the broker still holds coord's session past its expiry"
        );

        // coord's `agmen watch` comes back: nothing queued at the broker, so
        // all 20 arrive by replay from its cursor, in order, once each.
        let mut watch = Watch::start(&sessions, &coord, &[]).await;
        let mut got = Vec::new();
        while got.len() < sent.len() {
            let message = watch.message(|m| m.from == sol.identity.identity_id).await;
            got.push(message.client.body.as_str().to_owned());
        }
        assert_eq!(got, sent);
        let later = watch.messages_until_quiet(Duration::from_secs(2)).await;
        assert!(
            later.iter().all(|m| m.from != sol.identity.identity_id),
            "{later:?}"
        );
        watch.stop().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_claim_race_two_machines() {
    run(|ctx, _| async move {
        let (coord, sol) = machines(&ctx).await;
        let item = found_item(&coord.run(&["found", "the release checklist"]).await);
        assert_eq!(item, "B-1");

        let (sol_out, coord_out) =
            tokio::join!(sol.run(&["claim", "B-1"]), coord.run(&["claim", "B-1"]));
        let outcomes = [(&sol, &sol_out), (&coord, &coord_out)];
        let granted: Vec<_> = outcomes
            .iter()
            .filter(|(_, out)| out.status.code() == Some(0))
            .collect();
        let denied: Vec<_> = outcomes
            .iter()
            .filter(|(_, out)| out.status.code() == Some(4))
            .collect();
        assert_eq!(
            (granted.len(), denied.len()),
            (1, 1),
            "sol: {sol_out:?}\ncoord: {coord_out:?}"
        );
        let (winner, won) = granted[0];
        let (loser, lost) = denied[0];
        assert!(stdout(won).contains("granted B-1"), "{}", stdout(won));
        assert!(
            stderr(lost).contains("denied B-1: held_by"),
            "{}",
            stderr(lost)
        );

        // The hub's answers sit in each claimant's inbox.
        let won_inbox = read(
            winner,
            &Stream::Inbox(winner.identity.identity_id.clone()),
        )
        .await;
        assert_eq!(
            won_inbox
                .iter()
                .filter(|m| matches!(&m.client.payload, Payload::Grant { item, .. } if item.as_str() == "B-1"))
                .count(),
            1,
            "{won_inbox:?}"
        );
        let lost_inbox = read(
            loser,
            &Stream::Inbox(loser.identity.identity_id.clone()),
        )
        .await;
        let denies: Vec<_> = lost_inbox
            .iter()
            .filter_map(|m| match &m.client.payload {
                Payload::Deny { item, reason } => Some((item.as_str(), *reason)),
                _ => None,
            })
            .collect();
        assert_eq!(denies, [("B-1", DenyReason::HeldBy)]);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_done_reaches_coordinator() {
    run(|ctx, sessions| async move {
        let (coord, sol) = machines(&ctx).await;
        let mut watch = Watch::start(&sessions, &coord, &[]).await;
        let item = found_item(&sol.run(&["found", "the build is slow"]).await);
        let out = sol.run(&["claim", &item]).await;
        assert_exit(&out, 0);
        assert!(
            stdout(&out).contains(&format!("granted {item}")),
            "{}",
            stdout(&out)
        );

        std::fs::create_dir_all(sol.wt.path().join("src")).unwrap();
        std::fs::write(sol.wt.path().join("src/fast.rs"), "fn main() {}\n").unwrap();
        std::fs::write(sol.wt.path().join("README.md"), "faster\n").unwrap();
        sol.wt.git(&sol.home, &["add", "-A"]);
        sol.wt
            .git(&sol.home, &["commit", "-q", "-m", "speed up the build"]);
        let head = sol.wt.git(&sol.home, &["rev-parse", "HEAD"]);
        assert_exit(&sol.run(&["done", &item, "--commit", "HEAD"]).await, 0);

        let done = watch
            .message(|m| matches!(m.client.payload, Payload::Done { .. }))
            .await;
        assert_eq!(done.from, sol.identity.identity_id);
        assert_eq!(done.from_handle, sol.identity.handle);
        let Payload::Done {
            item: done_item,
            branch,
            commit,
            changes,
            ..
        } = &done.client.payload
        else {
            panic!("{done:?} is a DONE");
        };
        assert_eq!(done_item.as_str(), item);
        assert_eq!(branch, "main");
        assert_eq!(commit.as_str(), head.trim());
        let changes: Vec<(&str, &str)> = changes
            .iter()
            .map(|change| (change.status.as_str(), change.path.as_str()))
            .collect();
        assert_eq!(changes, [("M", "README.md"), ("A", "src/fast.rs")]);
        watch.stop().await;
    })
    .await;
}

/// `agmen mcp` driven over stdio by a plain MCP client, as an agent
/// harness other than Claude Code would run it.
#[tokio::test(flavor = "multi_thread")]
async fn it_mcp_over_stdio_reaches_the_other_machine() {
    run(|ctx, sessions| async move {
        let (coord, sol) = machines(&ctx).await;
        let mut watch = Watch::start(&sessions, &coord, &[]).await;
        let mcp = StdioMcp::start(&sessions, &sol).await;
        let body = "posted from an MCP client over stdio";
        let (text, is_error) = mcp
            .call("post", json!({ "verb": "FYI", "body": body }))
            .await;
        assert!(!is_error && text.starts_with("recorded #"), "{text}");
        let fyi = watch.message(|m| said(m, &sol, body)).await;
        assert_eq!(fyi.from_handle, sol.identity.handle);

        // And back: coord's DM shows in the agent's inbox tool.
        let dm = "seen it, thanks";
        assert_exit(&coord.run(&["dm", &sol.identity.handle, dm]).await, 0);
        let (text, is_error) = mcp.call("inbox", json!({})).await;
        assert!(!is_error, "{text}");
        assert!(
            text.contains(dm) && text.contains(&coord.identity.handle),
            "{text}"
        );
        mcp.stop().await;
        watch.stop().await;
    })
    .await;
}

/// Both machines reach the broker over WSS through the dev stack's HAProxy
/// (`AGMEN_IT_MQTT_WSS`), trusting its self-signed CA through
/// `SSL_CERT_FILE` (see `machines::trust_dev_ca`).
#[tokio::test(flavor = "multi_thread")]
async fn it_wss_exchange() {
    run(|ctx, sessions| async move {
        let (mut coord, mut sol) = machines(&ctx).await;
        let wss = env("AGMEN_IT_MQTT_WSS");
        use_broker(&mut coord, &wss);
        use_broker(&mut sol, &wss);
        let tls = trust_dev_ca();

        let mut watch = Watch::start(&sessions, &coord, &tls).await;
        let client = emqx_client(&format!("{}-watch", coord.identity.identity_id))
            .await
            .expect("EMQX lists coord's watch");
        assert_eq!(client["listener"], "ws:default", "{client}");

        let body = "over wss";
        assert_exit(&run_with(&sol, &["post", "FYI", body], &tls).await, 0);
        let fyi = watch.message(|m| said(m, &sol, body)).await;
        assert_eq!(fyi.from_handle, sol.identity.handle);

        // Without the dev CA the same machine cannot reach the broker.
        let out = run_with(&sol, &["post", "FYI", "untrusted", "--wait", "1"], &[]).await;
        assert_exit(&out, 1);
        watch.stop().await;
    })
    .await;
}

/// The contract test between the CLI and the Claude Code plugin; see
/// `machines::contract`.
#[tokio::test(flavor = "multi_thread")]
async fn it_plugin_contract_matches_the_cli() {
    run(|ctx, sessions| async move {
        let (coord, sol) = machines(&ctx).await;
        let lanes = coord.wt.path().join("lanes.toml");
        std::fs::write(
            &lanes,
            "[[lanes]]\nname = \"L-UI\"\nglobs = [\"/ui/\"]\n\n[[lanes]]\nname = \"L-CORE\"\nglobs = [\"/src/\"]\n",
        )
        .unwrap();
        assert_exit(&coord.run(&["lanes", "set", lanes.to_str().unwrap()]).await, 0);
        let ui = found_item(&coord.run(&["found", "contract ui", "--lane", "L-UI"]).await);
        assert_exit(&coord.run(&["claim", &ui]).await, 0);
        let core = found_item(&sol.run(&["found", "contract core", "--lane", "L-CORE"]).await);
        assert_exit(&sol.run(&["claim", &core]).await, 0);

        // The plugin runs sol's watch; coord DMs sol about its item and
        // mentions sol on the feed.
        let mut watch = Watch::start(&sessions, &sol, &[]).await;
        let handle = sol.identity.handle.as_str();
        let dm = "look at it when you can";
        let out = coord
            .run(&["post", "DM", dm, "--to", handle, "--item", &core])
            .await;
        assert_exit(&out, 0);
        let fyi = "heads up on the build";
        let out = coord.run(&["post", "FYI", fyi, "--mention", handle]).await;
        assert_exit(&out, 0);
        watch.message(|m| said(m, &coord, fyi)).await;
        watch.messages_until_quiet(Duration::from_secs(1)).await;
        let printed = watch.stop().await;

        let root = sol.wt.path();
        let mut check_path = Vec::new();
        for path in ["ui/a.ts", "src/ok.ts"] {
            let path = root.join(path).to_str().unwrap().to_owned();
            let out = sol.run(&["check-path", "--json", "--", &path]).await;
            assert_exit(&out, 0);
            check_path.push((path, stdout(&out)));
        }

        let replace = Replace::default()
            .with(root.to_str().unwrap(), "/work")
            .with(&sol.identity.handle, "andy/sol")
            .with(&coord.identity.handle, "adam/coord")
            .with(sol.identity.identity_id.as_str(), "andysol01")
            .with(coord.identity.identity_id.as_str(), "adamcoord01")
            .with(sol.human.id.as_str(), "andyhuman01")
            .with(coord.human.id.as_str(), "adamhuman01");
        let fresh = CliOutput {
            watch: printed.iter().map(|line| replace.apply(line)).collect(),
            check_path: check_path
                .iter()
                .map(|(path, out)| CheckPathRun {
                    paths: vec![replace.apply(path)],
                    stdout: replace.apply(out),
                })
                .collect(),
        };
        check_or_update(&fresh);
    })
    .await;
}
