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
    reason = "the machine harness, shared whole; this file uses a subset of its helpers"
)]
mod machines;

use std::path::Path;
use std::time::Duration;

use agmen_client::{HubClient, IdentityFile, TokenKey};
use agmen_hub::Role;
use agmen_proto::{DeliveryReply, NackReason, Stream};
use machines::daemon::Daemon;
use machines::fake_claude::FakeClaude;
use machines::{Watch, read, run};
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{PubAckReason, SubscribeReasonCode};
use serde_json::json;
use support::{WAIT, assert_exit, env, stdout};

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

#[tokio::test(flavor = "multi_thread")]
async fn it_role_verb_refused_end_to_end() {
    run(|ctx, sessions| async move {
        let owner = ctx.member(Role::Member, "main", "agent").await;
        let observer = ctx.member(Role::Coordinator, "coord", "coordinator").await;
        let daemon = Daemon::start(&owner.home, FakeClaude::new()).await;
        let launched = daemon
            .launch(
                &sessions,
                json!({"project": owner.identity.project, "name": "sol", "root": owner.wt.path(), "role": "engineer"}),
            )
            .await;
        let folder = Path::new(launched["worktree"].as_str().unwrap());
        let identity: IdentityFile = serde_json::from_slice(
            &std::fs::read(folder.join(".agmen/identity.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(identity.role.as_ref().unwrap().as_str(), "engineer");
        let token = owner
            .home
            .tokens()
            .get(&TokenKey::new(&identity.hub_url, &identity.identity_id).unwrap())
            .unwrap()
            .unwrap();
        let hub = HubClient::new(
            identity.hub_url,
            identity.project,
            identity.identity_id.clone(),
            token,
        )
        .unwrap();
        let broker_token = hub.broker_token().await.unwrap();
        let options = mqtt::options(
            &format!("{}-mcp", identity.identity_id),
            identity.identity_id.as_str(),
            &broker_token,
        );
        let mut worker = mqtt::connect_ok(options).await;
        let ack_topic = ctx.topics.ack(&identity.identity_id);
        assert_eq!(
            worker.subscribe(&ack_topic).await,
            SubscribeReasonCode::Success(QoS::AtLeastOnce)
        );
        let mut watch = Watch::start(&sessions, &observer, &[]).await;
        const QUIET: Duration = Duration::from_secs(2);
        tokio::time::timeout(WAIT, watch.messages_until_quiet(QUIET))
            .await
            .expect("the startup feed becomes quiet");

        let id = "01JBX6Q4T7W3C2M8Y9K5N0P1RS";
        let envelope = json!({
            "v": 1, "id": id, "sent_at": jiff::Timestamp::now(),
            "verb": "RULING", "data": {"subject": "ship it"}, "body": "ship it",
        });
        let up = ctx.topics.up(&identity.identity_id);
        assert_eq!(
            worker
                .publish(&up, &serde_json::to_vec(&envelope).unwrap())
                .await,
            PubAckReason::Success
        );
        let ack = worker.recv_on(&ack_topic, WAIT).await.expect("worker ack");
        let reply: DeliveryReply = serde_json::from_slice(&ack.payload).unwrap();
        assert!(
            matches!(&reply, DeliveryReply::Nack { id: rejected, reason: NackReason::ForbiddenVerb, retryable: false, retry_after_ms: None } if rejected.as_str() == id),
            "{reply:?}"
        );
        let delivered = tokio::time::timeout(WAIT, watch.messages_until_quiet(QUIET))
            .await
            .expect("the rejected publish leaves the watch quiet");
        assert!(delivered.is_empty(), "{delivered:?}");
        let feed = read(&observer, &Stream::Feed).await;
        assert!(feed.iter().all(|message| message.client.id.as_str() != id));

        // A permitted verb from the same worker reaches this same watch.
        let control_id = "01JBX6Q4T7W3C2M8Y9K5N0P1RT";
        let control = json!({
            "v": 1, "id": control_id, "sent_at": jiff::Timestamp::now(),
            "verb": "FYI", "data": {}, "body": "worker control",
        });
        assert_eq!(
            worker.publish(&up, &serde_json::to_vec(&control).unwrap()).await,
            PubAckReason::Success
        );
        let ack = worker.recv_on(&ack_topic, WAIT).await.expect("control ack");
        let reply: DeliveryReply = serde_json::from_slice(&ack.payload).unwrap();
        assert!(matches!(&reply, DeliveryReply::Ack { id, .. } if id.as_str() == control_id), "{reply:?}");
        let message = tokio::time::timeout(
            WAIT,
            watch.message(|message| message.client.id.as_str() == control_id),
        )
        .await
        .expect("the permitted worker FYI reaches the watch");
        assert_eq!(message.from, identity.identity_id);
        worker.disconnect().await;
        watch.stop().await;
        daemon.stop().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_merged_branch_pruned() {
    run(|ctx, sessions| async move {
        let owner = ctx.member(Role::Member, "main", "agent").await;
        let remote = tempfile::tempdir().unwrap();
        let remote_path = remote.path().to_str().unwrap();
        owner.wt.git(&owner.home, &["clone", "--bare", ".", remote_path]);
        owner.wt.git(&owner.home, &["remote", "add", "origin", remote_path]);
        let daemon = Daemon::start(&owner.home, FakeClaude::new()).await;
        let mut workers = Vec::new();
        for name in ["merged", "dirty"] {
            let launched = daemon
                .launch(
                    &sessions,
                    json!({"project": owner.identity.project, "name": name, "root": owner.wt.path(), "role": "engineer"}),
                )
                .await;
            let identity = launched["identity_id"].as_str().unwrap().parse().unwrap();
            let removed = daemon.remove(&sessions, &identity).await;
            assert_eq!(removed["removed"], true);
            assert_eq!(removed["kept"]["folder"], launched["worktree"]);
            workers.push(launched);
        }
        let clean = workers[0]["worktree"].as_str().unwrap();
        let clean_branch = workers[0]["branch"].as_str().unwrap();
        let dirty = workers[1]["worktree"].as_str().unwrap();
        let dirty_branch = workers[1]["branch"].as_str().unwrap();
        std::fs::write(Path::new(clean).join("finished.txt"), "finished work\n").unwrap();
        owner.wt.git(&owner.home, &["-C", clean, "add", "finished.txt"]);
        owner.wt.git(&owner.home, &["-C", clean, "commit", "-q", "-m", "finish worker task"]);
        let clean_tip = owner.wt.git(&owner.home, &["rev-parse", clean_branch]);
        assert_ne!(clean_tip, owner.wt.git(&owner.home, &["rev-parse", "main"]));
        std::fs::write(Path::new(dirty).join("unfinished.txt"), "keep this work\n").unwrap();
        let dirty_status = owner.wt.git(&owner.home, &["-C", dirty, "status", "--porcelain=v1"]);
        assert!(dirty_status.contains("?? unfinished.txt"), "{dirty_status}");

        // Fast-forward the bare remote's main to the worker, then fetch it.
        owner.wt.git(&owner.home, &["push", "origin", &format!("{clean_branch}:main")]);
        owner.wt.git(&owner.home, &["fetch", "origin", "main"]);
        assert_eq!(
            owner.wt.git(&owner.home, &["--git-dir", remote_path, "rev-parse", "main"]),
            clean_tip
        );
        assert_eq!(owner.wt.git(&owner.home, &["rev-parse", "origin/main"]), clean_tip);
        let out = daemon
            .run(&owner.home, &owner.wt.path(), &["worktree", "prune", "--base", "origin/main"])
            .await;
        assert_exit(&out, 0);
        let output = stdout(&out);
        assert!(output.lines().any(|line| line == format!("removed {clean_branch}")), "{output}");
        assert!(output.lines().any(|line| line == format!("kept {dirty_branch}: uncommitted")), "{output}");
        assert!(!Path::new(clean).exists());
        assert!(owner.wt.git(&owner.home, &["branch", "--list", clean_branch]).is_empty());
        assert!(Path::new(dirty).exists());
        assert_eq!(
            owner.wt.git(&owner.home, &["-C", dirty, "status", "--porcelain=v1"]),
            dirty_status
        );
        assert_eq!(std::fs::read_to_string(Path::new(dirty).join("unfinished.txt")).unwrap(), "keep this work\n");
        assert_eq!(
            owner.wt.git(&owner.home, &["show", &format!("refs/wip/{dirty_branch}:unfinished.txt")]),
            "keep this work\n"
        );
        assert!(!owner.wt.git(&owner.home, &["branch", "--list", dirty_branch]).is_empty());
        daemon.stop().await;
    })
    .await;
}
