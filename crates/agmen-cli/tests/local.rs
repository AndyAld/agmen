//! CLI commands against an in-process local origin, without a hub or broker.

#![cfg(test)]

#[path = "support/local.rs"]
mod local_support;
mod support;

use agmen_cli::McpOptions;
use agmen_client::{TokenKey, TokenStore, read_local_credential};
use agmen_proto::{ClientEnvelope, Payload, StampedEnvelope, Stream, Timestamp};
use local_support::Local;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use support::mcp::Mcp;
use support::{Home, Worktree, assert_exit, stderr, stdout};
use tokio::io::{AsyncBufReadExt as _, BufReader};

const MCP_WAIT: Duration = Duration::from_secs(15);
const LOCAL_POLL: Duration = Duration::from_millis(20);
const LOCAL_POLLS: usize = 200;
const WATCH_EVENTS: usize = 32;
/// The shortest beat `McpOptions::with_heartbeat` takes.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(1);
/// Polls for the first beat: 10 s at `LOCAL_POLL`, inside `MCP_WAIT`.
const HEARTBEAT_POLLS: usize = 500;

#[tokio::test]
async fn init_without_flags_starts_locally() {
    let local = Local::new().await;
    let worktree = Worktree::new(&local.home);
    let root = worktree.path();
    let name = root.file_name().unwrap().to_str().unwrap();
    assert!(!local.home.login_file().exists());
    let out = local.run(&worktree, &["init"]).await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains(&format!(
        "started {name} on this machine; add a hub later with agmen host up or agmen join <invite>"
    )));
    let identity = worktree.identity();
    assert!(identity.is_local());
    let handle_name = name.to_ascii_lowercase().trim_start_matches('.').to_owned();
    assert!(identity.handle.ends_with(&format!("/{handle_name}")));
    let projects = local.call("local_projects", json!({})).await;
    assert!(
        projects["projects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|project| {
                project["project"] == json!(identity.project) && project["name"] == name
            })
    );
    let exclude = worktree.git(&local.home, &["rev-parse", "--git-path", "info/exclude"]);
    assert!(
        std::fs::read_to_string(root.join(exclude.trim()))
            .unwrap()
            .lines()
            .any(|line| line == ".agmen/")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(root.join(".agmen/identity.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let out = local
        .run(&worktree, &["post", "FYI", "first message after init"])
        .await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("recorded #1 (feed)"));
    assert!(!local.home.login_file().exists());
    local.stop().await;
}

#[tokio::test]
async fn init_local_while_signed_in_stays_local() {
    let local = Local::new().await;
    let hub = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    hub.set_nonblocking(true).unwrap();
    let url = format!("http://{}/", hub.local_addr().unwrap());
    let login = json!({"hub_url": url, "human_id": "hmn7k2q9", "handle": "andy"});
    std::fs::create_dir_all(local.home.login_file().parent().unwrap()).unwrap();
    let login = serde_json::to_vec(&login).unwrap();
    std::fs::write(local.home.login_file(), &login).unwrap();
    let key = TokenKey::new(&url.parse().unwrap(), &"hmn7k2q9".parse().unwrap()).unwrap();
    local
        .home
        .tokens()
        .set(
            &key,
            &json!({
                "access_token": "fake-access", "refresh_token": "fake-refresh",
                "expires_at": "2099-01-01T00:00:00Z"
            })
            .to_string(),
        )
        .unwrap();
    const CHOICES: [&[&str]; 2] = [&["init"], &["init", "--local"]];
    for args in CHOICES {
        let worktree = Worktree::new(&local.home);
        let out = local.run(&worktree, args).await;
        assert_exit(&out, 0);
        assert!(worktree.identity().is_local());
        assert_eq!(std::fs::read(local.home.login_file()).unwrap(), login);
        assert_eq!(
            hub.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    local.stop().await;
}

#[tokio::test]
async fn join_in_a_local_worktree_refuses() {
    let local = Local::new().await;
    let worktree = local.member("worker").await;
    let identity_path = worktree.path().join(".agmen/identity.json");
    let identity = std::fs::read(&identity_path).unwrap();
    let exclude = worktree.git(&local.home, &["rev-parse", "--git-path", "info/exclude"]);
    let exclude_path = worktree.path().join(exclude.trim());
    let exclude = std::fs::read(&exclude_path).unwrap();
    let projects = local.call("local_projects", json!({})).await;
    let credential = read_local_credential(&local.home.tokens(), &worktree.identity().identity_id)
        .unwrap()
        .unwrap();
    let hub = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    hub.set_nonblocking(true).unwrap();
    let link = format!("http://{}/i#{}", hub.local_addr().unwrap(), "ab".repeat(32));
    let code = "ab".repeat(32);
    let choices: [&[&str]; 4] = [
        &["join"],
        &["join", &link],
        &["join", &code],
        &["join", "--project", "project1"],
    ];
    for args in choices {
        let out = local.run(&worktree, args).await;
        assert_exit(&out, 2);
        assert_eq!(
            stderr(&out).trim(),
            "this worktree is on this machine's local origin; a project cannot move to a hub"
        );
        assert_eq!(stdout(&out), "");
        assert_eq!(std::fs::read(&identity_path).unwrap(), identity);
        assert_eq!(std::fs::read(&exclude_path).unwrap(), exclude);
        assert_eq!(local.call("local_projects", json!({})).await, projects);
        let after = read_local_credential(&local.home.tokens(), &worktree.identity().identity_id)
            .unwrap()
            .unwrap();
        assert_eq!(after.local_token, credential.local_token);
        assert!(!local.home.login_file().exists());
        assert_eq!(
            hub.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    local.stop().await;
}

#[tokio::test]
async fn local_chat_posts_as_the_person() {
    let local = Local::new().await;
    let worktree = local.member("worker").await;
    let out = local
        .run(&worktree, &["chat", "hello as a person @all"])
        .await;
    assert_exit(&out, 0);
    let history = local
        .call("chat_history", json!({"project": local.project}))
        .await;
    assert_eq!(
        history["messages"][0]["from"],
        json!(worktree.identity().owner_id)
    );
    assert_ne!(
        history["messages"][0]["from"],
        json!(worktree.identity().identity_id)
    );
    assert_eq!(history["messages"][0]["body"], "hello as a person @all");
    assert_eq!(history["messages"][0]["mentions"], json!(["all"]));
    local.stop().await;
}

#[tokio::test]
async fn local_chat_all_succeeds() {
    let local = Local::new().await;
    let worktree = Worktree::new(&local.home);
    let out = local.run(&worktree, &["init", "--local"]).await;
    assert_exit(&out, 0);
    let out = local
        .run(&worktree, &["chat", "hello everyone", "--all"])
        .await;
    assert_exit(&out, 0);
    let history = local
        .call(
            "chat_history",
            json!({"project": worktree.identity().project}),
        )
        .await;
    assert_eq!(
        history["messages"][0]["from"],
        json!(worktree.identity().owner_id)
    );
    assert_eq!(history["messages"][0]["mentions"], json!(["all"]));
    local.stop().await;
}

#[tokio::test]
async fn init_local_project_names_have_bounded_suffixes() {
    let local = Local::new().await;
    local
        .call("local_project_create", json!({"name": "widgets"}))
        .await;
    const LAST_OCCUPIED_SUFFIX: u8 = 8;
    for suffix in 2..=LAST_OCCUPIED_SUFFIX {
        local
            .call(
                "local_project_create",
                json!({"name": format!("widgets-{suffix}")}),
            )
            .await;
    }
    let worktree = Worktree::new(&local.home);
    let out = local
        .run(
            &worktree,
            &["init", "--local", "--name", "widgets", "--handle", "sol"],
        )
        .await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("started widgets-9 on this machine"));
    assert!(worktree.identity().handle.ends_with("/sol"));
    let projects = local.call("local_projects", json!({})).await;
    let other = Worktree::new(&local.home);
    let exclude = other.git(&local.home, &["rev-parse", "--git-path", "info/exclude"]);
    let exclude_path = other.path().join(exclude.trim());
    let before = std::fs::read(&exclude_path).unwrap();
    let out = local
        .run(&other, &["init", "--local", "--name", "widgets"])
        .await;
    assert_exit(&out, 1);
    assert!(!other.path().join(".agmen").exists());
    assert_eq!(std::fs::read(&exclude_path).unwrap(), before);
    assert_eq!(local.call("local_projects", json!({})).await, projects);
    let out = local.run(&worktree, &["init"]).await;
    assert_exit(&out, 2);
    assert_eq!(local.call("local_projects", json!({})).await, projects);
    local.stop().await;
}

#[tokio::test]
async fn init_local_invalid_names_do_not_write_an_identity() {
    let local = Local::new().await;
    let worktree = Worktree::new(&local.home);
    let before = local.call("local_projects", json!({})).await;
    let out = local
        .run(&worktree, &["init", "--local", "--name", ""])
        .await;
    assert_exit(&out, 1);
    assert!(stderr(&out).contains("name must be 1 to 64 characters"));
    assert!(!worktree.path().join(".agmen").exists());
    assert_eq!(local.call("local_projects", json!({})).await, before);
    local.stop().await;
}

#[tokio::test]
async fn init_local_conflicts_with_hub_choices() {
    let home = Home::new();
    let worktree = Worktree::new(&home);
    const CHOICES: [&str; 2] = ["--self-host", "--hosted"];
    for flag in CHOICES {
        let out = home.run(&worktree.path(), &["init", "--local", flag]).await;
        assert_exit(&out, 2);
        assert!(stderr(&out).contains("cannot be used with"));
        assert!(!worktree.path().join(".agmen").exists());
    }
}

#[tokio::test]
async fn post_and_inbox_in_a_local_worktree() {
    let local = Local::new().await;
    let sender = local.member("sender").await;
    let reader = local.member("reader").await;
    let out = local
        .run(
            &sender,
            &["post", "FYI", "hello locally", "--mention", "all"],
        )
        .await;
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains("recorded #1 (feed)"),
        "{}",
        stdout(&out)
    );
    let out = local.run(&reader, &["inbox", "--json"]).await;
    assert_exit(&out, 0);
    let message: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(message["message"]["body"], "hello locally");
    assert_eq!(
        message["message"]["from"],
        json!(sender.identity().identity_id)
    );
    let out = local.run(&reader, &["inbox"]).await;
    assert_exit(&out, 0);
    assert_eq!(stdout(&out).trim(), "no new messages");
    let out = local
        .run(
            &sender,
            &["dm", &reader.identity().handle, "private local message"],
        )
        .await;
    assert_exit(&out, 0);
    let out = local.run(&reader, &["inbox"]).await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("private local message"));
    let out = local.run(&sender, &["chat", "local chat @all"]).await;
    assert_exit(&out, 0);
    let out = local.run(&reader, &["inbox"]).await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("local chat @all"));

    let out = local
        .run(
            &sender,
            &["ask", "local question", "--to", &reader.identity().handle],
        )
        .await;
    assert_exit(&out, 0);
    let out = local.run(&reader, &["inbox", "--json"]).await;
    assert_exit(&out, 0);
    let question: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    let question_id = question["message"]["id"].as_str().unwrap();
    let out = local.run(&sender, &["inbox"]).await;
    assert_exit(&out, 0);
    let out = local
        .run(&reader, &["answer", question_id, "local answer"])
        .await;
    assert_exit(&out, 0);
    let out = local.run(&sender, &["inbox"]).await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("local answer"), "{}", stdout(&out));

    check_credentials(&local, &sender, &reader).await;
    local.stop().await;
}

