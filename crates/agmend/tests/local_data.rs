//! Task 8: roles, worktrees, git facts and file text from the person's
//! checkout, which `console_snapshot` and `file_text` read from disk.

#![cfg(test)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use agmen_local::ErrorCode;
use agmen_local::console::{
    ChangeKindEntry, ConsoleSnapshot, FileTextEntry, HeadEntry, MarkEntry, RoleLayer, WorktreeEntry,
};
use serde_json::json;
use support::{Daemon, PROJECT, error, result};

/// An identity file `agmen join` would write for `project`.
fn identity(project: &str) -> String {
    json!({
        "tenant": "tenant01", "project": project, "identity_id": "idn7k2q9",
        "handle": "andy/sol", "owner_id": "hmn7k2q9",
        "hub_url": "https://api.agmen.example",
        "broker_url": "wss://agmen.example/mqtt"
    })
    .to_string()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=Andy A", "-c", "user.email=a@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// A temporary folder under target/, for a checkout and its neighbours.
fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ld")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

/// A checkout of `project` that `agmen join` set up, with one commit of
/// `files`.
fn checkout(at: &Path, project: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = at.join("main");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);
    std::fs::create_dir(root.join(".agmen")).unwrap();
    std::fs::write(root.join(".agmen/identity.json"), identity(project)).unwrap();
    std::fs::write(root.join(".git/info/exclude"), ".agmen/identity.json\n").unwrap();
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "First"]);
    // git's own spelling of the root, as the worktree list gives it.
    PathBuf::from(git(&root, &["rev-parse", "--show-toplevel"]).trim())
}

async fn snapshot(daemon: &Daemon, root: &Path) -> ConsoleSnapshot {
    let mut client = daemon.client().await;
    let snap = client
        .call(
            "console_snapshot",
            json!({"project": PROJECT, "root": root}),
        )
        .await;
    serde_json::from_value(result(snap)).unwrap()
}

fn find<'a>(worktrees: &'a [WorktreeEntry], path: &Path) -> &'a WorktreeEntry {
    worktrees
        .iter()
        .find(|w| Path::new(&w.path) == path)
        .unwrap_or_else(|| panic!("no worktree {}: {worktrees:?}", path.display()))
}

#[tokio::test]
async fn worktrees_list_branch_changes_and_commits() {
    let dir = scratch();
    let spaced = "dir with space/b \"q\".txt";
    let root = checkout(
        dir.path(),
        PROJECT,
        &[
            ("a.txt", "one\ntwo\nthree\n"),
            (spaced, "b\n"),
            ("gone.txt", "x\n"),
        ],
    );
    for n in 0..21 {
        git(
            &root,
            &["commit", "-q", "--allow-empty", "-m", &format!("Empty {n}")],
        );
    }
    let second = dir.path().join("second");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/x",
            second.to_str().unwrap(),
        ],
    );
    let second = PathBuf::from(git(&second, &["rev-parse", "--show-toplevel"]).trim());
    git(&second, &["commit", "-q", "--allow-empty", "-m", "On feat"]);
    std::fs::write(second.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    std::fs::write(second.join("new file.txt"), "n\n").unwrap();
    std::fs::remove_file(second.join("gone.txt")).unwrap();

    let daemon = Daemon::start().await;
    let snap = snapshot(&daemon, &root).await;
    assert_eq!(snap.worktrees.len(), 2, "{:?}", snap.worktrees);

    let main = find(&snap.worktrees, &root);
    assert_eq!(
        main.head,
        HeadEntry::Branch {
            name: "main".into()
        }
    );
    assert_eq!(
        main.identity.as_ref().map(|me| me.handle.as_str()),
        Some("andy/sol")
    );
    assert_eq!(main.commits.len(), 20, "at most 20 commits");
    assert_eq!(main.commits[0].subject, "Empty 20");
    assert!(main.changes.is_empty(), "{:?}", main.changes);
    assert_eq!(main.files, ["a.txt", spaced, "gone.txt"]);

    let feat = find(&snap.worktrees, &second);
    assert_eq!(
        feat.head,
        HeadEntry::Branch {
            name: "feat/x".into()
        }
    );
    assert_eq!(feat.identity, None);
    assert_eq!(feat.commits[0].subject, "On feat");
    assert_eq!(feat.commits[0].author, "Andy A");
    assert!(
        feat.commits[0]
            .sha
            .as_str()
            .starts_with(feat.commits[0].short.as_str())
    );
    let kinds: Vec<(&str, ChangeKindEntry, u32, u32)> = feat
        .changes
        .iter()
        .map(|c| (c.path.as_str(), c.kind, c.added, c.removed))
        .collect();
    assert_eq!(
        kinds,
        [
            ("a.txt", ChangeKindEntry::Modified, 2, 1),
            ("gone.txt", ChangeKindEntry::Deleted, 0, 1),
            ("new file.txt", ChangeKindEntry::Added, 0, 0),
        ]
    );
    let hunks = &feat.changes[0].hunks;
    assert_eq!(hunks.len(), 1, "{hunks:?}");
    assert_eq!((hunks[0].old_start, hunks[0].new_start), (1, 1));

    assert!(snap.git.branches.contains(&"main".to_owned()));
    assert!(snap.git.branches.contains(&"feat/x".to_owned()));
    assert!(snap.git.files.contains(&spaced.to_owned()));
    assert!(snap.git.commits.contains(&feat.commits[0].sha.to_string()));
}

