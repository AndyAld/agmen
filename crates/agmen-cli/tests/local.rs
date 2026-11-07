//! CLI commands against an in-process local origin, without a hub or broker.

#![cfg(test)]

#[path = "support/local.rs"]
mod local_support;
mod support;

use agmen_client::{TokenKey, TokenStore, read_local_credential};
use local_support::Local;
use serde_json::{Value, json};
use support::{Home, Worktree, assert_exit, stderr, stdout};

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

/// Task 12 gives `agmen watch` a local source and removes this test.
#[tokio::test]
async fn watch_refuses_on_local_until_supported() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    for args in [&["watch"][..], &["watch", "--json"][..]] {
        let out = local.run(&worker, args).await;
        assert_exit(&out, 2);
        assert!(
            stderr(&out).contains(
                "agmen watch does not support local projects yet; this worktree is on this machine's local origin"
            ),
            "{}",
            stderr(&out)
        );
        assert_eq!(stdout(&out), "", "watch prints nothing before it refuses");
    }
    local.stop().await;
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