async fn check_credentials(local: &Local, sender: &Worktree, reader: &Worktree) {
    let identity = sender.identity();
    let credential = read_local_credential(&local.home.tokens(), &identity.identity_id)
        .unwrap()
        .unwrap();
    let empty_store = local.home.path().join("empty-tokens");
    let empty_store = empty_store.to_str().unwrap();
    let out = local
        .run_env(sender, &["items"], &[(TokenStore::DIR_VAR, empty_store)])
        .await;
    assert_exit(&out, 3);
    assert!(stderr(&out).contains(&format!(
        "start agmend: no local token for {}",
        identity.handle
    )));
    let out = local
        .run_env(
            sender,
            &["items"],
            &[
                (TokenStore::DIR_VAR, empty_store),
                ("AGMEN_IDENTITY", identity.identity_id.as_str()),
                ("AGMEN_LOCAL_TOKEN", &credential.local_token),
            ],
        )
        .await;
    assert_exit(&out, 0);
    let out = local
        .run_env(
            sender,
            &["items"],
            &[
                ("AGMEN_IDENTITY", reader.identity().identity_id.as_str()),
                ("AGMEN_LOCAL_TOKEN", "invalid for an unrelated identity"),
            ],
        )
        .await;
    assert_exit(&out, 0);
}

