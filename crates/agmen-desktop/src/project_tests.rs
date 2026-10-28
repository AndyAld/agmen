use std::path::Path;
use std::process::Command;

use super::*;
use crate::appearance::DesktopSettings;

const IDENTITY: &str = r#"{
    "tenant": "tenant01", "project": "projectcwd", "identity_id": "idn7k2q9",
    "handle": "andy/sol", "owner_id": "andyandy",
    "hub_url": "https://api.agmen.example",
    "broker_url": "wss://agmen.example/mqtt"
}"#;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A worktree `agmen join` set up: the identity file, ignored by git.
fn joined_worktree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::create_dir(dir.path().join(".agmen")).unwrap();
    std::fs::write(dir.path().join(".agmen/identity.json"), IDENTITY).unwrap();
    std::fs::write(dir.path().join(".git/info/exclude"), ".agmen/\n").unwrap();
    dir
}

fn settings_with(dir: &Path, project: &str) -> std::path::PathBuf {
    let path = dir.join("settings.toml");
    DesktopSettings {
        last_project: Some(project.parse().unwrap()),
    }
    .save(&path)
    .unwrap();
    path
}

#[test]
fn the_project_comes_from_the_folders_identity() {
    let wt = joined_worktree();
    let elsewhere = tempfile::tempdir().unwrap();
    let settings = settings_with(elsewhere.path(), "projectold");
    // The identity file wins over the last project.
    let (project, root) = choose_project(wt.path(), Some(&settings));
    assert_eq!(
        project,
        ProjectChoice::Project("projectcwd".parse().unwrap())
    );
    // Task 8: and the checkout's root, for agmend to read.
    let root = root.unwrap();
    assert_eq!(
        std::fs::canonicalize(root).unwrap(),
        std::fs::canonicalize(wt.path()).unwrap()
    );
}

#[test]
fn else_the_last_project_opens() {
    let outside = tempfile::tempdir().unwrap();
    let settings = settings_with(outside.path(), "projectold");
    assert_eq!(
        choose_project(outside.path(), Some(&settings)),
        (ProjectChoice::Project("projectold".parse().unwrap()), None)
    );
}

#[test]
fn neither_gives_no_project() {
    let outside = tempfile::tempdir().unwrap();
    let missing = outside.path().join("settings.toml");
    assert_eq!(
        choose_project(outside.path(), Some(&missing)).0,
        ProjectChoice::None { why: None }
    );
    assert_eq!(
        choose_project(outside.path(), None).0,
        ProjectChoice::None { why: None }
    );
}

#[test]
fn an_identity_agmen_may_not_use_says_why() {
    // A tracked identity file fails the trust test (agmen-client's
    // find_identity); the window says so instead of opening another project.
    let wt = joined_worktree();
    std::fs::write(wt.path().join(".git/info/exclude"), "").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let settings = settings_with(elsewhere.path(), "projectold");
    let (ProjectChoice::None { why: Some(why) }, None) = choose_project(wt.path(), Some(&settings))
    else {
        panic!("an untrusted identity opened a project");
    };
    assert!(why.contains("agmen does not use"), "{why}");
}
