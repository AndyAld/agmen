//! End-to-end tests for roles and the listener. This file starts with the
//! harness smoke test: two agmend processes, a fake claude, and one message
//! delivered into a managed agent's batch.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

#[path = "../../../crates/agmen-cli/tests/support/mod.rs"]
mod support;

#[path = "../../../crates/agmen-hub/tests/support/mqtt.rs"]
#[expect(
    dead_code,
    reason = "the hub's MQTT test client, shared whole; later tests in this file use part of it"
)]
mod mqtt;

#[expect(
    dead_code,
    reason = "the machine harness, shared whole; this file uses Daemon and run"
)]
mod machines;

use agmen_hub::Role;
use machines::daemon::Daemon;
use machines::fake_claude::FakeClaude;
use machines::run;
use serde_json::json;
use support::{WAIT, assert_exit, env};

/// Two signed-in humans, two agmend, one managed agent. The other human's
/// mention reaches that agent's fake claude, which reports a cost of 0.01.
#[tokio::test(flavor = "multi_thread")]
async fn it_two_agmend_launch_and_deliver() {
    run(|ctx, sessions| async move {
        let a = ctx.member(Role::Member, "ada", "agent").await;
        let b = ctx.member(Role::Member, "bee", "agent").await;
        let daemon_a = Daemon::start(&a.home, FakeClaude::new()).await;
        let daemon_b = Daemon::start(&b.home, FakeClaude::new()).await;

        let launched = daemon_a
            .launch(
                &sessions,
                json!({
                    "project": a.identity.project,
                    "name": "sol",
                    "root": a.wt.path(),
                }),
            )
            .await;
        let agent_id = launched["identity_id"]
            .as_str()
            .unwrap_or_else(|| panic!("agent_launch returned {launched}"))
            .to_owned();
        let mut client = daemon_a.client().await;

        let body = "please look at the socket";
        assert_exit(
            &b.run(&["post", "FYI", body, "--mention", &agent_id]).await,
            0,
        );

        let deadline = tokio::time::Instant::now() + WAIT;
        let mut delivered = false;
        let mut last_list = String::new();
        while tokio::time::Instant::now() < deadline {
            let runs = daemon_a.claude().runs();
            let recorded = daemon_a.claude().recorded();
            let listed = client
                .call("agent_list", json!({"project": a.identity.project}))
                .await;
            last_list = match &listed {
                Ok(list) => list.to_string(),
                Err(error) => format!("{error:?}"),
            };
            let spent_ok = listed.as_ref().ok().is_some_and(|list| {
                list["agents"].as_array().is_some_and(|agents| {
                    agents.iter().any(|agent| {
                        agent["identity"].as_str() == Some(agent_id.as_str())
                            && agent["spent_usd"].as_str() == Some("0.010000")
                    })
                })
            });
            if runs == 1 && recorded.contains(body) && spent_ok {
                delivered = true;
                break;
            }
            if listed.is_err() {
                client = daemon_a.client().await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            delivered,
            "runs {}\nlist: {last_list}\nrecord:\n{}",
            daemon_a.claude().runs(),
            daemon_a.claude().recorded()
        );

        daemon_a.stop().await;
        daemon_b.stop().await;
    })
    .await;
}