#[tokio::test]
async fn claim_grant_round_trip_locally() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    let out = local.run(&worker, &["found", "fix local build"]).await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("item B-1"), "{}", stdout(&out));
    let out = local.run(&worker, &["claim", "B-1"]).await;
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains("granted B-1 until"),
        "{}",
        stdout(&out)
    );
    let out = local.run(&worker, &["items"]).await;
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains(&format!(
            "B-1 open, held by {} until",
            worker.identity().handle
        )),
        "{}",
        stdout(&out)
    );
    let other = local.member("other").await;
    let out = local.run(&other, &["claim", "B-1"]).await;
    assert_exit(&out, 4);
    assert!(stderr(&out).contains("denied B-1"));
    let out = local.run(&worker, &["release", "B-1"]).await;
    assert_exit(&out, 0);
    local.stop().await;
}

#[tokio::test]
async fn lanes_items_check_path_locally() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    let out = local.run(&worker, &["items"]).await;
    assert_exit(&out, 0);
    assert_eq!(stdout(&out).trim(), "no items");
    let out = local.run(&worker, &["lanes"]).await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("no lanes"));
    const LANE_FORMATS: [&str; 2] = ["json", "toml"];
    for extension in LANE_FORMATS {
        let file = worker.path().join(format!("lanes.{extension}"));
        let text = if extension == "json" {
            r#"{"lanes":[{"name":"L-SRC","globs":["src/**"]}]}"#
        } else {
            "[[lanes]]\nname = \"L-SRC\"\nglobs = [\"src/**\"]\n"
        };
        std::fs::write(&file, text).unwrap();
        let out = local
            .run(&worker, &["lanes", "set", file.to_str().unwrap()])
            .await;
        assert_exit(&out, 0);
        assert!(stdout(&out).contains("L-SRC: src/**"));
    }
    let out = local
        .run(
            &worker,
            &["check-path", "--json", "src/a.rs", "docs/a.md", "../escape"],
        )
        .await;
    assert_exit(&out, 0);
    let answers: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        answers,
        json!([
            {"path":"src/a.rs", "lane":"L-SRC", "allowed":false, "reason":"not claimed"},
            {"path":"docs/a.md", "lane":null, "allowed":true, "reason":"unowned"},
            {"path":"../escape", "lane":null, "allowed":false, "reason":"not a repository path"}
        ])
    );
    let out = local
        .run(&worker, &["found", "source fix", "--lane", "L-SRC"])
        .await;
    assert_exit(&out, 0);
    let out = local.run(&worker, &["claim", "B-1"]).await;
    assert_exit(&out, 0);
    let out = local
        .run(&worker, &["check-path", "--json", "src/a.rs"])
        .await;
    assert_exit(&out, 0);
    let answers: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(answers[0]["allowed"], true);
    local.stop().await;
}

