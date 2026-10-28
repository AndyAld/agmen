//! The `agmen` binary without a hub: its version line, usage errors, and
//! the refusals that come before any network call (not signed in, not
//! joined, an identity file git does not ignore).

#![cfg(test)]

mod support;

use support::{Home, Worktree, assert_exit, stderr, stdout};

#[test]
fn join_bare_code_needs_a_hub() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    let out = home
        .agmen(&wt.path(), &["join", &"ab".repeat(32)])
        .output()
        .unwrap();
    assert_exit(&out, 2);
    assert!(stderr(&out).contains("--hub"));
}

#[test]
fn join_no_setup_skips_setup() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    let out = home
        .agmen(&wt.path(), &["join", "--project", "project1", "--no-setup"])
        .output()
        .unwrap();
    assert_exit(&out, 3);
    assert!(stderr(&out).contains("not signed in"));
    let link = format!("https://hub.example/i#{}", "ab".repeat(32));
    let out = home
        .agmen(&wt.path(), &["join", &link, "--no-setup", "--help"])
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("--no-setup"));
}

/// Local usage checks run before sign-in or any hub call: nothing listens on
/// port 9, so a join that got as far as the device login would exit 1 with
/// "starting the device login failed".
#[test]
fn join_checks_the_worktree_locally_before_signing_in() {
    let code = "ab".repeat(32);
    let link = format!("http://127.0.0.1:9/i#{code}");
    let home = Home::new();
    let wt = Worktree::new(&home);
    for args in [
        vec!["join", &link, "--role", "listener"],
        vec!["join", &link, "--role", "no-such-role"],
        vec![
            "join",
            &code,
            "--hub",
            "http://127.0.0.1:9",
            "--role",
            "listener",
        ],
    ] {
        let out = home.agmen(&wt.path(), &args).output().unwrap();
        assert_exit(&out, 2);
        let text = stderr(&out);
        assert!(
            text.contains("listeners are started by agmend") || text.contains("unknown role"),
            "{args:?}: {text}"
        );
        assert!(!text.contains("device login"), "{args:?}: {text}");
    }
    // An invite is single use: a worktree that already joined must refuse
    // before the invite is accepted.
    let joined = Worktree::new(&home);
    std::fs::create_dir(joined.path().join(".agmen")).unwrap();
    std::fs::write(joined.path().join(".agmen/identity.json"), IDENTITY).unwrap();
    let out = home
        .agmen(&joined.path(), &["join", &link])
        .output()
        .unwrap();
    assert_exit(&out, 1);
    let text = stderr(&out);
    assert!(text.contains("already joined"), "{text}");
    assert!(!text.contains("device login"), "{text}");
}

/// A minimal identity file pointing at a hub nobody runs.
const IDENTITY: &str = r#"{"tenant":"tenant01","project":"project1","identity_id":"idn7k2q9",
"handle":"andy/sol","owner_id":"andyandy","hub_url":"http://127.0.0.1:9",
"broker_url":"mqtt://127.0.0.1:9"}"#;

#[cfg(unix)]
fn setup_worktree(home: &Home) -> Worktree {
    let wt = Worktree::new(home);
    std::fs::create_dir(wt.path().join(".agmen")).unwrap();
    std::fs::write(wt.path().join(".agmen/identity.json"), IDENTITY).unwrap();
    std::fs::write(wt.path().join(".git/info/exclude"), ".agmen/\n").unwrap();
    wt
}

#[cfg(unix)]
#[test]
fn setup_needs_a_joined_worktree() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    let out = home.agmen(&wt.path(), &["setup"]).output().unwrap();
    assert_exit(&out, 3);
    assert!(stderr(&out).contains("not joined: run agmen join"));
}

#[cfg(unix)]
#[test]
fn setup_refuses_an_agent_it_cannot_set_up_yet() {
    let home = Home::new();
    let out = home
        .agmen(home.path(), &["setup", "--agent", "codex"])
        .output()
        .unwrap();
    assert_exit(&out, 2);
    assert!(
        stderr(&out).contains("possible values: claude"),
        "{}",
        stderr(&out)
    );
}

