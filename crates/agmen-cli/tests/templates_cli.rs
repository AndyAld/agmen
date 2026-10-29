//! Template commands use temporary homes and repositories, without a hub.
#![cfg(test)]

mod support;

use agmen_roles::{NodeAgent, Template, parse_template};
use support::{Home, Worktree, assert_exit, stderr, stdout};

fn personal(home: &Home, file: &str, xml: &str) -> std::path::PathBuf {
    let dir = home.path().join(".config/agmen/templates");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file);
    std::fs::write(&path, xml).unwrap();
    path
}

#[tokio::test]
async fn list_shows_builtins() {
    let home = Home::new();
    let out = home.run(home.path(), &["templates", "list"]).await;
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert_eq!(text.lines().next(), Some("name\tkind\tlayer\tdescription"));
    for name in ["claude", "codex"] {
        assert!(
            text.contains(&format!("{name}\tagent\tbuiltin\t")),
            "{text}"
        );
    }
}

#[tokio::test]
async fn list_names_skipped_files() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    let dir = wt.path().join(".agmen/templates");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("broken.xml"), "<agent-template").unwrap();
    let out = home.run(&wt.path(), &["templates", "list"]).await;
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert!(text.contains("claude\tagent\tbuiltin\t"));
    assert!(
        text.lines()
            .any(|line| line.starts_with("skipped ") && line.contains("broken.xml"))
    );
}

#[tokio::test]
async fn list_json_has_both_arrays() {
    let home = Home::new();
    let out = home
        .run(home.path(), &["templates", "list", "--json"])
        .await;
    assert_exit(&out, 0);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["templates"].as_array().unwrap().len(), 2);
    assert!(value["problems"].as_array().unwrap().is_empty());
    for key in ["name", "kind", "layer", "description", "saved_by", "path"] {
        assert!(value["templates"][0].get(key).is_some(), "{key}");
    }
}

#[tokio::test]
async fn export_inlines_a_team() {
    let home = Home::new();
    personal(
        &home,
        "worker.agent.xml",
        r#"<agent-template name="worker"/>"#,
    );
    personal(
        &home,
        "eng.team.xml",
        r#"<team-template name="eng"><node template="worker"/></team-template>"#,
    );
    let out = home.run(home.path(), &["templates", "export", "eng"]).await;
    assert_exit(&out, 0);
    let Template::Team(team) = parse_template(&stdout(&out)).unwrap() else {
        panic!("team")
    };
    assert!(matches!(team.template.nodes[0].agent, NodeAgent::Inline(_)));
}

#[tokio::test]
async fn export_refuses_to_overwrite() {
    let home = Home::new();
    let file = home.path().join("output.xml");
    std::fs::write(&file, "unchanged").unwrap();
    let args = [
        "templates",
        "export",
        "claude",
        "--out",
        file.to_str().unwrap(),
    ];
    let out = home.run(home.path(), &args).await;
    assert!(!out.status.success());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "unchanged");
    let mut args = args.to_vec();
    args.push("--force");
    let out = home.run(home.path(), &args).await;
    assert_exit(&out, 0);
    assert_eq!(
        parse_template(&std::fs::read_to_string(file).unwrap())
            .unwrap()
            .name(),
        "claude"
    );
}

#[tokio::test]
async fn export_unknown_name_lists_names() {
    let home = Home::new();
    let out = home
        .run(home.path(), &["templates", "export", "nope"])
        .await;
    assert_exit(&out, 2);
    let text = stderr(&out);
    assert!(text.contains("claude") && text.contains("codex"), "{text}");
}

#[tokio::test]
async fn list_cleans_problem_lines_and_bounds_descriptions() {
    let home = Home::new();
    personal(
        &home,
        "bad\u{1b}.xml",
        "<agent-template name=\"bad\" saved-by=\"x&#13;y\"/>",
    );
    personal(
        &home,
        "good.xml",
        &format!(
            "<agent-template name=\"good\"><description>{}\nsecond</description></agent-template>",
            "é".repeat(41)
        ),
    );
    let out = home.run(home.path(), &["templates", "list"]).await;
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert!(!text.contains('\u{1b}') && !text.contains('\r'), "{text:?}");
    let row = text
        .lines()
        .find(|line| line.starts_with("good\t"))
        .unwrap();
    assert_eq!(row.split('\t').nth(3).unwrap(), "é".repeat(40));
    assert!(!text.contains("second"));
}

fn input(home: &Home, xml: &str) -> std::path::PathBuf {
    let path = home.path().join("input.xml");
    std::fs::write(&path, xml).unwrap();
    path
}