/// The local token is a credential an agent's terminal must never show, on
/// success, on refusal and with logging at its loudest.
#[tokio::test]
async fn local_token_never_reaches_output() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    let id = worker.identity().identity_id;
    let token = read_local_credential(&local.home.tokens(), &id)
        .unwrap()
        .unwrap()
        .local_token;
    let wrong = "0".repeat(64);
    // The stored token, and a well-formed one agmend refuses.
    let cases: [(&[&str], &str, bool); 5] = [
        (&["items"], &token, true),
        (&["claim", "B-99"], &token, false),
        (&["post", "NOPE", "text"], &token, false),
        (&["items"], &wrong, false),
        (&["post", "FYI", "text"], &wrong, false),
    ];
    for (args, supplied, succeeds) in cases {
        let env = [
            ("AGMEN_LOG", "trace"),
            ("AGMEN_IDENTITY", id.as_str()),
            ("AGMEN_LOCAL_TOKEN", supplied),
        ];
        let out = local.run_env(&worker, args, &env).await;
        assert_eq!(out.status.success(), succeeds, "{args:?}: {}", stderr(&out));
        for text in [stdout(&out), stderr(&out)] {
            assert!(!text.contains(&token), "{args:?}: {text}");
            assert!(!text.contains(&wrong), "{args:?}: {text}");
        }
    }
    local.stop().await;
}

#[tokio::test]
async fn agmen_watch_prints_local_messages() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    let sender = local.member("sender").await;
    let token = read_local_credential(&local.home.tokens(), &worker.identity().identity_id)
        .unwrap()
        .unwrap()
        .local_token;
    for args in [&["watch"][..], &["watch", "--json"][..]] {
        let mut command = tokio::process::Command::from(local.home.agmen(&worker.path(), args));
        let mut child = command
            .env("AGMEN_LOG", "trace")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        tokio::time::timeout(MCP_WAIT, async {
            let first = lines.next_line().await.unwrap().expect("watch banner");
            assert!(!first.contains(&token));
            if args.len() == 2 {
                let me: Value = serde_json::from_str(&first).unwrap();
                assert_eq!(me["kind"], "self");
                assert_eq!(me["identity_id"], json!(worker.identity().identity_id));
            } else {
                assert!(first.starts_with("watching as "), "{first}");
            }
            wait_local_status(&local, &worker, "online").await;
            assert_exit(
                &local
                    .run(&sender, &["post", "FYI", "local watch feed"])
                    .await,
                0,
            );
            let mut feed = false;
            let mut inbox = false;
            person_message(&local, &worker, "local watch inbox").await;
            for _ in 0..WATCH_EVENTS {
                let line = lines.next_line().await.unwrap().expect("watch event");
                assert!(!line.contains(&token));
                if args.len() == 2 {
                    let event: Value = serde_json::from_str(&line).unwrap();
                    assert_ne!(event["kind"], "self", "one self line per process");
                    if event["kind"] == "message" {
                        feed |= event["message"]["body"] == "local watch feed";
                        inbox |= event["message"]["body"] == "local watch inbox";
                    }
                } else {
                    feed |= line.contains("local watch feed");
                    inbox |= line.contains("local watch inbox");
                }
                if feed && inbox {
                    return;
                }
            }
            panic!("watch did not print both messages");
        })
        .await
        .unwrap();
        child.start_kill().unwrap();
        let output = tokio::time::timeout(MCP_WAIT, child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(!stderr(&output).contains(&token));
        wait_local_status(&local, &worker, "offline").await;
    }
    local.stop().await;
}

