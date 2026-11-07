//! CLI commands against an in-process local origin, without a hub or broker.

#![cfg(test)]

#[path = "support/local.rs"]
mod local_support;
mod support;

use agmen_client::{TokenStore, read_local_credential};
use local_support::Local;
use serde_json::{Value, json};
use support::{Home, Worktree, assert_exit, stderr, stdout};

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