#[tokio::test]
async fn a_root_of_another_project_reads_nothing() {
    let dir = scratch();
    let root = checkout(dir.path(), "projectother", &[("a.txt", "a\n")]);
    let daemon = Daemon::start().await;
    let snap = snapshot(&daemon, &root).await;
    assert!(snap.worktrees.is_empty());
    assert!(snap.git.files.is_empty());
    assert!(snap.roles.iter().all(|r| r.layer != RoleLayer::Project));
    // Nor will it read a file there.
    let mut client = daemon.client().await;
    let err = error(
        client
            .call(
                "file_text",
                json!({"project": PROJECT, "root": root, "worktree": root, "path": "a.txt"}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::NotFound, "{err:?}");
}

async fn file_text(
    daemon: &Daemon,
    root: &Path,
    worktree: &Path,
    path: &str,
) -> Result<FileTextEntry, agmen_local::ApiError> {
    let mut client = daemon.client().await;
    let answer = client
        .call(
            "file_text",
            json!({"project": PROJECT, "root": root, "worktree": worktree, "path": path}),
        )
        .await;
    match answer.outcome {
        agmen_local::Outcome::Ok { result } => Ok(serde_json::from_value(result).unwrap()),
        agmen_local::Outcome::Err { error } => Err(error),
    }
}

#[tokio::test]
async fn file_text_refuses_symlink_escape() {
    let dir = scratch();
    let secret = dir.path().join("id_ed25519");
    std::fs::write(&secret, "PRIVATE KEY\n").unwrap();
    let root = checkout(dir.path(), PROJECT, &[("src/main.rs", "fn main() {}\n")]);
    // An agent commits notes.md pointing at a key outside the checkout.
    std::os::unix::fs::symlink(&secret, root.join("notes.md")).unwrap();
    std::os::unix::fs::symlink("src", root.join("code")).unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "Links"]);
    let daemon = Daemon::start().await;

    let err = file_text(&daemon, &root, &root, "notes.md")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BadRequest, "{err:?}");
    assert!(err.message.contains("outside"), "{}", err.message);
    assert!(!err.message.contains("PRIVATE"));
    for path in ["../id_ed25519", "/etc/hosts", ".git/config", ""] {
        let err = file_text(&daemon, &root, &root, path).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest, "{path}: {err:?}");
    }
    // A worktree agmend did not list is refused, even inside the root.
    let err = file_text(&daemon, &root, &root.join("src"), "main.rs")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound, "{err:?}");

    // A link that stays inside the root reads, with its change marks.
    std::fs::write(root.join("src/main.rs"), "fn main() {\n    run();\n}\n").unwrap();
    let file = file_text(&daemon, &root, &root, "code/main.rs")
        .await
        .unwrap();
    assert_eq!(file.text, "fn main() {\n    run();\n}\n");
    assert!(!file.truncated);
    assert_eq!(file.marks, [MarkEntry::Modified { from: 1, to: 4 }]);
}

#[tokio::test]
async fn file_text_caps_at_512k() {
    let dir = scratch();
    let line = "é".repeat(39) + "\n";
    let big = line.repeat(600 * 1024 / line.len());
    assert!(big.len() > 512 * 1024);
    let root = checkout(dir.path(), PROJECT, &[("big.txt", &big)]);
    let daemon = Daemon::start().await;
    let file = file_text(&daemon, &root, &root, "big.txt").await.unwrap();
    assert!(file.truncated);
    assert!(file.text.len() <= 512 * 1024, "{}", file.text.len());
    assert!(file.text.len() > 511 * 1024, "{}", file.text.len());
    assert!(big.starts_with(&file.text));
    // Review fix 2: the full size, so the desktop can say how much is cut.
    assert_eq!(file.size, big.len() as u64);
    // An untracked file is all new lines.
    std::fs::write(root.join("new.txt"), "a\nb\n").unwrap();
    let file = file_text(&daemon, &root, &root, "new.txt").await.unwrap();
    assert_eq!(file.marks, [MarkEntry::Added { from: 1, to: 3 }]);
}

