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

#[cfg(unix)]
async fn host_home(ctx: &support::Ctx) -> (Home, agmen_hub::SeededHuman) {
    let human = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
    for query in [
        "DELETE FROM memberships WHERE human_id = $1",
        "DELETE FROM tenant_members WHERE human_id = $1",
    ] {
        sqlx::query(query)
            .bind(human.id.as_str())
            .execute(&ctx.db)
            .await
            .unwrap();
    }
    let home = Home::new();
    home.sign_in(&ctx.base, &human);
    home.fake_program(
        "docker",
        r#"
if [ "$1" = "version" ]; then
	printf '1\n'
	exit 0
fi
shift
while [ "$#" -gt 0 ]; do
	case "$1" in
	--project-directory | -f)
		shift
		shift
		;;
	*) break ;;
	esac
done
case "$1" in
version) printf '1\n' ;;
ps) printf '{"Service":"hub","State":"running","Health":"healthy"}\n' ;;
logs) /bin/cat "$HOME/fake/hub.log" ;;
restart)
	if [ -f "$HOME/fake/hub2.log" ]; then
		/bin/mv "$HOME/fake/hub2.log" "$HOME/fake/hub.log"
	fi
	;;
esac
exit 0
"#,
    );
    std::fs::create_dir(home.path().join("fake")).unwrap();
    (home, human)
}

#[cfg(unix)]
async fn host_clone(home: &Home, origin: &str) -> std::path::PathBuf {
    let root = home.path().join("widgets");
    std::fs::create_dir(&root).unwrap();
    for args in [vec!["init", "-q"], vec!["remote", "add", "origin", origin]] {
        let mut cmd = Command::new("git");
        cmd.current_dir(&root).args(args);
        home.apply_env(&mut cmd);
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::from(cmd)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(out.status.success());
    }
    std::fs::canonicalize(root).unwrap()
}

#[cfg(unix)]
fn host_claim_log(home: &Home, file: &str, token: &str) {
    std::fs::write(
        home.path().join("fake").join(file),
        format!("hub-1 | warn claim_token={token} expires_at=later\n"),
    )
    .unwrap();
}

