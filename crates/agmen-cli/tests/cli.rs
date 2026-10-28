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

/// Signs `home` in at `hub` with tokens that have already expired, so the
/// next command renews them first.
fn sign_in_expired(home: &Home, hub: &str) {
    let file = home.login_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let login = serde_json::json!({"hub_url": hub, "human_id": "hmn7k2q9", "handle": "andy"});
    std::fs::write(&file, serde_json::to_vec(&login).unwrap()).unwrap();
    let tokens = r#"{"access_token":"a","refresh_token":"r","expires_at":"2020-01-01T00:00:00Z"}"#;
    let key = agmen_client::TokenKey::new(&hub.parse().unwrap(), &"hmn7k2q9".parse().unwrap());
    home.tokens().set(&key.unwrap(), tokens).unwrap();
}

/// A hub that answers every request with `200 OK` and a body that is not
/// JSON, then closes. It serves one connection.
fn nonsense_hub() -> String {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _read = conn.read(&mut buf).unwrap();
        conn.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 3\r\nconnection: close\r\n\r\nnah",
        )
        .unwrap();
    });
    format!("http://{addr}/")
}

#[test]
fn renewal_errors_keep_their_wording() {
    // The hub cannot be reached while renewing.
    let home = Home::new();
    sign_in_expired(&home, "http://127.0.0.1:9/");
    let out = home.agmen(home.path(), &["whoami"]).output().unwrap();
    assert_exit(&out, 1);
    assert!(
        stderr(&out).contains("renewing the sign-in failed: the hub could not be reached"),
        "{}",
        stderr(&out)
    );

    // The hub answers 200 with a body that is not a token response.
    let home = Home::new();
    sign_in_expired(&home, &nonsense_hub());
    let out = home.agmen(home.path(), &["whoami"]).output().unwrap();
    assert_exit(&out, 1);
    assert!(
        stderr(&out)
            .contains("renewing the sign-in failed: the hub's answer is not what agmen expects"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn local_read_errors_keep_their_wording() {
    // The token store cannot be read: its folder is a file.
    let home = Home::new();
    sign_in_expired(&home, "http://127.0.0.1:9/");
    std::fs::remove_dir_all(home.path().join("tokens")).unwrap();
    std::fs::write(home.path().join("tokens"), "not a folder").unwrap();
    let out = home.agmen(home.path(), &["whoami"]).output().unwrap();
    assert_exit(&out, 1);
    let err = stderr(&out);
    assert!(
        err.contains("agmen: reading the token store failed"),
        "{err}"
    );
    assert!(!err.contains("renewing"), "{err}");

    // The login file cannot be read: it is a folder.
    let home = Home::new();
    let file = home.login_file();
    std::fs::create_dir_all(&file).unwrap();
    let out = home.agmen(home.path(), &["whoami"]).output().unwrap();
    assert_exit(&out, 1);
    let err = stderr(&out);
    assert!(
        err.contains(&format!("agmen: reading {}", file.display())),
        "{err}"
    );
}