#[cfg(unix)]
#[test]
fn fake_programs_can_run_system_tools() {
    let home = Home::new();
    home.fake_program(
        "probe",
        "cat \"$1\" > \"$HOME/out\"; mv \"$HOME/out\" \"$HOME/moved\"",
    );
    let file = home.path().join("input");
    std::fs::write(&file, "hello\n").unwrap();
    let mut cmd = std::process::Command::new("probe");
    cmd.arg(&file);
    home.apply_env(&mut cmd);
    assert_exit(&cmd.output().unwrap(), 0);
    assert_eq!(
        std::fs::read_to_string(home.path().join("moved")).unwrap(),
        "hello\n"
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("calls/probe.log"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn setup_with_no_agents_says_so() {
    let home = Home::new();
    let wt = setup_worktree(&home);
    let out = home.agmen(&wt.path(), &["setup"]).output().unwrap();
    assert_exit(&out, 0);
    assert_eq!(
        stdout(&out),
        "no agents found: install one, then run agmen setup\n"
    );
}

#[cfg(unix)]
#[test]
fn setup_installs_the_claude_plugin() {
    let home = Home::new();
    home.fake_program("claude", "if [ \"$2\" = list ]; then printf '[]\\n'; fi");
    let wt = setup_worktree(&home);
    let out = home
        .agmen(&wt.path(), &["setup", "--agent", "claude"])
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert_eq!(stdout(&out), "Claude Code: installed the agmen plugin\n");
    assert_eq!(
        std::fs::read_to_string(home.path().join("calls/claude.log")).unwrap(),
        "plugin list --json\nplugin install agmen --marketplace agmen-dev/agmen --scope user --yes\n"
    );
    assert!(!wt.path().join(".mcp.json").exists());
}

#[cfg(unix)]
#[test]
fn setup_skips_an_installed_plugin() {
    let home = Home::new();
    home.fake_program("claude", r#"printf '%s\n' '[{"id":"agmen@agmen","version":"1","scope":"user","enabled":true,"installPath":"test"}]'"#);
    let wt = setup_worktree(&home);
    let out = home.agmen(&wt.path(), &["setup"]).output().unwrap();
    assert_exit(&out, 0);
    assert_eq!(stdout(&out), "Claude Code: already set up\n");
    assert_eq!(
        std::fs::read_to_string(home.path().join("calls/claude.log")).unwrap(),
        "plugin list --json\n"
    );
}

#[cfg(unix)]
#[test]
fn setup_no_plugin_writes_mcp_json() {
    let home = Home::new();
    std::fs::create_dir(home.path().join(".claude")).unwrap();
    let wt = setup_worktree(&home);
    let out = home
        .agmen(&wt.path(), &["setup", "--no-plugin"])
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("wrote .mcp.json"));
    let text = std::fs::read_to_string(wt.path().join(".mcp.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        serde_json::json!({"mcpServers":{"agmen":{"command":"agmen","args":["mcp"]}}})
    );
    let exclude = std::fs::read_to_string(wt.path().join(".git/info/exclude")).unwrap();
    assert!(exclude.lines().any(|line| line == ".mcp.json"));
    let out = home
        .agmen(&wt.path(), &["setup", "--no-plugin"])
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert_eq!(stdout(&out), "Claude Code: already set up\n");
    assert_eq!(
        std::fs::read_to_string(wt.path().join(".mcp.json")).unwrap(),
        text
    );
    assert_eq!(
        std::fs::read_to_string(wt.path().join(".git/info/exclude")).unwrap(),
        exclude
    );
}

#[cfg(unix)]
#[test]
fn setup_falls_back_when_plugin_install_fails() {
    let home = Home::new();
    home.fake_program("claude", "if [ \"$2\" = list ]; then printf '[]\\n'; else printf 'offline\\nother error\\n' >&2; exit 1; fi");
    let wt = setup_worktree(&home);
    let out = home.agmen(&wt.path(), &["setup"]).output().unwrap();
    assert_exit(&out, 0);
    assert_eq!(
        stdout(&out),
        "Claude Code: plugin install failed (offline); wrote .mcp.json instead\n"
    );
    assert!(wt.path().join(".mcp.json").exists());
}

#[cfg(unix)]
#[test]
fn setup_leaves_a_tracked_mcp_json_alone() {
    let home = Home::new();
    std::fs::create_dir(home.path().join(".claude")).unwrap();
    let wt = setup_worktree(&home);
    let original = "{\"mcpServers\":{\"other\":{\"command\":\"other\"}}}\n";
    std::fs::write(wt.path().join(".mcp.json"), original).unwrap();
    wt.git(&home, &["add", ".mcp.json"]);
    wt.git(&home, &["commit", "-q", "-m", "config"]);
    let out = home
        .agmen(&wt.path(), &["setup", "--no-plugin"])
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert!(
        stdout(&out).contains(
            ".mcp.json is tracked by git; add the agmen entry by hand (docs/mcp-setup.md)"
        )
    );
    assert_eq!(
        std::fs::read_to_string(wt.path().join(".mcp.json")).unwrap(),
        original
    );
}

#[cfg(unix)]
#[test]
fn setup_preserves_unreadable_and_oversized_configs() {
    for original in [
        "// comment\n{}".to_owned(),
        "[]".to_owned(),
        " ".repeat(1024 * 1024 + 1),
    ] {
        let home = Home::new();
        std::fs::create_dir(home.path().join(".claude")).unwrap();
        let wt = setup_worktree(&home);
        std::fs::write(wt.path().join(".mcp.json"), &original).unwrap();
        let out = home
            .agmen(&wt.path(), &["setup", "--no-plugin"])
            .output()
            .unwrap();
        assert_exit(&out, 0);
        assert!(stdout(&out).contains(".mcp.json"));
        assert!(!stdout(&out).contains("wrote"));
        assert_eq!(
            std::fs::read_to_string(wt.path().join(".mcp.json")).unwrap(),
            original
        );
        assert_eq!(
            std::fs::read_to_string(wt.path().join(".git/info/exclude")).unwrap(),
            ".agmen/\n"
        );
    }
}

#[cfg(unix)]
#[test]
fn setup_fallback_does_not_claim_a_tracked_file_was_written() {
    let home = Home::new();
    home.fake_program("claude", "exit 1");
    let wt = setup_worktree(&home);
    std::fs::write(wt.path().join(".mcp.json"), "{}").unwrap();
    wt.git(&home, &["add", ".mcp.json"]);
    let out = home.agmen(&wt.path(), &["setup"]).output().unwrap();
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("is tracked by git"));
    assert!(!stdout(&out).contains("wrote"));
    assert_eq!(
        std::fs::read_to_string(wt.path().join(".mcp.json")).unwrap(),
        "{}"
    );
}

#[cfg(unix)]
#[test]
fn setup_caps_plugin_stderr_at_200_characters() {
    let home = Home::new();
    home.fake_program("claude", "if [ \"$2\" = list ]; then printf '[]\\n'; else printf '%0300d\\nsecond line\\n' 0 >&2; exit 1; fi");
    let wt = setup_worktree(&home);
    let out = home.agmen(&wt.path(), &["setup"]).output().unwrap();
    assert_exit(&out, 0);
    assert_eq!(
        stdout(&out),
        format!(
            "Claude Code: plugin install failed ({}); wrote .mcp.json instead\n",
            "0".repeat(200)
        )
    );
}

#[test]
fn install_receipt_command_uses_shared_reader() {
    let home = Home::new();
    let out = home
        .agmen(home.path(), &["install-receipt", "--json"])
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert_eq!(stdout(&out).trim(), "null");
    let dir = home.state_base();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("install-receipt.json");
    let receipt =
        serde_json::json!({"channel":"shell","version":"1","owned_paths":["/active/agmen"]});
    std::fs::write(&path, receipt.to_string()).unwrap();
    let out = home
        .agmen(home.path(), &["install-receipt", "--json"])
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout(&out)).unwrap(),
        receipt
    );
    std::fs::write(&path, "broken").unwrap();
    assert!(
        !home
            .agmen(home.path(), &["install-receipt", "--json"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn hub_url_policy_cli_and_env() {
    let home = Home::new();
    for hub in [
        "http://user:password@hub.example/private?secret=yes",
        "ftp://localhost",
    ] {
        for from_env in [false, true] {
            let mut cmd = home.agmen(home.path(), &["login"]);
            if from_env {
                cmd.env("AGMEN_HUB", hub);
            } else {
                cmd.args(["--hub", hub]);
            }
            let out = cmd.output().unwrap();
            assert!(!out.status.success());
            let text = stderr(&out);
            assert!(text.contains("use https"), "{text}");
            for hidden in ["password", "private", "secret=yes"] {
                assert!(!text.contains(hidden), "{text}");
            }
        }
    }
}

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

/// A bounded local HTTP fixture, using only a temporary login and token store.
fn account_fixture(
    home: &Home,
    body: serde_json::Value,
) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let hub = format!("http://{}/", listener.local_addr().unwrap());
    let file = home.login_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        serde_json::json!({"hub_url":hub,"human_id":"hmn7k2q9","handle":"andy"}).to_string(),
    )
    .unwrap();
    let key =
        agmen_client::TokenKey::new(&hub.parse().unwrap(), &"hmn7k2q9".parse().unwrap()).unwrap();
    home.tokens()
        .set(
            &key,
            r#"{"access_token":"test","refresh_token":"test","expires_at":"2099-01-01T00:00:00Z"}"#,
        )
        .unwrap();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match listener.accept() {
                Ok((mut conn, _)) => {
                    // On macOS an accepted socket inherits the listener's
                    // nonblocking mode, so a read before the client writes
                    // would fail with WouldBlock instead of waiting.
                    conn.set_nonblocking(false).unwrap();
                    conn.set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut request = Vec::new();
                    let mut buf = [0; 4096];
                    for _ in 0..32 {
                        let n = conn.read(&mut buf).unwrap();
                        if n == 0 {
                            break;
                        }
                        request.extend_from_slice(&buf[..n]);
                        if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                            let headers =
                                String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                            let len = headers
                                .lines()
                                .find_map(|line| line.strip_prefix("content-length: "))
                                .map_or(0, |s| s.parse::<usize>().unwrap());
                            if request.len() >= end + 4 + len {
                                break;
                            }
                        }
                    }
                    let body = body.to_string();
                    write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                    return String::from_utf8(request).unwrap();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                Err(e) => panic!("{e}"),
            }
        }
        String::new()
    });
    (hub, server)
}