#[tokio::test]
async fn import_writes_the_personal_layer() {
    let home = Home::new();
    let path = input(
        &home,
        include_str!("../../agmen-roles/tests/fixtures/templates/rust-reviewer.agent.xml"),
    );
    let out = home
        .run(
            home.path(),
            &[
                "templates",
                "import",
                path.to_str().unwrap(),
                "--keep-options",
            ],
        )
        .await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("imported"));
    let xml = std::fs::read_to_string(
        home.path()
            .join(".config/agmen/templates/rust-reviewer.agent.xml"),
    )
    .unwrap();
    assert_eq!(
        parse_template(&xml).unwrap().option_names(),
        ["context-window"]
    );
    let out = home.run(home.path(), &["templates", "list"]).await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("rust-reviewer\tagent\tpersonal"));
}

#[tokio::test]
async fn import_drops_options_without_keep() {
    let home = Home::new();
    let path = input(
        &home,
        r#"<team-template name="eng"><node><agent-template><options><option name="a" value="1"/><option name="b" value="2"/></options></agent-template></node></team-template>"#,
    );
    let out = home
        .run(
            home.path(),
            &["templates", "import", path.to_str().unwrap()],
        )
        .await;
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("dropped 2 options"));
    let xml =
        std::fs::read_to_string(home.path().join(".config/agmen/templates/eng.team.xml")).unwrap();
    assert!(!xml.contains("<option"));
}

#[tokio::test]
async fn import_lists_options_first() {
    let home = Home::new();
    let path = input(
        &home,
        include_str!("../../agmen-roles/tests/fixtures/templates/rust-reviewer.agent.xml"),
    );
    let out = home
        .run(
            home.path(),
            &["templates", "import", path.to_str().unwrap()],
        )
        .await;
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert!(
        text.find("options:").unwrap() < text.find("agent:").unwrap(),
        "{text}"
    );
    assert!(text.contains("context-window = 1m"));
    assert!(text.contains("pack github:groupdrive/agmen-rust@1a2b3c4: not installed (pack installs arrive in a later phase)"));
    assert!(text.contains("role: reviewer") && !text.contains("reviewer: missing"));
    assert!(text.contains("cc-skill-coding-standards: local"));
}