#[tokio::test]
async fn roles_list_three_layers() {
    let dir = scratch();
    let project_role = r#"<role name="security-reviewer" extends="reviewer" kind="agent" tier="deep" version="1"/>"#;
    let root = checkout(
        dir.path(),
        PROJECT,
        &[(".agmen/roles/security-reviewer.xml", project_role)],
    );
    let daemon = Daemon::start().await;
    let personal = daemon.home.personal_roles();
    std::fs::create_dir_all(&personal).unwrap();
    std::fs::write(
        personal.join("docs-writer.xml"),
        r#"<role name="docs-writer" extends="engineer" kind="agent" tier="standard" version="1"/>"#,
    )
    .unwrap();
    std::fs::write(personal.join("broken.xml"), "<role").unwrap();
    std::fs::write(personal.join("notes.txt"), "not a role").unwrap();

    let snap = snapshot(&daemon, &root).await;
    let layers: Vec<(&str, RoleLayer)> = snap
        .roles
        .iter()
        .map(|r| (r.name.as_str(), r.layer))
        .collect();
    let built_in: Vec<&str> = agmen_local::roles::BUILT_IN
        .iter()
        .map(|(n, _)| *n)
        .collect();
    let mut want: Vec<(&str, RoleLayer)> =
        built_in.iter().map(|n| (*n, RoleLayer::BuiltIn)).collect();
    want.extend([
        ("security-reviewer", RoleLayer::Project),
        ("broken", RoleLayer::Personal),
        ("docs-writer", RoleLayer::Personal),
    ]);
    assert_eq!(layers, want);
    assert_eq!(
        snap.roles
            .iter()
            .find(|r| r.name == "security-reviewer")
            .unwrap()
            .text,
        project_role
    );

    // Without a root, the project layer is empty and the rest stay.
    let mut client = daemon.client().await;
    let bare: ConsoleSnapshot = serde_json::from_value(result(
        client
            .call("console_snapshot", json!({"project": PROJECT}))
            .await,
    ))
    .unwrap();
    assert_eq!(bare.roles.len(), want.len() - 1);
    assert!(bare.worktrees.is_empty());
}

#[tokio::test]
async fn project_roles_stay_inside_the_checkout() {
    let dir = scratch();
    let outside = dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("planted.xml"),
        r#"<role name="planted" kind="agent" tier="small"/>"#,
    )
    .unwrap();
    std::fs::write(outside.join("linked.xml"), "<role name=\"linked\"/>").unwrap();
    let root = checkout(dir.path(), PROJECT, &[("a.txt", "a\n")]);
    // A committed .agmen/roles that links out of the checkout, and a role
    // file that does.
    std::os::unix::fs::symlink(&outside, root.join(".agmen/roles")).unwrap();
    let daemon = Daemon::start().await;
    let snap = snapshot(&daemon, &root).await;
    assert!(
        snap.roles.iter().all(|r| r.layer != RoleLayer::Project),
        "{:?}",
        snap.roles.iter().map(|r| &r.name).collect::<Vec<_>>()
    );
    std::fs::remove_file(root.join(".agmen/roles")).unwrap();
    std::fs::create_dir(root.join(".agmen/roles")).unwrap();
    std::os::unix::fs::symlink(
        outside.join("linked.xml"),
        root.join(".agmen/roles/linked.xml"),
    )
    .unwrap();
    let snap = snapshot(&daemon, &root).await;
    assert!(snap.roles.iter().all(|r| r.layer != RoleLayer::Project));
}

#[tokio::test]
async fn hunks_stay_with_their_file_beside_a_submodule() {
    // Review fix 1: with diff.submodule=log the patch has no section of its
    // own for the submodule, so hunks paired by position landed on it.
    let dir = scratch();
    let sub = dir.path().join("subrepo");
    std::fs::create_dir_all(&sub).unwrap();
    git(&sub, &["init", "-q"]);
    git(&sub, &["commit", "-q", "--allow-empty", "-m", "Sub one"]);
    let root = checkout(
        dir.path(),
        PROJECT,
        &[("a.txt", "a1\na2\n"), ("z.txt", "z1\nz2\n")],
    );
    git(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.to_str().unwrap(),
            "sub",
        ],
    );
    git(&root, &["commit", "-q", "-m", "Add sub"]);
    git(&root, &["config", "diff.submodule", "log"]);
    git(
        &root.join("sub"),
        &["commit", "-q", "--allow-empty", "-m", "Sub two"],
    );
    std::fs::write(root.join("a.txt"), "a1\nA2\n").unwrap();
    std::fs::write(root.join("z.txt"), "z1\nZ2\nz3\n").unwrap();

    let daemon = Daemon::start().await;
    let snap = snapshot(&daemon, &root).await;
    let main = find(&snap.worktrees, &root);
    let text_of = |path: &str| -> Vec<String> {
        main.changes
            .iter()
            .find(|c| c.path == path)
            .unwrap_or_else(|| panic!("no change {path}: {:?}", main.changes))
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter().map(|l| l.text.clone()))
            .collect()
    };
    assert!(
        text_of("z.txt").contains(&"Z2".to_owned()),
        "{:?}",
        main.changes
    );
    assert!(text_of("a.txt").contains(&"A2".to_owned()));
    let sub_lines = text_of("sub");
    assert!(
        !sub_lines
            .iter()
            .any(|l| l.starts_with('z') || l.starts_with('Z')),
        "z.txt's hunks landed on the submodule: {sub_lines:?}"
    );
    let z = main.changes.iter().find(|c| c.path == "z.txt").unwrap();
    assert_eq!((z.added, z.removed), (2, 1));
}
