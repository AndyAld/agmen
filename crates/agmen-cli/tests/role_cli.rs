//! Role commands use isolated homes and temporary repositories, without a hub.
#![cfg(test)]

mod support;

use support::{Home, Worktree, assert_exit, stderr, stdout};

fn project_role(wt: &Worktree, name: &str, xml: &str) {
    let dir = wt.path().join(".agmen/roles");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.xml")), xml).unwrap();
}

#[tokio::test]
async fn role_list_shows_builtins() {
    let home = Home::new();
    let out = home.run(home.path(), &["role", "list"]).await;
    assert_exit(&out, 0);
    let text = stdout(&out);
    let rows: Vec<_> = text.lines().skip(1).collect();
    assert_eq!(rows.len(), 7, "{text}");
    assert!(!rows.iter().any(|row| row.starts_with("base\t")));
    let reviewer = rows
        .iter()
        .find(|row| row.starts_with("reviewer\t"))
        .unwrap();
    assert_eq!(reviewer.split('\t').nth(3), Some("deep"));
}

#[tokio::test]
async fn role_list_json_has_layers() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    project_role(&wt, "reviewer", include_str!("../../../roles/reviewer.xml"));
    let out = home.run(&wt.path(), &["role", "list", "--json"]).await;
    assert_exit(&out, 0);
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows.len(), 7);
    for row in &rows {
        for key in ["name", "layer", "kind", "tier", "tools", "verbs"] {
            assert!(row.get(key).is_some(), "{row}");
        }
        assert!(row["tools"].is_array());
        assert!(row["verbs"].is_array());
    }
    let reviewer = rows.iter().find(|row| row["name"] == "reviewer").unwrap();
    assert_eq!(reviewer["layer"], "project");
    assert_eq!(reviewer["tier"], "deep");
}

#[tokio::test]
async fn role_show_reports_stripped() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    project_role(
        &wt,
        "listener",
        r#"<role name="listener" kind="listener" tier="small"><tools allow="read shell"/></role>"#,
    );
    let out = home.run(&wt.path(), &["role", "show", "listener"]).await;
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert!(text.contains("stripped: tool shell"), "{text}");
    assert!(text.contains("<purpose>"), "{text}");
}

#[tokio::test]
async fn role_show_unknown_name_fails() {
    let home = Home::new();
    let out = home.run(home.path(), &["role", "show", "nope"]).await;
    assert_exit(&out, 2);
    let text = stderr(&out);
    for name in ["engineer", "reviewer", "coordinator", "listener"] {
        assert!(text.contains(name), "{text}");
    }
}

#[tokio::test]
async fn role_import_writes_xml() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    let input = home.path().join("subagent.md");
    std::fs::write(
        &input,
        include_str!("../../agmen-roles/tests/fixtures/subagent.md"),
    )
    .unwrap();
    let args = ["role", "import", input.to_str().unwrap(), "--to", "project"];
    let out = home.run(&wt.path(), &args).await;
    assert_exit(&out, 0);
    let path = wt.path().join(".agmen/roles/subagent.xml");
    let xml = std::fs::read_to_string(&path).unwrap();
    assert_eq!(agmen_roles::parse(&xml).unwrap().name, "subagent");
    let out = home.run(&wt.path(), &args).await;
    assert!(!out.status.success());
    assert_eq!(std::fs::read_to_string(path).unwrap(), xml);
}

#[tokio::test]
async fn role_import_personal_uses_config_roles() {
    let home = Home::new();
    let input = home.path().join("subagent.md");
    std::fs::write(
        &input,
        include_str!("../../agmen-roles/tests/fixtures/subagent.md"),
    )
    .unwrap();
    let out = home
        .run(
            home.path(),
            &[
                "role",
                "import",
                input.to_str().unwrap(),
                "--to",
                "personal",
                "--name",
                "custom",
            ],
        )
        .await;
    assert_exit(&out, 0);
    let xml = std::fs::read_to_string(home.path().join(".config/agmen/roles/custom.xml")).unwrap();
    assert_eq!(agmen_roles::parse(&xml).unwrap().name, "custom");
}

#[tokio::test]
async fn join_role_conflicts_with_kind() {
    let home = Home::new();
    let out = home
        .run(
            home.path(),
            &[
                "join",
                "--project",
                "project1",
                "--role",
                "reviewer",
                "--kind",
                "agent",
            ],
        )
        .await;
    assert_exit(&out, 2);
    assert!(stderr(&out).contains("cannot be used with"));
}

#[tokio::test]
async fn join_refuses_listener_role() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    let out = home
        .run(
            &wt.path(),
            &["join", "--project", "project1", "--role", "listener"],
        )
        .await;
    assert_exit(&out, 2);
    assert!(stderr(&out).contains("listeners are started by agmend, not agmen join"));
    assert!(!wt.path().join(".agmen/identity.json").exists());
}

#[tokio::test]
async fn role_import_rejects_oversized_input_and_invalid_names() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    let input = home.path().join("subagent.md");
    std::fs::write(&input, "x".repeat(65537)).unwrap();
    let out = home
        .run(&wt.path(), &["role", "import", input.to_str().unwrap()])
        .await;
    assert_exit(&out, 2);
    assert!(stderr(&out).contains("64 KiB"));
    assert!(!wt.path().join(".agmen/roles/subagent.xml").exists());
    std::fs::write(
        &input,
        include_str!("../../agmen-roles/tests/fixtures/subagent.md"),
    )
    .unwrap();
    let out = home
        .run(
            &wt.path(),
            &[
                "role",
                "import",
                input.to_str().unwrap(),
                "--name",
                "../escape",
            ],
        )
        .await;
    assert_exit(&out, 2);
    assert!(stderr(&out).contains("invalid"));
    assert!(!wt.path().join(".agmen/escape.xml").exists());
}

#[tokio::test]
async fn role_show_sanitizes_prompt_text() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    project_role(
        &wt,
        "custom",
        "<role name=\"custom\" kind=\"agent\" tier=\"deep\"><purpose>safe\u{202e}text</purpose></role>",
    );
    let out = home.run(&wt.path(), &["role", "show", "custom"]).await;
    assert_exit(&out, 0);
    let text = stdout(&out);
    assert!(text.contains("safetext"), "{text}");
    assert!(!text.contains('\u{202e}'));
}

#[cfg(unix)]
#[tokio::test]
async fn role_import_rejects_project_directory_links_outside_checkout() {
    let home = Home::new();
    let input = home.path().join("subagent.md");
    std::fs::write(
        &input,
        include_str!("../../agmen-roles/tests/fixtures/subagent.md"),
    )
    .unwrap();
    for link_agmen in [false, true] {
        let wt = Worktree::new(&home);
        let outside = tempfile::tempdir().unwrap();
        let link = if link_agmen {
            wt.path().join(".agmen")
        } else {
            std::fs::create_dir(wt.path().join(".agmen")).unwrap();
            wt.path().join(".agmen/roles")
        };
        std::os::unix::fs::symlink(outside.path(), link).unwrap();
        let out = home
            .run(&wt.path(), &["role", "import", input.to_str().unwrap()])
            .await;
        assert!(!out.status.success(), "{}", stdout(&out));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