#[test]
fn whoami_json_and_unchanged_text() {
    let me = serde_json::json!({"id":"hmn7k2q9","handle":"andy","broker_url":"mqtt://localhost","projects":[{"tenant":"tenant01","project":"project1","name":"marked","role":"admin"}]});
    for json in [true, false] {
        let home = Home::new();
        let (hub, server) = account_fixture(&home, me.clone());
        let args = if json {
            vec!["whoami", "--json"]
        } else {
            vec!["whoami"]
        };
        let out = home.agmen(home.path(), &args).output().unwrap();
        assert_exit(&out, 0);
        assert!(server.join().unwrap().starts_with("GET /v1/me "));
        if json {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&stdout(&out)).unwrap(),
                me
            );
        } else {
            assert_eq!(
                stdout(&out),
                format!(
                    "andy (hmn7k2q9) at {hub}\n  project project1 (marked, tenant tenant01): admin\n"
                )
            );
        }
    }
}

#[test]
fn join_without_arguments_explains_missing_or_multiple_projects() {
    for projects in [
        serde_json::json!([]),
        serde_json::json!([
            {"tenant":"tenant01","project":"project1","name":"first","role":"member"},
            {"tenant":"tenant01","project":"project2","name":"second","role":"member"}
        ]),
    ] {
        let home = Home::new();
        let wt = Worktree::new(&home);
        let (_, server) = account_fixture(
            &home,
            serde_json::json!({"id":"hmn7k2q9","handle":"andy","broker_url":"mqtt://localhost","projects":projects}),
        );
        let out = home.agmen(&wt.path(), &["join"]).output().unwrap();
        assert_exit(&out, 2);
        assert!(server.join().unwrap().starts_with("GET /v1/me "));
        let text = stderr(&out);
        if projects.as_array().unwrap().is_empty() {
            assert!(
                text.contains("no projects yet: run agmen join <invite link>"),
                "{text}"
            );
        } else {
            for expected in ["project1", "first", "project2", "second", "--project"] {
                assert!(text.contains(expected), "{text}");
            }
        }
    }
}