#[cfg(unix)]
async fn host_up(
    ctx: &support::Ctx,
    home: &Home,
    root: &std::path::Path,
    extra: &[&str],
) -> std::process::Output {
    let port = ctx.base.port().unwrap().to_string();
    let mut args = vec![
        "host",
        "up",
        "--public-url",
        ctx.base.as_str(),
        "--port",
        &port,
        "--handle",
        "sol",
    ];
    args.extend_from_slice(extra);
    let mut cmd = home.agmen(root, &args);
    cmd.env("AGMEN_HOST_ALLOW_HTTP", "1").stdin(Stdio::null());
    tokio::time::timeout(
        Duration::from_secs(45),
        tokio::process::Command::from(cmd)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}

#[cfg(unix)]
async fn host_me(ctx: &support::Ctx, human: &agmen_hub::SeededHuman) -> serde_json::Value {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .get(ctx.base.join("v1/me").unwrap())
        .bearer_auth(&human.access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[cfg(unix)]
async fn host_projects(ctx: &support::Ctx) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as("SELECT p.id, p.name, p.repo_url FROM projects p JOIN hub_group g ON p.tenant_id = g.tenant_id ORDER BY p.id")
        .fetch_all(&ctx.db).await.unwrap()
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn it_host_up_claims_creates_project_and_invites() {
    run(Ingest::Off, |ctx| async move {
        let (home, human) = host_home(&ctx).await;
        let root = host_clone(&home, "https://github.com/acme/widgets.git").await;
        let token = ctx.hub.mint_claim_token().await.unwrap().unwrap();
        host_claim_log(&home, "hub.log", &token);
        let out = host_up(&ctx, &home, &root, &[]).await;
        assert_exit(&out, 0);
        assert_eq!(host_me(&ctx, &human).await["groups"][0]["role"], "owner");
        let projects = host_projects(&ctx).await;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].1, "widgets");
        assert_eq!(
            projects[0].2.as_deref(),
            Some("https://github.com/acme/widgets.git")
        );
        let path = root.join(".agmen/identity.json");
        let before = std::fs::read(&path).unwrap();
        let identity: agmen_client::IdentityFile = serde_json::from_slice(&before).unwrap();
        assert_eq!(identity.project.as_str(), projects[0].0);
        assert!(identity.handle.ends_with("/sol"));
        assert!(stdout(&out).contains("joined project widgets"));
        assert!(stdout(&out).contains("/i#"));
        assert!(!stdout(&out).contains(&token));
        assert!(!stderr(&out).contains(&token));
        let again = host_up(&ctx, &home, &root, &[]).await;
        assert_exit(&again, 0);
        assert_eq!(host_projects(&ctx).await, projects);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(stdout(&again).contains("/i#"));
        let identities: i64 =
            sqlx::query_scalar("SELECT count(*) FROM identities WHERE project_id = $1")
                .bind(&projects[0].0)
                .fetch_one(&ctx.db)
                .await
                .unwrap();
        assert_eq!(identities, 1);
        let calls = std::fs::read_to_string(home.path().join("calls/docker.log")).unwrap();
        assert!(!calls.contains("restart hub"));
    })
    .await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn it_host_up_keeps_tokens_out_of_repo_url() {
    run(Ingest::Off, |ctx| async move {
        let (home, _) = host_home(&ctx).await;
        let secret = ["ghp", "s3cret"].join("_");
        let root = host_clone(
            &home,
            &format!("https://andy:{secret}@github.com/acme/widgets.git"),
        )
        .await;
        let token = ctx.hub.mint_claim_token().await.unwrap().unwrap();
        host_claim_log(&home, "hub.log", &token);
        let out = host_up(&ctx, &home, &root, &[]).await;
        assert_exit(&out, 0);
        let projects = host_projects(&ctx).await;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].2, None);
        assert!(stdout(&out).contains("the project has no repository URL:"));
        for output in [stdout(&out), stderr(&out)] {
            assert!(!output.contains(&secret));
            assert!(!output.contains(&token));
        }
    })
    .await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn it_host_up_mints_new_claim_when_stale() {
    run(Ingest::Off, |ctx| async move {
        let (home, human) = host_home(&ctx).await;
        let root = host_clone(&home, "https://github.com/acme/widgets.git").await;
        host_claim_log(&home, "hub.log", &"0".repeat(64));
        let token = ctx.hub.mint_claim_token().await.unwrap().unwrap();
        host_claim_log(&home, "hub2.log", &token);
        let out = host_up(&ctx, &home, &root, &[]).await;
        assert_exit(&out, 0);
        assert_eq!(host_me(&ctx, &human).await["groups"][0]["role"], "owner");
        let calls = std::fs::read_to_string(home.path().join("calls/docker.log")).unwrap();
        assert_eq!(calls.matches("restart hub").count(), 1);
        assert!(!stdout(&out).contains(&token));
        assert!(!stderr(&out).contains(&token));
    })
    .await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn it_host_up_outside_git_stops_after_claim() {
    run(Ingest::Off, |ctx| async move {
        let (home, human) = host_home(&ctx).await;
        let token = ctx.hub.mint_claim_token().await.unwrap().unwrap();
        host_claim_log(&home, "hub.log", &token);
        let out = host_up(&ctx, &home, home.path(), &[]).await;
        assert_exit(&out, 0);
        assert_eq!(host_me(&ctx, &human).await["groups"][0]["role"], "owner");
        assert!(host_projects(&ctx).await.is_empty());
        assert!(stdout(&out).contains("inside a clone of your repository"));
        assert!(!home.path().join(".agmen/identity.json").exists());
    })
    .await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn it_host_up_refuses_a_claimed_hub() {
    run(Ingest::Off, |ctx| async move {
        let (owner_home, owner) = host_home(&ctx).await;
        let (home, _) = host_home(&ctx).await;
        let token = ctx.hub.mint_claim_token().await.unwrap().unwrap();
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap()
            .post(ctx.base.join("v1/claim").unwrap())
            .bearer_auth(&owner.access_token)
            .json(&serde_json::json!({"token": token}))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        host_claim_log(&home, "hub.log", &token);
        let out = host_up(&ctx, &home, home.path(), &[]).await;
        assert_exit(&out, 1);
        assert!(stderr(&out).contains("already has an owner"));
        assert!(!stdout(&out).contains(&token));
        assert!(!stderr(&out).contains(&token));
        assert!(host_projects(&ctx).await.is_empty());
        assert!(owner_home.login_file().exists());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_chat_one_shot_mentions() {
    run(Ingest::On, |ctx| async move {
        let coord = ctx.member(Role::Admin, "coord", "coordinator").await;
        let text = format!("stand by @{}", coord.identity.handle);
        let out = coord.run(&["chat", &text, "--all"]).await;
        assert_exit(&out, 0);
        assert_eq!(stdout(&out).trim(), "recorded #1");
        let messages = feed(&ctx.base, &coord).await;
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert!(matches!(message.client.payload, Payload::Fyi {}));
        assert_eq!(message.from.as_str(), coord.human.id.as_str());
        assert_eq!(
            message.client.mentions,
            [
                agmen_proto::Mention::All,
                agmen_proto::Mention::Identity(coord.identity.identity_id.clone())
            ]
        );
        // The mention reaches coord's inbox once: a FYI is stored in the feed
        // only, and `agmen inbox` lists what mentions the identity.
        let out = coord.run(&["inbox"]).await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert_eq!(text.matches("stand by").count(), 1, "{text}");
        let out = coord.run(&["inbox"]).await;
        assert_eq!(stdout(&out).trim(), "no new messages");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_chat_unknown_handle_exit_2() {
    run(Ingest::On, |ctx| async move {
        let who = ctx.member(Role::Member, "sol", "agent").await;
        let out = who.run(&["chat", "hello @nobody"]).await;
        assert_exit(&out, 2);
        assert!(stderr(&out).contains("@nobody"), "{}", stderr(&out));
        assert!(feed(&ctx.base, &who).await.is_empty());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_chat_follow_posts_stdin_lines() {
    use tokio::io::AsyncWriteExt as _;
    run(Ingest::On, |ctx| async move {
        let who = ctx.member(Role::Member, "sol", "agent").await;
        let mut command = who.home.agmen(&who.wt.path(), &["chat"]);
        command.stdin(Stdio::piped());
        let mut child = spawn(command);
        let mut lines = Lines::new(&mut child);
        let mut input = child.stdin.take().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            input.write_all(b"first\nsecond\n").await.unwrap();
            drop(input);
            assert_eq!(
                lines.next().await.unwrap(),
                format!("#1 {}: first", who.human.handle)
            );
            assert_eq!(
                lines.next().await.unwrap(),
                format!("#2 {}: second", who.human.handle)
            );
            assert!(child.wait().await.unwrap().success());
        })
        .await
        .expect("chat posts and echoes both lines within 10 seconds");
        let messages = feed(&ctx.base, &who).await;
        assert_eq!(messages.len(), 2);
        assert!(
            messages
                .iter()
                .all(|message| matches!(message.client.payload, Payload::Fyi {})
                    && message.from.as_str() == who.human.id.as_str())
        );
    })
    .await;
}

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
            &["join", &link, "--yes", "--handle", "sol", "--no-setup"],
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
async fn it_join_link_to_another_hub_with_yes_signs_in_there() {
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
        let before = std::fs::read(home.login_file()).unwrap();
        let wt = Worktree::new(&home);
        let mut child = spawn(home.agmen(
            &wt.path(),
            &["join", &link, "--yes", "--handle", "sol", "--no-setup"],
        ));
        let mut lines = Lines::new(&mut child);
        let first = lines.next().await.expect("join shows a device code");
        let code = first.strip_prefix("Your code is ").unwrap();
        assert_eq!(std::fs::read(home.login_file()).unwrap(), before);
        ctx.github.approve(code, 9003, "Visitor");
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
        let login: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.login_file()).unwrap()).unwrap();
        assert_eq!(login["hub_url"], hub.as_str());
        assert_eq!(wt.identity().hub_url, hub);
        assert_eq!(wt.identity().handle, "visitor/sol");
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

/// 30 roster entries, more than the 4 KiB a retained `state/{key}` may hold:
/// `agmen dm` of the last handle, in handle order, exits 0. The retained topic
/// is only a head, so the lookup goes over HTTP.
#[tokio::test(flavor = "multi_thread")]
async fn it_handles_resolve_past_25() {
    run(Ingest::On, |ctx| async move {
        let sol = ctx.member(Role::Member, "sol", "agent").await;
        let http = reqwest::Client::new();
        let project = ctx.topics.project().as_str();
        // A long `agent` name makes each entry big enough that 30 of them
        // pass 4 KiB.
        let agent = format!("claude-code/2.1.310 {}", "x".repeat(100));
        // The member is one human and one agent. 28 more agents make 30.
        // `z27` sorts after `sol`, so it is the 30th handle.
        let mut handles = vec![sol.human.handle.clone(), sol.identity.handle.clone()];
        for n in 0..28 {
            let handle = format!("{}/z{n:02}", sol.human.handle);
            let resp = http
                .post(ctx.base.join("v1/identities").unwrap())
                .bearer_auth(&sol.human.access_token)
                .json(&serde_json::json!({
                    "handle": &handle,
                    "project": project,
                    "kind": "agent",
                    "agent": &agent,
                }))
                .send()
                .await
                .unwrap();
            let status = resp.status();
            if status != reqwest::StatusCode::CREATED {
                let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
                panic!("mint answered {status}: {}", body["error"]);
            }
            // The body holds the new identity token. Do not print it.
            let _ = resp.bytes().await.unwrap();
            handles.push(handle);
        }
        handles.sort();
        assert_eq!(handles.len(), 30);
        let target = &handles[29];
        assert!(target.ends_with("/z27"), "{target}");
        // The premise: the full roster is past the retained limit.
        let resp = http
            .get(
                ctx.base
                    .join(&format!("v1/projects/{project}/state?key=roster"))
                    .unwrap(),
            )
            .bearer_auth(&sol.human.access_token)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        let bytes = serde_json::to_vec(&body["value"]).unwrap().len();
        assert!(bytes > 4 * 1024, "the roster is {bytes} bytes");
        let out = sol.run(&["dm", target, "hi"]).await;
        assert_exit(&out, 0);
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

async fn make_owner(ctx: &support::Ctx, human: &str) {
    let rows = sqlx::query(
        "UPDATE tenant_members SET role = 'owner' WHERE tenant_id = $1 AND human_id = $2",
    )
    .bind(ctx.topics.tenant().as_str())
    .bind(human)
    .execute(&ctx.db)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(rows, 1);
}

async fn add_tenant(ctx: &support::Ctx, name: &str, human: &str, role: &str) -> String {
    let id = support::new_id();
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2)")
        .bind(&id)
        .bind(name)
        .execute(&ctx.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenant_members (tenant_id, human_id, role) VALUES ($1, $2, $3)")
        .bind(&id)
        .bind(human)
        .bind(role)
        .execute(&ctx.db)
        .await
        .unwrap();
    id
}

async fn move_member(ctx: &support::Ctx, human: &str, tenant: &str, role: &str) {
    sqlx::query("DELETE FROM tenant_members WHERE human_id = $1")
        .bind(human)
        .execute(&ctx.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tenant_members (tenant_id, human_id, role) VALUES ($1, $2, $3)")
        .bind(tenant)
        .bind(human)
        .bind(role)
        .execute(&ctx.db)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn it_group_controls() {
    run(Ingest::Off, |ctx| async move {
        let owner = ctx.member(Role::Admin, "own", "agent").await;
        let member = ctx.member(Role::Member, "mem", "agent").await;
        make_owner(&ctx, owner.human.id.as_str()).await;
        let project = format!("project {}", ctx.topics.project().as_str());
        let group = format!("tenant {}", ctx.topics.tenant().as_str());

        let out = owner.run(&["group", "members"]).await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        let owner_line = format!("owner {} ({project})", owner.human.handle);
        let member_line = format!("member {} ({project})", member.human.handle);
        let owner_at = text.find(&owner_line).unwrap_or_else(|| panic!("{text}"));
        let member_at = text.find(&member_line).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(
            owner_at < member_at,
            owner.human.handle < member.human.handle,
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "  {} {}/own in {project}",
                owner.identity.identity_id.as_str(),
                owner.human.handle
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "  {} {}/mem in {project}",
                member.identity.identity_id.as_str(),
                member.human.handle
            )),
            "{text}"
        );

        let out = owner
            .run(&["group", "role", &member.human.handle, "admin"])
            .await;
        assert_exit(&out, 0);
        assert_eq!(
            stdout(&out).trim(),
            format!("{} is now an admin of {group}", member.human.handle)
        );
        let out = owner.run(&["group", "members"]).await;
        assert_exit(&out, 0);
        assert!(
            stdout(&out).contains(&format!("admin {} ({project})", member.human.handle)),
            "{}",
            stdout(&out)
        );

        let out = owner
            .run(&["group", "transfer", &member.human.handle, "--yes"])
            .await;
        assert_exit(&out, 0);
        assert_eq!(
            stdout(&out).trim(),
            format!("{} is now the owner of {group}", member.human.handle)
        );
        let out = member.run(&["group", "members"]).await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert!(
            text.contains(&format!("owner {} ({project})", member.human.handle)),
            "{text}"
        );
        assert!(
            text.contains(&format!("admin {} ({project})", owner.human.handle)),
            "{text}"
        );
        assert!(
            !text.contains(&format!("owner {} ({project})", owner.human.handle)),
            "{text}"
        );

        let out = owner.run(&["group", "remove", &member.human.handle]).await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains("the owner cannot be removed; transfer ownership first"),
            "{}",
            stderr(&out)
        );

        let out = member.run(&["group", "remove", &owner.human.handle]).await;
        assert_exit(&out, 0);
        assert!(
            stdout(&out).contains(&format!("removed {} from {group}", owner.human.handle)),
            "{}",
            stdout(&out)
        );
        let rows: i64 =
            sqlx::query_scalar("SELECT count(*) FROM tenant_members WHERE human_id = $1")
                .bind(owner.human.id.as_str())
                .fetch_one(&ctx.db)
                .await
                .unwrap();
        assert_eq!(rows, 0);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_group_member_cannot_change_roles() {
    run(Ingest::Off, |ctx| async move {
        let owner = ctx.member(Role::Admin, "own", "agent").await;
        let member = ctx.member(Role::Member, "mem", "agent").await;
        make_owner(&ctx, owner.human.id.as_str()).await;
        let out = member
            .run(&["group", "role", &owner.human.handle, "admin"])
            .await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains("only the group's owner or an admin can do that"),
            "{}",
            stderr(&out)
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_group_admin_cannot_demote_admin() {
    run(Ingest::Off, |ctx| async move {
        let owner = ctx.member(Role::Admin, "own", "agent").await;
        let admin = ctx.member(Role::Admin, "ada", "agent").await;
        let other = ctx.member(Role::Admin, "bea", "agent").await;
        make_owner(&ctx, owner.human.id.as_str()).await;
        let out = admin
            .run(&["group", "role", &other.human.handle, "member"])
            .await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains("only the group's owner can do that"),
            "{}",
            stderr(&out)
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_group_revoke_identity() {
    run(Ingest::Off, |ctx| async move {
        let owner = ctx.member(Role::Admin, "own", "agent").await;
        let member = ctx.member(Role::Member, "mem", "agent").await;
        make_owner(&ctx, owner.human.id.as_str()).await;
        let member_id = member.identity.identity_id.as_str();
        let out = owner.run(&["group", "members"]).await;
        assert_exit(&out, 0);
        assert!(stdout(&out).contains(member_id), "{}", stdout(&out));

        let out = owner.run(&["group", "revoke", member_id]).await;
        assert_exit(&out, 0);
        assert!(
            stdout(&out).contains(&format!(
                "revoked {member_id}; its agent is disconnected and cannot reconnect"
            )),
            "{}",
            stdout(&out)
        );
        let revoked: bool =
            sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM identities WHERE id = $1")
                .bind(member_id)
                .fetch_one(&ctx.db)
                .await
                .unwrap();
        assert!(revoked);

        let owner_id = owner.identity.identity_id.as_str();
        let out = member.run(&["group", "revoke", owner_id]).await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains(&format!(
                "no identity {owner_id} that you can revoke (agmen group members lists them)"
            )),
            "{}",
            stderr(&out)
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_group_flag_takes_id_or_name() {
    run(Ingest::Off, |ctx| async move {
        let owner = ctx.member(Role::Admin, "own", "agent").await;
        make_owner(&ctx, owner.human.id.as_str()).await;
        sqlx::query("UPDATE tenants SET name = 'alpha' WHERE id = $1")
            .bind(ctx.topics.tenant().as_str())
            .execute(&ctx.db)
            .await
            .unwrap();
        let alpha_only = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
        let beta = add_tenant(&ctx, "beta", owner.human.id.as_str(), "owner").await;
        let beta_only = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
        move_member(&ctx, beta_only.id.as_str(), &beta, "member").await;

        let out = owner
            .home
            .run(owner.home.path(), &["group", "members"])
            .await;
        assert_exit(&out, 2);
        assert!(
            stderr(&out).contains("name the group: --group <name or id> (agmen whoami lists them)"),
            "{}",
            stderr(&out)
        );

        for flag in [beta.as_str(), "beta"] {
            let out = owner
                .home
                .run(owner.home.path(), &["group", "members", "--group", flag])
                .await;
            assert_exit(&out, 0);
            let text = stdout(&out);
            assert!(
                text.contains(&format!("member {}", beta_only.handle)),
                "{flag}: {text}"
            );
            assert!(!text.contains(&alpha_only.handle), "{flag}: {text}");
        }

        let out = owner
            .home
            .run(owner.home.path(), &["group", "members", "--group", "gamma"])
            .await;
        assert_exit(&out, 2);
        assert!(
            stderr(&out).contains("no group gamma: agmen whoami lists yours"),
            "{}",
            stderr(&out)
        );

        // tenants.name is unique. The resolver still has to list two groups
        // that share a stored name, so this test's database drops the check.
        sqlx::query("ALTER TABLE tenants DROP CONSTRAINT IF EXISTS tenants_name_key")
            .execute(&ctx.db)
            .await
            .unwrap();
        let again = support::new_id();
        sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'beta')")
            .bind(&again)
            .execute(&ctx.db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO tenant_members (tenant_id, human_id, role) VALUES ($1, $2, 'member')",
        )
        .bind(&again)
        .bind(owner.human.id.as_str())
        .execute(&ctx.db)
        .await
        .unwrap();
        let third = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
        move_member(&ctx, third.id.as_str(), &again, "member").await;

        let out = owner
            .home
            .run(owner.home.path(), &["group", "members", "--group", "beta"])
            .await;
        assert_exit(&out, 2);
        let err = stderr(&out);
        assert!(err.contains(&format!("({beta})")), "{err}");
        assert!(err.contains(&format!("({again})")), "{err}");

        let out = owner
            .home
            .run(
                owner.home.path(),
                &["group", "members", "--group", beta.as_str()],
            )
            .await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert!(text.contains(&beta_only.handle), "{text}");
        assert!(!text.contains(&third.handle), "{text}");
    })
    .await;
}
