//! The `agmen` binary against an in-process hub and the dev EMQX: login,
//! invites and joining, posting with its exit codes, watch, inbox, claims
//! and DONE, lanes and check-path, handle lookup, and leaving.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agmen_client::{ClientEvent, HubClient, TokenKey, find_identity};
use agmen_hub::Role;
use agmen_proto::{ItemKey, Payload, StampedEnvelope, Stream};
use support::{Home, Ingest, Member, WAIT, Worktree, assert_exit, run, stderr, stdout};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child, ChildStdout};

// shortcut: Ctx exposes no per-test database handle, so these join tests check
// files only. Add hub-row assertions when the harness exposes its pool.
#[tokio::test(flavor = "multi_thread")]
async fn it_join_with_role_mints_spec() {
    run(Ingest::On, |ctx| async move {
        let human = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &human);
        let wt = Worktree::new(&home);
        let out = home
            .run(
                &wt.path(),
                &[
                    "join",
                    "--project",
                    ctx.topics.project().as_str(),
                    "--handle",
                    "rev",
                    "--role",
                    "reviewer",
                ],
            )
            .await;
        assert_exit(&out, 0);
        let file: serde_json::Value =
            serde_json::from_slice(&std::fs::read(wt.path().join(".agmen/identity.json")).unwrap())
                .unwrap();
        assert_eq!(file["role"], "reviewer");
        let sha = file["role_sha256"].as_str().unwrap();
        assert_eq!(sha.len(), 64);
        assert!(
            sha.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        let verbs: std::collections::BTreeSet<String> =
            serde_json::from_value(file["verbs"].clone()).unwrap();
        assert_eq!(
            verbs,
            [
                "HELLO", "BYE", "FYI", "ASK", "ANSWER", "ACK", "STATUS", "DM", "ALERT", "FOUND",
                "REVIEW"
            ]
            .map(str::to_owned)
            .into_iter()
            .collect()
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_join_with_coordinator_role_needs_membership() {
    run(Ingest::On, |ctx| async move {
        let human = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &human);
        let wt = Worktree::new(&home);
        let out = home
            .run(
                &wt.path(),
                &[
                    "join",
                    "--project",
                    ctx.topics.project().as_str(),
                    "--handle",
                    "coord",
                    "--role",
                    "coordinator",
                ],
            )
            .await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains(
                "a coordinator identity needs the coordinator or admin role in the project"
            )
        );
        assert!(!wt.path().join(".agmen/identity.json").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_join_unknown_role_fails_before_minting() {
    run(Ingest::On, |ctx| async move {
        let human = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &human);
        let wt = Worktree::new(&home);
        let out = home
            .run(
                &wt.path(),
                &[
                    "join",
                    "--project",
                    ctx.topics.project().as_str(),
                    "--handle",
                    "rev",
                    "--role",
                    "nope",
                ],
            )
            .await;
        assert_exit(&out, 2);
        assert!(stderr(&out).contains("available roles:"));
        assert!(!wt.path().join(".agmen/identity.json").exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_join_without_role_is_unchanged() {
    run(Ingest::On, |ctx| async move {
        let member = ctx.member(Role::Member, "plain", "agent").await;
        let file: serde_json::Value = serde_json::from_slice(
            &std::fs::read(member.wt.path().join(".agmen/identity.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(file.as_object().unwrap().len(), 7);
        for key in ["role", "role_sha256", "verbs"] {
            assert!(file.get(key).is_none());
        }
    })
    .await;
}

/// Starts `cmd` with stdout piped and stderr in the test output.
fn spawn(cmd: Command) -> Child {
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    cmd.spawn().unwrap()
}

/// A child's stdout, line by line.
struct Lines(tokio::io::Lines<BufReader<ChildStdout>>);

impl Lines {
    fn new(child: &mut Child) -> Self {
        Self(BufReader::new(child.stdout.take().unwrap()).lines())
    }

    /// The next line, or `None` at the end; panics after [`WAIT`].
    async fn next(&mut self) -> Option<String> {
        tokio::time::timeout(WAIT, self.0.next_line())
            .await
            .expect("no line in time")
            .unwrap()
    }
}

/// The feed as `who` reads it through the hub's replay API.
async fn feed(ctx_base: &url::Url, who: &Member) -> Vec<StampedEnvelope> {
    stream(ctx_base, who, &Stream::Feed).await
}

/// `who`'s first page of `stream`.
async fn stream(ctx_base: &url::Url, who: &Member, stream: &Stream) -> Vec<StampedEnvelope> {
    let token = who
        .home
        .tokens()
        .get(&TokenKey::new(ctx_base, &who.identity.identity_id).unwrap())
        .unwrap()
        .expect("join stored the identity token");
    let hub = HubClient::new(
        ctx_base.clone(),
        who.identity.project.clone(),
        who.identity.identity_id.clone(),
        token,
    )
    .unwrap();
    hub.page(stream, 0).await.unwrap().messages
}

#[tokio::test(flavor = "multi_thread")]
async fn it_login_device_flow() {
    run(Ingest::On, |ctx| async move {
        let human = ctx.hub.seed_human(&ctx.topics, Role::Admin).await.unwrap();
        let home = Home::new();
        let base = ctx.base.to_string();
        let mut child = spawn(home.agmen(home.path(), &["login", "--hub", &base]));
        let mut lines = Lines::new(&mut child);
        // The code is the line's last word, such as `BCDF-GHJK`.
        let code = loop {
            let line = lines.next().await.expect("login shows a code");
            if line.contains("code") {
                break line.split_whitespace().last().unwrap().to_owned();
            }
        };
        ctx.hub.approve_device(&code, &human.id).await.unwrap();
        let status = tokio::time::timeout(WAIT, child.wait())
            .await
            .expect("login ends once approved")
            .unwrap();
        let mut rest = Vec::new();
        while let Some(line) = lines.next().await {
            rest.push(line);
        }
        assert!(status.success(), "{rest:?}");
        assert!(
            rest.iter()
                .any(|line| line == &format!("signed in as {}", human.handle)),
            "{rest:?}"
        );

        assert!(
            rest.iter().any(|line| line
                == &format!(
                    "Open {}login/device and enter the code. It expires in 15 minutes.",
                    ctx.github.base()
                )),
            "{rest:?}"
        );

        let out = home.run(home.path(), &["whoami"]).await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert!(text.contains(&human.handle), "{text}");
        assert!(text.contains(ctx.topics.project().as_str()), "{text}");
        assert!(text.contains("admin"), "{text}");

        let out = home.run(home.path(), &["logout"]).await;
        assert_exit(&out, 0);
        let out = home.run(home.path(), &["whoami"]).await;
        assert_exit(&out, 3);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_login_declined_on_github() {
    run(Ingest::Off, |ctx| async move {
        let home = Home::new();
        let base = ctx.base.to_string();
        let mut command =
            tokio::process::Command::from(home.agmen(home.path(), &["login", "--hub", &base]));
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = Lines::new(&mut child);
        let line = lines.next().await.expect("login shows a code first");
        let code = line.strip_prefix("Your code is ").unwrap();
        ctx.github.deny(code);
        let output = tokio::time::timeout(WAIT, child.wait_with_output())
            .await
            .expect("declined login ends")
            .unwrap();
        assert_exit(&output, 1);
        assert!(stderr(&output).contains("declined on GitHub"));
    })
    .await;
}

async fn invite_link(ctx: &support::Ctx) -> String {
    let admin = ctx.hub.seed_human(&ctx.topics, Role::Admin).await.unwrap();
    let home = Home::new();
    home.sign_in(&ctx.base, &admin);
    let out = home
        .run(
            home.path(),
            &["invite", "--project", ctx.topics.project().as_str()],
        )
        .await;
    assert_exit(&out, 0);
    stdout(&out)
        .lines()
        .find_map(|line| line.strip_prefix("  "))
        .unwrap()
        .to_owned()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn it_join_link_signs_in_accepts_and_sets_up() {
    run(Ingest::On, |ctx| async move {
        let link = invite_link(&ctx).await;
        let home = Home::new();
        home.fake_program("claude", "if [ \"$2\" = list ]; then printf '[]\\n'; fi");
        let wt = Worktree::new(&home);
        let mut child = spawn(home.agmen(&wt.path(), &["join", &link, "--handle", "sol"]));
        let mut lines = Lines::new(&mut child);
        let first = lines.next().await.expect("join shows a device code");
        let code = first.strip_prefix("Your code is ").unwrap();
        ctx.github.approve(code, 9001, "Newbie");
        let text = tokio::time::timeout(WAIT, async {
            let mut text = first;
            for _ in 0..100 {
                let Some(line) = lines.next().await else {
                    return text;
                };
                text.push('\n');
                text.push_str(&line);
            }
            panic!("join printed more than 100 lines");
        })
        .await
        .unwrap();
        let status = tokio::time::timeout(WAIT, child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "{text}");
        assert_eq!(wt.identity().handle, "newbie/sol");
        let login: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.login_file()).unwrap()).unwrap();
        assert_eq!(login["hub_url"], ctx.base.as_str());
        for expected in [
            "joined project",
            "Claude Code: installed the agmen plugin",
            "Next: start your agent",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(
            std::fs::read_to_string(home.path().join("calls/claude.log"))
                .unwrap()
                .contains("plugin install agmen --marketplace agmen-dev/agmen --scope user --yes")
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_join_reuses_the_sign_in_for_the_same_hub() {
    run(Ingest::On, |ctx| async move {
        let link = invite_link(&ctx).await;
        let other = ctx.hub.seed_project().await.unwrap();
        let person = ctx.hub.seed_human(&other, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &person);
        let wt = Worktree::new(&home);
        let out = home
            .run(&wt.path(), &["join", &link, "--handle", "sol"])
            .await;
        assert_exit(&out, 0);
        assert!(!stdout(&out).contains("Your code is"));
    })
    .await;
}

/// A failed plugin install falls back to `.mcp.json` and the join still
/// succeeds with its identity in place.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn it_join_plugin_failure_falls_back_to_mcp_json() {
    run(Ingest::On, |ctx| async move {
        let link = invite_link(&ctx).await;
        let other = ctx.hub.seed_project().await.unwrap();
        let person = ctx.hub.seed_human(&other, Role::Member).await.unwrap();
        let home = Home::new();
        home.fake_program("claude", "echo broken >&2; exit 1");
        home.sign_in(&ctx.base, &person);
        let wt = Worktree::new(&home);
        let out = home
            .run(&wt.path(), &["join", &link, "--handle", "sol"])
            .await;
        assert_exit(&out, 0);
        assert_eq!(wt.identity().handle, format!("{}/sol", person.handle));
        let text = stdout(&out);
        assert!(text.contains("plugin install failed"), "{text}");
        assert!(text.contains("wrote .mcp.json instead"), "{text}");
        assert!(text.contains("Next: start your agent"), "{text}");
    })
    .await;
}

/// An invite for a project the person already belongs to is refused with
/// the way on, and no identity is minted.
#[tokio::test(flavor = "multi_thread")]
async fn it_join_link_for_a_project_you_belong_to_points_to_project() {
    run(Ingest::On, |ctx| async move {
        let link = invite_link(&ctx).await;
        let person = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &person);
        let wt = Worktree::new(&home);
        let out = home.run(&wt.path(), &["join", &link]).await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains("agmen join --project"),
            "{}",
            stderr(&out)
        );
        assert!(!wt.path().join(".agmen/identity.json").exists());
    })
    .await;
}

/// A link to another hub signs in there and says it replaced the old
/// sign-in. `localhost` and `127.0.0.1` are two origins for one test hub.
#[tokio::test(flavor = "multi_thread")]
async fn it_join_link_to_another_hub_says_it_signed_you_out() {
    run(Ingest::On, |ctx| async move {
        let link = invite_link(&ctx).await;
        let (hub, code) = link.split_once("/i#").unwrap();
        let mut hub: url::Url = hub.parse().unwrap();
        hub.set_host(Some("localhost")).unwrap();
        let link = format!("{}/i#{code}", hub.as_str().trim_end_matches('/'));
        let other = ctx.hub.seed_project().await.unwrap();
        let person = ctx.hub.seed_human(&other, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &person);
        let wt = Worktree::new(&home);
        let mut child = spawn(home.agmen(
            &wt.path(),
            &["join", &link, "--handle", "sol", "--no-setup"],
        ));
        let mut lines = Lines::new(&mut child);
        let first = lines.next().await.expect("join shows a device code");
        let code = first.strip_prefix("Your code is ").unwrap();
        ctx.github.approve(code, 9002, "Visitor");
        let mut text = first;
        while let Some(line) = tokio::time::timeout(WAIT, lines.next()).await.unwrap() {
            text.push('\n');
            text.push_str(&line);
        }
        let status = tokio::time::timeout(WAIT, child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "{text}");
        assert!(
            text.contains(&format!(
                "signed out of {}: agmen keeps one sign-in per machine",
                ctx.base
            )),
            "{text}"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_join_outside_a_worktree_points_to_the_clone() {
    run(Ingest::On, |ctx| async move {
        ctx.hub
            .set_repo_url(
                &ctx.topics,
                &"https://github.com/agmen-dev/agmen".parse().unwrap(),
            )
            .await
            .unwrap();
        let link = invite_link(&ctx).await;
        let other = ctx.hub.seed_project().await.unwrap();
        let person = ctx.hub.seed_human(&other, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &person);
        let out = home.run(home.path(), &["join", &link]).await;
        assert_exit(&out, 1);
        assert!(stderr(&out).contains("git clone https://github.com/agmen-dev/agmen"));
        assert!(stderr(&out).contains(&format!("agmen join --project {}", ctx.topics.project())));
        assert!(!home.path().join(".agmen/identity.json").exists());
        let wt = Worktree::new(&home);
        let out = home.run(&wt.path(), &["join", &link]).await;
        assert_exit(&out, 1);
        assert!(stderr(&out).contains("already used"));
        assert!(stderr(&out).contains("agmen join --project <id>"));
        let out = home
            .run(
                &wt.path(),
                &[
                    "join",
                    "--project",
                    ctx.topics.project().as_str(),
                    "--handle",
                    "sol",
                ],
            )
            .await;
        assert_exit(&out, 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_join_without_arguments_uses_the_only_project() {
    run(Ingest::On, |ctx| async move {
        let person = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &person);
        let wt = Worktree::new(&home);
        let out = home.run(&wt.path(), &["join", "--handle", "sol"]).await;
        assert_exit(&out, 0);
        assert_eq!(wt.identity().project, *ctx.topics.project());
        let other = ctx.hub.seed_project().await.unwrap();
        let admin = ctx.hub.seed_human(&other, Role::Admin).await.unwrap();
        let admin_home = Home::new();
        admin_home.sign_in(&ctx.base, &admin);
        let out = admin_home
            .run(
                admin_home.path(),
                &["invite", "--project", other.project().as_str()],
            )
            .await;
        assert_exit(&out, 0);
        let link = stdout(&out)
            .lines()
            .find_map(|line| line.strip_prefix("  "))
            .unwrap()
            .to_owned();
        let wt2 = Worktree::new(&home);
        let out = home
            .run(&wt2.path(), &["join", &link, "--handle", "luna"])
            .await;
        assert_exit(&out, 0);
        let wt3 = Worktree::new(&home);
        let out = home.run(&wt3.path(), &["join"]).await;
        assert_exit(&out, 2);
        let text = stderr(&out);
        assert!(text.contains(ctx.topics.project().as_str()), "{text}");
        assert!(text.contains(other.project().as_str()), "{text}");
        assert!(text.contains("--project"), "{text}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_join_bare_code_uses_the_signed_in_hub() {
    run(Ingest::On, |ctx| async move {
        let link = invite_link(&ctx).await;
        let code = link.split_once('#').unwrap().1;
        let other = ctx.hub.seed_project().await.unwrap();
        let person = ctx.hub.seed_human(&other, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &person);
        let wt = Worktree::new(&home);
        let out = home
            .run(&wt.path(), &["join", code, "--handle", "sol", "--no-setup"])
            .await;
        assert_exit(&out, 0);
        assert_eq!(wt.identity().project, *ctx.topics.project());
        assert!(stdout(&out).contains("agent setup skipped: run agmen setup"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_invite_and_join() {
    run(Ingest::On, |ctx| async move {
        let admin = ctx.hub.seed_human(&ctx.topics, Role::Admin).await.unwrap();
        let admin_home = Home::new();
        admin_home.sign_in(&ctx.base, &admin);
        let project = ctx.topics.project().as_str();
        let out = admin_home
            .run(
                admin_home.path(),
                &["invite", "--project", project, "--expires", "2h"],
            )
            .await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert!(!text.contains("copied"));
        let link = text
            .lines()
            .find_map(|line| line.strip_prefix("They run: agmen join "))
            .expect("an invite link with the code in its fragment")
            .to_owned();

        // A person from another project joins with the code.
        let elsewhere = ctx.hub.seed_project().await.unwrap();
        let newcomer = ctx.hub.seed_human(&elsewhere, Role::Member).await.unwrap();
        let home = Home::new();
        home.sign_in(&ctx.base, &newcomer);
        let wt = Worktree::new(&home);
        let out = home
            .run(&wt.path(), &["join", &link, "--handle", "sol"])
            .await;
        assert_exit(&out, 0);
        let identity = wt.identity();
        assert_eq!(identity.handle, format!("{}/sol", newcomer.handle));
        assert_eq!(identity.project, *ctx.topics.project());
        assert_eq!(identity.tenant, *ctx.topics.tenant());
        assert_eq!(identity.owner_id.as_str(), newcomer.id.as_str());
        assert_eq!(identity.hub_url, ctx.base);
        assert_eq!(
            identity.broker_url.to_string(),
            support::env("AGMEN_IT_MQTT")
        );
        let exclude = std::fs::read_to_string(wt.path().join(".git/info/exclude")).unwrap();
        assert!(exclude.lines().any(|line| line == ".agmen/"), "{exclude}");
        let joined = find_identity(&wt.path()).unwrap().expect("trusted");
        assert_eq!(joined.identity, identity);

        // The code works once; a second worktree of a member joins by project.
        let wt2 = Worktree::new(&home);
        let out = home.run(&wt2.path(), &["join", &link]).await;
        assert_exit(&out, 1);
        assert!(stderr(&out).contains("already used"), "{}", stderr(&out));
        let out = home
            .run(
                &wt2.path(),
                &["join", "--project", project, "--handle", "luna"],
            )
            .await;
        assert_exit(&out, 0);
        assert_eq!(wt2.identity().handle, format!("{}/luna", newcomer.handle));

        // A joined worktree cannot be joined again.
        let out = home
            .run(
                &wt2.path(),
                &["join", "--project", project, "--handle", "x"],
            )
            .await;
        assert_exit(&out, 1);
        assert!(stderr(&out).contains("already joined"), "{}", stderr(&out));

        // A member may not invite.
        let out = home.run(&wt2.path(), &["invite"]).await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains("only a project admin or coordinator"),
            "{}",
            stderr(&out)
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_post_prints_recorded_seq() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let out = sol.run(&["post", "FYI", "hello room"]).await;
        assert_exit(&out, 0);
        assert_eq!(stdout(&out).trim(), "recorded #1 (feed)");
        let feed = feed(&ctx.base, &sol).await;
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].client.body.as_str(), "hello room");
        assert_eq!(feed[0].from_handle, sol.identity.handle);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_post_final_nack_exit_4() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let body = "x".repeat(40_000);
        let out = sol.run(&["post", "FYI", &body]).await;
        assert_exit(&out, 4);
        assert!(stderr(&out).contains("body_too_large"), "{}", stderr(&out));

        // The hub's own final NACK: an agent may not post a RULING.
        let out = sol
            .run(&["post", "RULING", "--data", r#"{"subject":"x"}"#, "ok"])
            .await;
        assert_exit(&out, 4);
        assert!(
            stderr(&out).contains("rejected: forbidden_verb"),
            "{}",
            stderr(&out)
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_post_queued_exit_5() {
    run(Ingest::Off, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let out = sol.run(&["post", "FYI", "anyone?", "--wait", "1"]).await;
        assert_exit(&out, 5);
        assert!(stdout(&out).starts_with("queued id="), "{}", stdout(&out));
    })
    .await;
}

/// A CLAIM the hub records only after the command gave up waiting is still
/// in `claims.db`: HEARTBEAT renews it and DONE diffs from it.
#[tokio::test(flavor = "multi_thread")]
async fn it_claim_granted_after_the_wait() {
    run(Ingest::On, |mut ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        assert_exit(&sol.run(&["found", "the build is slow"]).await, 0);

        ctx.set_ingest(Ingest::Off).await;
        let out = sol.run(&["claim", "B-1", "--wait", "1"]).await;
        assert_exit(&out, 5);
        assert!(stdout(&out).starts_with("queued id="), "{}", stdout(&out));

        // The next command sends the queued CLAIM again before its own
        // message, so the hub grants it first.
        ctx.set_ingest(Ingest::On).await;
        let out = sol.run(&["heartbeat"]).await;
        assert_exit(&out, 0);
        let inbox = stream(&ctx.base, &sol, &Stream::Inbox(sol.identity.identity_id.clone())).await;
        let renewed: Vec<&[ItemKey]> = inbox
            .iter()
            .filter_map(|message| match &message.client.payload {
                Payload::Heartbeat { items } => Some(items.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(renewed, [["B-1".parse::<ItemKey>().unwrap()]], "{inbox:?}");
        assert!(
            inbox
                .iter()
                .any(|message| matches!(&message.client.payload, Payload::Grant { item, .. } if item.as_str() == "B-1")),
            "the queued CLAIM was granted: {inbox:?}"
        );

        let out = sol.run(&["done", "B-1", "--commit", "HEAD"]).await;
        assert_exit(&out, 0);
        assert!(stdout(&out).contains("done B-1"), "{}", stdout(&out));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_watch_json_lines() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let luna = ctx.member(Role::Member, "luna", "agent").await;
        let mut child = spawn(sol.home.agmen(&sol.wt.path(), &["watch", "--json"]));
        let mut lines = Lines::new(&mut child);
        let first = lines.next().await.expect("watch prints its self line");
        let me: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(me["kind"], "self");
        assert_eq!(me["identity_id"], sol.identity.identity_id.as_str());
        assert_eq!(me["handle"], sol.identity.handle.as_str());
        assert_eq!(me["owner_id"], sol.identity.owner_id.as_str());

        // Wait until the watch is connected, then DM it by id.
        let mut connected = false;
        while !connected {
            let line = lines.next().await.expect("watch connects");
            let event: ClientEvent = serde_json::from_str(&line).unwrap();
            connected = event == ClientEvent::Connection { connected: true };
        }
        let to = sol.identity.identity_id.to_string();
        let out = luna.run(&["dm", &to, "ping from luna"]).await;
        assert_exit(&out, 0);
        assert!(stdout(&out).starts_with("recorded #"), "{}", stdout(&out));

        let dm = loop {
            let line = lines.next().await.expect("the DM reaches the watch");
            // Every line is a ClientEvent.
            let event: ClientEvent = serde_json::from_str(&line).unwrap();
            if let ClientEvent::Message { message } = event
                && matches!(message.client.payload, Payload::Dm {})
            {
                break message;
            }
        };
        assert_eq!(dm.client.body.as_str(), "ping from luna");
        assert_eq!(dm.from, luna.identity.identity_id);
        drop(lines);
        child.kill().await.unwrap();
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_inbox_lists_then_advances() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let luna = ctx.member(Role::Member, "luna", "agent").await;
        let to = sol.identity.identity_id.to_string();
        assert_exit(&luna.run(&["dm", &to, "first"]).await, 0);
        assert_exit(&luna.run(&["post", "FYI", "not for sol"]).await, 0);

        let out = sol.run(&["inbox"]).await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert!(text.contains("DM from"), "{text}");
        assert!(text.contains(&luna.identity.handle), "{text}");
        assert!(text.contains("first"), "{text}");
        assert!(!text.contains("not for sol"), "{text}");

        let out = sol.run(&["inbox"]).await;
        assert_exit(&out, 0);
        assert_eq!(stdout(&out).trim(), "no new messages");

        // An explicit --after lists again.
        let out = sol.run(&["inbox", "--after", "0", "--json"]).await;
        assert_exit(&out, 0);
        let lines: Vec<ClientEvent> = stdout(&out)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_handles_resolve_through_the_roster() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let luna = ctx.member(Role::Member, "luna", "agent").await;

        let out = luna.run(&["dm", "nobody/here", "hi"]).await;
        assert_exit(&out, 2);
        assert!(stderr(&out).contains("nobody/here"), "{}", stderr(&out));

        // No HELLO: minting sol made the hub publish the roster.
        let deadline = Instant::now() + WAIT;
        loop {
            let out = luna.run(&["dm", &sol.identity.handle, "by handle"]).await;
            if out.status.code() == Some(0) {
                break;
            }
            assert_exit(&out, 2);
            assert!(Instant::now() < deadline, "{}", stderr(&out));
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let out = sol.run(&["inbox"]).await;
        assert!(stdout(&out).contains("by handle"), "{}", stdout(&out));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_claim_and_done_fill_changes() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let out = sol.run(&["found", "the build is slow"]).await;
        assert_exit(&out, 0);
        assert!(stdout(&out).contains("item B-1"), "{}", stdout(&out));

        let out = sol.run(&["claim", "B-1"]).await;
        assert_exit(&out, 0);
        assert!(stdout(&out).contains("granted B-1"), "{}", stdout(&out));

        let out = sol.run(&["heartbeat"]).await;
        assert_exit(&out, 0);

        let base = sol.wt.git(&sol.home, &["rev-parse", "HEAD"]);
        std::fs::create_dir_all(sol.wt.path().join("src")).unwrap();
        std::fs::write(sol.wt.path().join("src/fast.rs"), "fn main() {}\n").unwrap();
        std::fs::write(sol.wt.path().join("README.md"), "faster\n").unwrap();
        sol.wt.git(&sol.home, &["add", "-A"]);
        sol.wt.git(&sol.home, &["commit", "-q", "-m", "speed up"]);
        let head = sol.wt.git(&sol.home, &["rev-parse", "HEAD"]);

        let out = sol.run(&["done", "B-1", "--commit", "HEAD"]).await;
        assert_exit(&out, 0);
        let feed = feed(&ctx.base, &sol).await;
        let done = feed
            .iter()
            .find_map(|message| match &message.client.payload {
                Payload::Done {
                    item,
                    branch,
                    commit,
                    base_sha,
                    changes,
                    ..
                } => Some((item, branch, commit, base_sha, changes)),
                _ => None,
            })
            .expect("a DONE on the feed");
        assert_eq!(done.0.as_str(), "B-1");
        assert_eq!(done.1, "main");
        assert_eq!(done.2.as_str(), head.trim());
        assert_eq!(done.3.as_str(), base.trim());
        let changes: Vec<(&str, &str)> = done
            .4
            .iter()
            .map(|change| (change.status.as_str(), change.path.as_str()))
            .collect();
        assert_eq!(changes, [("M", "README.md"), ("A", "src/fast.rs")]);

        // A second claim of a done item is denied.
        let out = sol.run(&["claim", "B-1"]).await;
        assert_exit(&out, 4);
        assert!(
            stderr(&out).contains("denied B-1: closed"),
            "{}",
            stderr(&out)
        );
        // ...and leaves nothing in claims.db for HEARTBEAT to renew.
        let out = sol.run(&["heartbeat"]).await;
        assert_exit(&out, 0);
        assert_eq!(stdout(&out).trim(), "no claims to renew");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_lanes_and_check_path() {
    run(Ingest::On, |ctx| async move {
        let coord = ctx.member(Role::Coordinator, "coord", "coordinator").await;
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let file = coord.wt.path().join("lanes.toml");
        std::fs::write(
            &file,
            "[[lanes]]\nname = \"L-UI\"\nglobs = [\"/ui/\"]\n\n[[lanes]]\nname = \"L-CORE\"\nglobs = [\"/src/\"]\n",
        )
        .unwrap();
        let out = coord.run(&["lanes", "set", file.to_str().unwrap()]).await;
        assert_exit(&out, 0);
        let out = sol.run(&["lanes"]).await;
        assert_exit(&out, 0);
        assert!(stdout(&out).contains("L-UI"), "{}", stdout(&out));

        let root = sol.wt.path();
        let absolute = root.join("ui/b.ts");
        let absolute = absolute.to_str().unwrap();
        let out = sol
            .run(&[
                "check-path",
                "--json",
                "--",
                "ui/a.ts",
                "-n.ts",
                absolute,
                "/elsewhere/x.ts",
                "--json",
            ])
            .await;
        assert_exit(&out, 0);
        let entries: Vec<serde_json::Value> = serde_json::from_str(&stdout(&out)).unwrap();
        let summary: Vec<(String, serde_json::Value, bool, String)> = entries
            .iter()
            .map(|entry| {
                (
                    entry["path"].as_str().unwrap().to_owned(),
                    entry["lane"].clone(),
                    entry["allowed"].as_bool().unwrap(),
                    entry["reason"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let ui = serde_json::json!("L-UI");
        assert_eq!(
            summary,
            [
                ("ui/a.ts".to_owned(), ui.clone(), false, "not claimed".to_owned()),
                ("-n.ts".to_owned(), serde_json::Value::Null, true, "unowned".to_owned()),
                (absolute.to_owned(), ui, false, "not claimed".to_owned()),
                (
                    "/elsewhere/x.ts".to_owned(),
                    serde_json::Value::Null,
                    false,
                    "not a repository path".to_owned()
                ),
                ("--json".to_owned(), serde_json::Value::Null, true, "unowned".to_owned()),
            ]
        );

        // Titles come from peers: an escape sequence or a line break in one
        // never reaches the terminal.
        let out = sol.run(&["found", "red \u{1b}[31malert\r\nB-9 fake"]).await;
        assert_exit(&out, 0);
        let out = sol.run(&["items"]).await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains("red [31malertB-9 fake"), "{text:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_revoked_identity_exits_3_over_http() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        // Revoked behind the worktree's back: its file and token stay.
        let resp = reqwest::Client::new()
            .delete(
                ctx.base
                    .join(&format!("v1/identities/{}", sol.identity.identity_id))
                    .unwrap(),
            )
            .bearer_auth(&sol.human.access_token)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
        for args in [&["lanes"][..], &["items"], &["check-path", "--", "a.rs"]] {
            let out = sol.run(args).await;
            assert_exit(&out, 3);
            let err = stderr(&out);
            assert!(err.contains("identity_revoked"), "{args:?}: {err}");
            assert!(
                err.contains("run agmen leave, then agmen join"),
                "{args:?}: {err}"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_leave_revokes_and_removes_the_identity() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let out = sol.run(&["leave"]).await;
        assert_exit(&out, 0);
        assert!(!sol.wt.path().join(".agmen/identity.json").exists());
        let key = TokenKey::new(&ctx.base, &sol.identity.identity_id).unwrap();
        assert_eq!(sol.home.tokens().get(&key).unwrap(), None);
        let out = sol.run(&["post", "FYI", "still here?"]).await;
        assert_exit(&out, 3);
    })
    .await;
}
