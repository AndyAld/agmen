//! The `agmen` binary without a hub: its version line, usage errors, and
//! the refusals that come before any network call (not signed in, not
//! joined, an identity file git does not ignore).

#![cfg(test)]

mod support;

use support::{Home, Worktree, assert_exit, stderr, stdout};

/// A minimal identity file pointing at a hub nobody runs.
const IDENTITY: &str = r#"{"tenant":"tenant01","project":"project1","identity_id":"idn7k2q9",
"handle":"andy/sol","owner_id":"andyandy","hub_url":"http://127.0.0.1:9",
"broker_url":"mqtt://127.0.0.1:9"}"#;

#[test]
fn version_is_agmen_semver() {
    let home = Home::new();
    let out = home.agmen(home.path(), &["--version"]).output().unwrap();
    assert_exit(&out, 0);
    assert_eq!(
        stdout(&out).trim(),
        format!("agmen {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn usage_errors_exit_2() {
    let home = Home::new();
    for args in [
        &["dm", "only-a-recipient"][..],
        &["post", "NOT_A_VERB", "hi"],
        &["post", "STATUS", "--data", "{not json"],
        &["claim", "not-an-item"],
        &["invite", "--expires", "soon"],
        &["login"],
    ] {
        let out = home.agmen(home.path(), args).output().unwrap();
        assert_exit(&out, 2);
        assert!(!stderr(&out).is_empty(), "{args:?}");
    }
}

#[test]
fn not_signed_in_exits_3() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    for args in [
        &["whoami"][..],
        &["invite"],
        &["join", "--project", "project1"],
    ] {
        let out = home.agmen(&wt.path(), args).output().unwrap();
        assert_exit(&out, 3);
        assert!(stderr(&out).contains("agmen login"), "{}", stderr(&out));
    }
}

#[test]
fn not_joined_exits_3() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    for args in [
        &["post", "FYI", "hi"][..],
        &["inbox"],
        &["watch", "--json"],
        &["check-path", "--json", "--", "src/a.rs"],
        &["leave"],
    ] {
        let out = home.agmen(&wt.path(), args).output().unwrap();
        assert_exit(&out, 3);
        assert!(stderr(&out).contains("agmen join"), "{}", stderr(&out));
    }
}

#[test]
fn identity_file_without_token_exits_3() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    std::fs::create_dir(wt.path().join(".agmen")).unwrap();
    std::fs::write(wt.path().join(".agmen/identity.json"), IDENTITY).unwrap();
    std::fs::write(wt.path().join(".git/info/exclude"), ".agmen/\n").unwrap();
    let out = home
        .agmen(&wt.path(), &["check-path", "--json", "--", "a"])
        .output()
        .unwrap();
    assert_exit(&out, 3);
    assert!(stderr(&out).contains("andy/sol"), "{}", stderr(&out));
}

#[test]
fn tracked_identity_file_is_refused() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    std::fs::create_dir(wt.path().join(".agmen")).unwrap();
    std::fs::write(wt.path().join(".agmen/identity.json"), IDENTITY).unwrap();
    wt.git(&home, &["add", ".agmen/identity.json"]);
    let out = home
        .agmen(&wt.path(), &["post", "FYI", "hi"])
        .output()
        .unwrap();
    assert_exit(&out, 1);
    assert!(
        stderr(&out).contains("agmen does not use"),
        "{}",
        stderr(&out)
    );
}