#[test]
fn project_create_posts_name_and_optional_tenant() {
    for tenant in [None, Some("tenant01")] {
        let home = Home::new();
        let (_, server) = account_fixture(
            &home,
            serde_json::json!({"tenant":"tenant01","project":"project1","name":"marked","role":"admin"}),
        );
        let mut args = vec!["project", "create", "marked"];
        if let Some(tenant) = tenant {
            args.extend(["--tenant", tenant]);
        }
        let out = home.agmen(home.path(), &args).output().unwrap();
        assert_exit(&out, 0);
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /v1/projects "), "{request}");
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["name"], "marked");
        assert_eq!(body.get("tenant").and_then(|v| v.as_str()), tenant);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stdout(&out)).unwrap()["project"],
            "project1"
        );
    }
}

#[test]
fn project_create_accepts_bootstrap_name_starting_with_hyphen() {
    let home = Home::new();
    let (_, server) = account_fixture(
        &home,
        serde_json::json!({"tenant":"tenant01","project":"project1","name":"--help","role":"admin"}),
    );
    let out = home
        .agmen(home.path(), &["project", "create", "--", "--help"])
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout(&out)).unwrap()["name"],
        "--help"
    );
    assert!(server.join().unwrap().contains("\"name\":\"--help\""));
}

#[cfg(unix)]
#[test]
fn setup_never_runs_a_program_from_a_relative_path_entry() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = Home::new();
    let wt = setup_worktree(&home);
    // A repository that ships its own `claude` must not get it run.
    let planted = wt.path().join("claude");
    std::fs::write(&planted, "#!/bin/sh\necho ran >> \"$HOME/planted.log\"\n").unwrap();
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o700)).unwrap();
    for entry in [".", "", "bin"] {
        let mut cmd = home.agmen(&wt.path(), &["setup"]);
        cmd.env(
            "PATH",
            format!(
                "{}:{entry}:/usr/bin:/bin",
                home.path().join("bin").display()
            ),
        );
        let out = cmd.output().unwrap();
        assert_exit(&out, 0);
        assert_eq!(
            stdout(&out),
            "no agents found: install one, then run agmen setup\n",
            "PATH entry {entry:?}"
        );
        assert!(!home.path().join("planted.log").exists());
    }
}