/// An agmend from before watch resume, standing in for the real one.
mod older_agmend {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use agmen_local::{ErrorCode, Request, Response, frame};
    use serde_json::{Value, json};
    use tokio::net::UnixListener;
    use tokio::task::{JoinHandle, JoinSet};

    /// What the agmend of 5d8f266 answers a `watch` that carries `after`: its
    /// params are `{project}` with `deny_unknown_fields`, which the console
    /// wraps as `bad params: {error}`.
    const REFUSAL: &str = "bad params: unknown field `after`, expected `project`";

    fn message(seq: u64) -> Value {
        json!({
            "kind": "message",
            "message": {
                "v": 1, "id": format!("01JBX6Q4T7W3C2M8Y9K5N0P{seq:03}"), "verb": "FYI",
                "data": {}, "body": format!("message {seq}"),
                "sent_at": "2026-10-25T16:04:11Z", "stream": "feed", "seq": seq,
                "from": "idother1", "from_handle": "x/y", "from_kind": "agent",
                "hub_at": "2026-10-25T16:04:11Z",
            },
        })
    }

    /// Serves `hello`, `post` and `watch` at `socket`. A `watch` with `after`
    /// is refused. The others deliver one new message each, seqs 10, 11, 12
    /// and so on; every even one then cuts the connection.
    pub fn start(socket: &Path, me: Value) -> JoinHandle<()> {
        let listener = UnixListener::bind(socket).unwrap();
        std::fs::set_permissions(socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        let opened = Arc::new(AtomicU64::new(0));
        tokio::spawn(async move {
            let mut connections = JoinSet::new();
            while let Ok((mut socket, _)) = listener.accept().await {
                let (me, opened) = (me.clone(), opened.clone());
                connections.spawn(async move {
                    while let Ok(request) = frame::read_async::<Request>(&mut socket).await {
                        let id = request.id;
                        let result = match request.method.as_str() {
                            "hello" => json!({"api": 1}),
                            "post" => json!({"stream": "feed", "seq": 1}),
                            "watch" if request.params.get("after").is_some() => {
                                let refusal = Response::err(id, ErrorCode::BadRequest, REFUSAL);
                                let _sent = frame::write_async(&mut socket, &refusal).await;
                                return;
                            }
                            "watch" => {
                                let n = opened.fetch_add(1, Ordering::SeqCst);
                                for result in [me.clone(), message(10 + n)] {
                                    let ok = Response::ok(id, result);
                                    if frame::write_async(&mut socket, &ok).await.is_err() {
                                        return;
                                    }
                                }
                                if n % 2 == 1 {
                                    // Held open until the CLI closes its end.
                                    let _end = frame::read_async::<Request>(&mut socket).await;
                                }
                                return;
                            }
                            _ => return,
                        };
                        if frame::write_async(&mut socket, &Response::ok(id, result))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        })
    }
}

#[tokio::test]
async fn agmen_watch_on_an_older_agmend_says_so_once_on_stderr() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    let identity = worker.identity();
    let me = json!({
        "kind": "self", "identity_id": identity.identity_id,
        "handle": identity.handle, "owner_id": identity.owner_id,
    });
    // The real daemon keeps running, out of reach of the CLI.
    let socket = local.home.state_base().join("agmend.sock");
    std::fs::remove_file(&socket).unwrap();
    let older = older_agmend::start(&socket, me);
    // Each run reads two messages, the second after a reconnect, and the
    // seqs only grow: 10 and 11, then 12 and 13.
    let runs = [(&["watch"][..], 10), (&["watch", "--json"][..], 12)];
    for (args, first) in runs {
        let mut command = tokio::process::Command::from(local.home.agmen(&worker.path(), args));
        // Off, so a line on stderr is not a log line the filter let through.
        let mut child = command
            .env("AGMEN_LOG", "off")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let wanted = [format!("message {first}"), format!("message {}", first + 1)];
        let mut printed = Vec::new();
        let read = tokio::time::timeout(MCP_WAIT, async {
            while !wanted
                .iter()
                .all(|want| printed.iter().any(|line: &String| line.contains(want)))
            {
                // A watch that ends early is reported below with its stderr.
                let Some(line) = lines.next_line().await.unwrap() else {
                    break;
                };
                printed.push(line);
            }
        })
        .await;
        child.start_kill().unwrap();
        let output = tokio::time::timeout(MCP_WAIT, child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        let err = stderr(&output);
        let all_printed = wanted
            .iter()
            .all(|want| printed.iter().any(|line| line.contains(want)));
        assert!(
            read.is_ok() && all_printed,
            "{args:?} did not print {wanted:?}\nstdout: {printed:?}\nstderr: {err}"
        );
        if args.len() == 2 {
            for line in &printed {
                assert!(
                    serde_json::from_str::<Value>(line).is_ok(),
                    "stdout is not JSON lines: {line}"
                );
            }
        }
        assert!(
            !printed.iter().any(|line| line.contains("older")),
            "stdout carries the notice: {printed:?}"
        );
        let notices = err
            .lines()
            .filter(|l| l.contains("agmend is older"))
            .count();
        assert_eq!(notices, 1, "{args:?}: {err}");
    }
    older.abort();
    local.stop().await;
}

fn local_mcp_options(local: &Local, worktree: &Worktree) -> McpOptions {
    McpOptions::new(
        worktree.path(),
        local.home.tokens(),
        local.home.state_base(),
    )
}

async fn person_message(local: &Local, worker: &Worktree, body: &str) {
    let now = Timestamp::now();
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).unwrap();
    let envelope = ClientEnvelope {
        v: 1,
        id: format!("{:026X}", u64::from_be_bytes(random))
            .parse()
            .unwrap(),
        reply_to: None,
        to: vec![worker.identity().identity_id],
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: body.try_into().unwrap(),
        valid_until: None,
        sent_at: now,
        payload: Payload::Dm {},
    };
    local
        .call(
            "post",
            json!({"project": local.project, "envelope": envelope}),
        )
        .await;
}

async fn wait_local_status(local: &Local, worker: &Worktree, status: &str) {
    for _ in 0..LOCAL_POLLS {
        let roster = local
            .call("state", json!({"project": local.project, "key": "roster"}))
            .await;
        if roster["value"].as_array().unwrap().iter().any(|row| {
            row["identity_id"] == json!(worker.identity().identity_id) && row["status"] == status
        }) {
            return;
        }
        tokio::time::sleep(LOCAL_POLL).await;
    }
    panic!("local identity did not become {status}");
}

#[tokio::test]
async fn mcp_tools_work_on_a_local_identity() {
    tokio::time::timeout(MCP_WAIT, async {
        let local = Local::new().await;
        let worker = local.member("worker").await;
        let mcp = Mcp::start(local_mcp_options(&local, &worker)).await;
        let chat = mcp
            .text("chat", json!({"text": "hello from local MCP"}))
            .await;
        assert!(chat.starts_with("recorded #1 (feed)"), "{chat}");
        assert!(!chat.contains("queued"));
        let history = mcp.text("read_chat", json!({})).await;
        assert!(history.contains("hello from local MCP"), "{history}");
        assert!(
            mcp.text("found", json!({"title": "local MCP work"}))
                .await
                .contains("B-1")
        );
        let claim = mcp.text("claim", json!({"item": "B-1"})).await;
        assert!(claim.contains("granted B-1 until"), "{claim}");
        assert!(
            mcp.text("state", json!({"key": "roster"}))
                .await
                .contains(&worker.identity().handle)
        );
        assert!(
            mcp.text("state", json!({"key": "head"}))
                .await
                .starts_with("state/head:")
        );
        assert!(mcp.text("lanes", json!({})).await.contains("no lanes"));
        assert!(
            mcp.text("check_path", json!({"paths": ["src/a.rs"]}))
                .await
                .contains("unowned")
        );
        wait_local_status(&local, &worker, "online").await;
        person_message(&local, &worker, "person DM for pending block").await;
        let pending = mcp.text("lanes", json!({})).await;
        assert!(
            pending.contains("for you, from other participants"),
            "{pending}"
        );
        assert!(pending.contains("person DM for pending block"), "{pending}");
        assert!(
            !mcp.text("lanes", json!({}))
                .await
                .contains("person DM for pending block")
        );
        let token = read_local_credential(&local.home.tokens(), &worker.identity().identity_id)
            .unwrap()
            .unwrap()
            .local_token;
        for text in [&chat, &history, &claim, &pending] {
            assert!(!text.contains(&token));
        }
        mcp.stop().await;
        wait_local_status(&local, &worker, "offline").await;
        local.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn mcp_heartbeat_posts_locally() {
    tokio::time::timeout(MCP_WAIT, async {
        let local = Local::new().await;
        let worker = local.member("worker").await;
        assert_exit(
            &local.run(&worker, &["found", "hold for heartbeat"]).await,
            0,
        );
        assert_exit(&local.run(&worker, &["claim", "B-1"]).await, 0);
        let mcp =
            Mcp::start(local_mcp_options(&local, &worker).with_heartbeat(HEARTBEAT_EVERY)).await;
        // agmen-core routes a HEARTBEAT to the sender's own inbox only; what it
        // does there is renew the claim's lease (crates/agmend/tests/local_origin.rs).
        let inbox = Stream::Inbox(worker.identity().identity_id);
        let mut beat = None;
        for _ in 0..HEARTBEAT_POLLS {
            beat = identity_page(&local, &worker, &inbox)
                .await
                .into_iter()
                .find(|message| {
                    matches!(&message.client.payload, Payload::Heartbeat { items }
                        if items.iter().any(|item| item.as_str() == "B-1"))
                });
            if beat.is_some() {
                break;
            }
            tokio::time::sleep(LOCAL_POLL).await;
        }
        let beat = beat.expect("MCP did not record a local HEARTBEAT without a tool call");
        assert_eq!(beat.from, worker.identity().identity_id);
        assert_eq!(beat.stream, inbox);
        mcp.stop().await;
        local.stop().await;
    })
    .await
    .unwrap();
}

/// A page of `stream` read as `worker`: agmend's `page` with the worker's own
/// local token, the call `agmen inbox` makes.
async fn identity_page(local: &Local, worker: &Worktree, stream: &Stream) -> Vec<StampedEnvelope> {
    use agmen_local::{API, Outcome, Request, Response, frame};

    let id = worker.identity().identity_id;
    let token = read_local_credential(&local.home.tokens(), &id)
        .unwrap()
        .unwrap()
        .local_token;
    let socket = local.home.state_base().join("agmend.sock");
    let mut socket = tokio::net::UnixStream::connect(socket).await.unwrap();
    frame::write_async(&mut socket, &Request::new(1, "hello", json!({"api": API})))
        .await
        .unwrap();
    let hello: Response = frame::read_async(&mut socket).await.unwrap();
    assert!(matches!(hello.outcome, Outcome::Ok { .. }));
    let mut request = Request::new(2, "page", json!({"stream": stream, "local_token": token}));
    request.as_ = Some(id);
    frame::write_async(&mut socket, &request).await.unwrap();
    let response: Response = frame::read_async(&mut socket).await.unwrap();
    match response.outcome {
        Outcome::Ok { result } => serde_json::from_value(result["messages"].clone()).unwrap(),
        Outcome::Err { error } => panic!("page: {error:?}"),
    }
}

/// A refusal reads as it does from a hub (`rejected: <reason>`, a text
/// answer, never `queued`), and records nothing.
#[tokio::test]
async fn local_mcp_rejection_reads_like_a_hub() {
    tokio::time::timeout(MCP_WAIT, async {
        let local = Local::new().await;
        let worker = local.member("worker").await;
        let mcp = Mcp::start(local_mcp_options(&local, &worker)).await;
        // Built in two parts so no credential-shaped literal is in the source.
        let secret = format!("{}{}", "ghp_", "a".repeat(36));
        let answer = mcp.text("chat", json!({"text": secret})).await;
        assert_eq!(answer, "rejected: secret_detected");
        let feed = identity_page(&local, &worker, &Stream::Feed).await;
        assert!(feed.is_empty(), "{feed:?}");
        mcp.stop().await;
        local.stop().await;
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn local_watch_and_mcp_keep_tokens_out_of_output() {
    use rmcp::ServiceExt as _;
    use rmcp::model::CallToolRequestParams;
    use support::mcp::text_of;

    let local = Local::new().await;
    let worker = local.member("worker").await;
    let id = worker.identity().identity_id;
    let token = read_local_credential(&local.home.tokens(), &id)
        .unwrap()
        .unwrap()
        .local_token;
    let wrong = "0".repeat(64);
    let env = [
        ("AGMEN_LOG", "trace"),
        ("AGMEN_IDENTITY", id.as_str()),
        ("AGMEN_LOCAL_TOKEN", wrong.as_str()),
    ];
    let out = local.run_env(&worker, &["watch", "--json"], &env).await;
    assert!(!out.status.success());
    for text in [stdout(&out), stderr(&out)] {
        assert!(!text.contains(&token));
        assert!(!text.contains(&wrong));
    }
    for supplied in [&token, &wrong] {
        let mut command = tokio::process::Command::from(local.home.agmen(&worker.path(), &["mcp"]));
        let mut child = command
            .env("AGMEN_LOG", "trace")
            .env("AGMEN_IDENTITY", id.as_str())
            .env("AGMEN_LOCAL_TOKEN", supplied)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let client = tokio::time::timeout(
            MCP_WAIT,
            ().serve((child.stdout.take().unwrap(), child.stdin.take().unwrap())),
        )
        .await
        .unwrap()
        .unwrap();
        let result = tokio::time::timeout(
            MCP_WAIT,
            client.call_tool(
                CallToolRequestParams::new("chat")
                    .with_arguments(json!({"text": "binary MCP"}).as_object().unwrap().clone()),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.is_error, Some(supplied == &wrong));
        let text = text_of(&result);
        assert!(!text.contains(&token));
        assert!(!text.contains(&wrong));
        tokio::time::timeout(MCP_WAIT, client.cancel())
            .await
            .unwrap()
            .unwrap();
        let out = tokio::time::timeout(MCP_WAIT, child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_exit(&out, 0);
        for text in [stdout(&out), stderr(&out)] {
            assert!(!text.contains(&token));
            assert!(!text.contains(&wrong));
        }
    }
    local.stop().await;
}

#[tokio::test]
async fn local_mcp_surfaces_busy_and_stopped_without_hub_fallback() {
    use agmen_local::{API, ErrorCode, Request, Response, frame};
    use std::os::unix::fs::PermissionsExt as _;

    tokio::time::timeout(MCP_WAIT, async {
        let local = Local::new().await;
        let worker = local.member("worker").await;
        let mcp = Mcp::start(local_mcp_options(&local, &worker)).await;
        mcp.text("chat", json!({"text": "before busy"})).await;
        // Keep the established watch, and supply exact daemon refusals for new calls.
        let socket = local.home.state_base().join("agmend.sock");
        std::fs::rename(&socket, socket.with_extension("saved")).unwrap();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        const ERRORS: [&str; 2] = [
            "local.db is busy; try again",
            "the local origin has stopped",
        ];
        let daemon = tokio::spawn(async move {
            for message in ERRORS {
                let (mut stream, _) = listener.accept().await.unwrap();
                let hello: Request = frame::read_async(&mut stream).await.unwrap();
                frame::write_async(&mut stream, &Response::ok(hello.id, json!({"api": API})))
                    .await
                    .unwrap();
                let call: Request = frame::read_async(&mut stream).await.unwrap();
                assert_eq!(call.method, "post");
                frame::write_async(
                    &mut stream,
                    &Response::err(call.id, ErrorCode::Hub, message),
                )
                .await
                .unwrap();
            }
        });
        for message in ERRORS {
            let answer = mcp.call("chat", json!({"text": "while unavailable"})).await;
            assert_eq!(answer.is_error, Some(true));
            let text = support::mcp::text_of(&answer);
            assert!(text.contains(message), "{text}");
            assert!(!text.contains("queued"));
        }
        daemon.await.unwrap();
        mcp.stop().await;
        local.stop().await;
    })
    .await
    .unwrap();
}

/// A file the trust test refuses is not a local worktree, so a hub command
/// reaches its sign-in check as it did before local worktrees existed.
#[tokio::test]
async fn untrusted_identity_file_does_not_block_hub_commands() {
    let home = Home::new();
    let worktree = Worktree::new(&home);
    // `.agmen/` is not in .git/info/exclude, so git may track the file.
    std::fs::create_dir(worktree.path().join(".agmen")).unwrap();
    std::fs::write(worktree.path().join(".agmen/identity.json"), "{}").unwrap();
    const COMMANDS: [&[&str]; 6] = [
        &["invite"],
        &["group", "members"],
        &["project", "create", "test"],
        &["whoami", "--json"],
        &["chat", "--project", "projectp1", "hello"],
        &["chat", "hello"],
    ];
    for args in COMMANDS {
        let out = home.run(&worktree.path(), args).await;
        assert_exit(&out, 3);
        assert!(
            stderr(&out).contains("not signed in"),
            "{args:?}: {}",
            stderr(&out)
        );
    }
}

#[tokio::test]
async fn hub_admin_commands_refuse_on_local() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    const COMMANDS: [&[&str]; 5] = [
        &["invite"],
        &["group", "members"],
        &["project", "create", "test"],
        &["whoami", "--json"],
        &["leave"],
    ];
    for args in COMMANDS {
        let out = local.run(&worker, args).await;
        assert_exit(&out, 2);
        assert!(
            stderr(&out).contains(&format!(
                "{} needs a hub; this worktree is on this machine's local origin",
                args[0]
            )),
            "{}",
            stderr(&out)
        );
    }
    local.stop().await;
}