#[tokio::test]
async fn import_refuses_an_existing_name() {
    let home = Home::new();
    let path = input(&home, r#"<agent-template name="plain"/>"#);
    let args = ["templates", "import", path.to_str().unwrap()];
    assert_exit(&home.run(home.path(), &args).await, 0);
    let target = home.path().join(".config/agmen/templates/plain.agent.xml");
    let original = std::fs::read(&target).unwrap();
    std::fs::write(
        &path,
        r#"<agent-template name="plain"><description>new</description></agent-template>"#,
    )
    .unwrap();
    let out = home.run(home.path(), &args).await;
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--force"));
    assert_eq!(std::fs::read(&target).unwrap(), original);
    let mut args = args.to_vec();
    args.push("--force");
    assert_exit(&home.run(home.path(), &args).await, 0);
    assert!(std::fs::read_to_string(target).unwrap().contains("new"));
}

#[tokio::test]
async fn import_rejects_a_bad_file() {
    let home = Home::new();
    let path = input(
        &home,
        r#"<agent-template name="plain"><hooks/></agent-template>"#,
    );
    let out = home
        .run(
            home.path(),
            &["templates", "import", path.to_str().unwrap()],
        )
        .await;
    assert!(!out.status.success());
    assert!(stderr(&out).contains("hooks"));
    assert!(!home.path().join(".config/agmen/templates").exists());
}

#[tokio::test]
async fn import_rejects_oversized_input() {
    let home = Home::new();
    let path = input(&home, &"x".repeat(agmen_roles::MAX_TEMPLATE_BYTES + 1));
    let out = home
        .run(
            home.path(),
            &["templates", "import", path.to_str().unwrap()],
        )
        .await;
    assert_exit(&out, 2);
    assert!(stderr(&out).contains("template file exceeds 32 KiB"));
    assert!(!home.path().join(".config/agmen/templates").exists());
}

#[tokio::test]
async fn import_reports_missing_references() {
    let home = Home::new();
    let path = input(
        &home,
        r#"<team-template name="eng"><node template="nope"/><node><agent-template><role name="unknown"/><mcp><server name="docs"/></mcp></agent-template></node></team-template>"#,
    );
    let out = home
        .run(
            home.path(),
            &["templates", "import", path.to_str().unwrap()],
        )
        .await;
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert!(text.contains("node nope: not found here"), "{text}");
    assert!(text.contains("role: unknown: missing"), "{text}");
    assert!(text.contains("MCP docs: local"), "{text}");
}

#[cfg(unix)]
mod save {
    use super::*;
    use agmen_local::{API, Request, Response, frame};
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // shortcut: copied from tests/workers.rs; move both to tests/support once the workers lane settles.
    struct Fake {
        requests: Arc<Mutex<Vec<Request>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Fake {
        fn start(home: &Home, answer: Value) -> Self {
            let path = home.state_base().join(agmen_client::AGMEND_SOCKET);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded = requests.clone();
            let task = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(10), async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    for _ in 0..4 {
                        let Ok(request) = frame::read_async::<Request>(&mut socket).await else {
                            return;
                        };
                        recorded.lock().unwrap().push(request.clone());
                        let answer = if request.method == "hello" {
                            Response::ok(request.id, json!({"api":API}))
                        } else {
                            assert_eq!(request.method, "agent_list");
                            Response::ok(request.id, answer.clone())
                        };
                        frame::write_async(&mut socket, &answer).await.unwrap();
                    }
                })
                .await
                .unwrap();
            });
            Self { requests, task }
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    #[tokio::test]
    async fn save_from_a_running_agent() {
        let base = tempfile::tempdir_in("/tmp").unwrap();
        let home = Home::new_in(base.path());
        let fake = Fake::start(
            &home,
            json!({"agents":[{"identity":"agent001", "model":"sonnet", "role":"engineer", "budget_usd":"5"}]}),
        );
        let out = home
            .run(
                home.path(),
                &[
                    "templates",
                    "save",
                    "sol-copy",
                    "--from",
                    "agent001",
                    "--description",
                    "A copy",
                ],
            )
            .await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert!(text.starts_with("<?xml"), "{text}");
        assert!(
            text.contains("saved ") && text.contains("skills and MCP servers are not recorded")
        );
        {
            let requests = fake.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1].method, "agent_list");
            assert_eq!(requests[1].params, json!({}));
        }
        let out = home.run(home.path(), &["templates", "list"]).await;
        assert_exit(&out, 0);
        assert!(stdout(&out).contains("sol-copy\tagent\tpersonal"));
    }

    #[tokio::test]
    async fn save_unknown_identity_fails() {
        let base = tempfile::tempdir_in("/tmp").unwrap();
        let home = Home::new_in(base.path());
        let _fake = Fake::start(&home, json!({"agents":[]}));
        let out = home
            .run(
                home.path(),
                &["templates", "save", "sol-copy", "--from", "agent001"],
            )
            .await;
        assert_exit(&out, 2);
        assert!(stderr(&out).contains("not one of your agents"));
        assert!(!home.path().join(".config/agmen/templates").exists());
    }

    #[tokio::test]
    async fn save_respects_force_and_preserves_existing_template() {
        let base = tempfile::tempdir_in("/tmp").unwrap();
        let home = Home::new_in(base.path());
        let original = r#"<agent-template name="sol-copy"><description>original</description></agent-template>"#;
        let path = personal(&home, "sol-copy.agent.xml", original);
        let record = json!({"agents":[{"identity":"agent001", "model":"sonnet"}]});
        let fake = Fake::start(&home, record.clone());
        let args = ["templates", "save", "sol-copy", "--from", "agent001"];
        let out = home.run(home.path(), &args).await;
        assert!(!out.status.success());
        assert!(stderr(&out).contains("--force"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        drop(fake);
        std::fs::remove_file(home.state_base().join(agmen_client::AGMEND_SOCKET)).unwrap();
        let _fake = Fake::start(&home, record);
        let mut args = args.to_vec();
        args.push("--force");
        let out = home.run(home.path(), &args).await;
        assert_exit(&out, 0);
        assert!(std::fs::read_to_string(path).unwrap().contains("sonnet"));
    }
}

// macOS rejects non-UTF-8 filenames before discovery; unit tests cover synthetic paths there.
#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
async fn list_json_handles_non_utf8_source_paths() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    let home = Home::new();
    let dir = home.path().join(".config/agmen/templates");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(OsString::from_vec(b"good\xff.xml".to_vec())),
        r#"<agent-template name="plain"/>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join(OsString::from_vec(b"bad\xff.xml".to_vec())),
        "<agent-template",
    )
    .unwrap();
    let out = home
        .run(home.path(), &["templates", "list", "--json"])
        .await;
    assert_exit(&out, 0);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = value["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "plain")
        .unwrap();
    assert!(row["path"].as_str().unwrap().contains("good\u{fffd}.xml"));
    assert!(
        value["problems"][0]["path"]
            .as_str()
            .unwrap()
            .contains("bad\u{fffd}.xml")
    );
}