#[cfg(unix)]
#[test]
fn setup_replaces_mcp_json_without_writing_through_a_hard_link() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = Home::new();
    std::fs::create_dir(home.path().join(".claude")).unwrap();
    let wt = setup_worktree(&home);
    let other = home.path().join("other.json");
    let original = "{\"mcpServers\":{\"x\":{\"command\":\"x\"}}}\n";
    std::fs::write(&other, original).unwrap();
    std::fs::hard_link(&other, wt.path().join(".mcp.json")).unwrap();
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600)).unwrap();
    let out = home
        .agmen(&wt.path(), &["setup", "--no-plugin"])
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert!(stdout(&out).contains("wrote .mcp.json"), "{}", stdout(&out));
    assert_eq!(std::fs::read_to_string(&other).unwrap(), original);
    let text = std::fs::read_to_string(wt.path().join(".mcp.json")).unwrap();
    assert!(text.contains("\"agmen\"") && text.contains("\"x\""));
    let mode = std::fs::metadata(wt.path().join(".mcp.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let names: Vec<_> = std::fs::read_dir(wt.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        names
            .iter()
            .all(|name| !name.to_string_lossy().contains("tmp")),
        "{names:?}"
    );
}

#[cfg(unix)]
#[test]
fn setup_leaves_a_symlinked_mcp_json_alone() {
    let home = Home::new();
    std::fs::create_dir(home.path().join(".claude")).unwrap();
    let wt = setup_worktree(&home);
    let target = home.path().join("outside.json");
    std::fs::write(&target, "{}").unwrap();
    std::os::unix::fs::symlink(&target, wt.path().join(".mcp.json")).unwrap();
    let out = home
        .agmen(&wt.path(), &["setup", "--no-plugin"])
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert!(!stdout(&out).contains("wrote"), "{}", stdout(&out));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "{}");
}
