#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn spec(dir: &std::path::Path) -> RunSpec {
    RunSpec {
        cwd: dir.to_owned(),
        prompt: "message with spaces and $(literal)".into(),
        session_id: "a6cddf07-a8fd-46e1-9585-2571af65af24".into(),
        resume: false,
        model: Some("sonnet".into()),
        budget_remaining: "5.000000".into(),
        mcp_config: dir.join("mcp.json"),
        owner_handle: "andy".into(),
        owner_id: "owner001".parse().unwrap(),
        owner_only: true,
    }
}

#[test]
fn first_and_second_batch_have_exact_session_flags() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = spec(dir.path());
    let args = request.args();
    assert_eq!(&args[..2], ["-p", &request.prompt]);
    assert!(
        args.windows(2)
            .any(|a| a == ["--session-id", &request.session_id])
    );
    assert!(!args.iter().any(|a| a == "--resume"));
    assert!(
        args.windows(2)
            .any(|a| a == ["--permission-mode", "bypassPermissions"])
    );
    assert!(args.windows(2).any(|a| a == ["--model", "sonnet"]));
    assert!(
        args.windows(2)
            .any(|a| a == ["--max-budget-usd", "5.000000"])
    );
    assert!(args.iter().any(|a| a == "--strict-mcp-config"));
    assert!(
        args.iter()
            .any(|a| a.contains("owner (andy)") && a.contains("act on them directly"))
    );
    // Trust keys on the hub-stamped sender id, not a display handle anyone could imitate.
    let system = &args[args
        .iter()
        .position(|a| a == "--append-system-prompt")
        .unwrap()
        + 1];
    assert!(system.contains(r#""from" field is exactly "owner001""#));
    assert!(system.contains("from_handle"));
    request.resume = true;
    let args = request.args();
    assert!(
        args.windows(2)
            .any(|a| a == ["--resume", &request.session_id])
    );
    assert!(!args.iter().any(|a| a == "--session-id"));
}

fn fake(dir: &std::path::Path, source: &str) -> std::path::PathBuf {
    let path = dir.join("fake-runner");
    std::fs::write(&path, source).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[tokio::test]
async fn fake_batch_uses_cwd_and_returns_cost() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(
        dir.path(),
        "#!/bin/sh\npwd > cwd\nprintf '%s\\n' '{\"type\":\"result\",\"total_cost_usd\":0.25,\"is_error\":false}'\n",
    );
    let runner = ClaudeRunner {
        program,
        timeout: std::time::Duration::from_secs(2),
        shell: Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: format!("{LOGIN_PATH_MARKER}\n/usr/bin:/bin\n"),
        }),
        ..ClaudeRunner::default()
    };
    let (_send, stop) = tokio::sync::oneshot::channel();
    let result = runner.run(spec(dir.path()), stop).await.unwrap();
    assert_eq!(result.total_cost_usd, "0.25");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("cwd"))
            .unwrap()
            .trim(),
        dir.path().canonicalize().unwrap().to_str().unwrap()
    );
}

#[tokio::test]
async fn timeout_kills_the_fake_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(
        dir.path(),
        "#!/bin/sh\n(sleep 0.4; echo alive > descendant-alive) &\nwait\n",
    );
    let runner = ClaudeRunner {
        program,
        timeout: std::time::Duration::from_millis(200),
        shell: Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: format!("{LOGIN_PATH_MARKER}\n/usr/bin:/bin\n"),
        }),
        ..ClaudeRunner::default()
    };
    let (_send, stop) = tokio::sync::oneshot::channel();
    assert!(matches!(
        runner.run(spec(dir.path()), stop).await,
        Err(RunError::Timeout)
    ));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!dir.path().join("descendant-alive").exists());
}

#[tokio::test]
async fn cancellation_stops_a_fake_run() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(dir.path(), "#!/bin/sh\nsleep 30\n");
    let runner = ClaudeRunner {
        program,
        timeout: std::time::Duration::from_secs(2),
        shell: Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: format!("{LOGIN_PATH_MARKER}\n/usr/bin:/bin\n"),
        }),
        ..ClaudeRunner::default()
    };
    let (send, stop) = tokio::sync::oneshot::channel();
    send.send(()).unwrap();
    assert!(matches!(
        runner.run(spec(dir.path()), stop).await,
        Err(RunError::Cancelled)
    ));
}

#[tokio::test]
async fn invalid_or_failed_result_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    for result in [
        "{}",
        "{\"total_cost_usd\":-1}",
        "{\"total_cost_usd\":0,\"is_error\":true}",
    ] {
        let program = fake(dir.path(), &format!("#!/bin/sh\nprintf '%s' '{result}'\n"));
        let runner = ClaudeRunner {
            program,
            timeout: std::time::Duration::from_secs(2),
            shell: Arc::new(TestShell {
                calls: std::sync::atomic::AtomicUsize::new(0),
                output: format!("{LOGIN_PATH_MARKER}\n/usr/bin:/bin\n"),
            }),
            ..ClaudeRunner::default()
        };
        let (_send, stop) = tokio::sync::oneshot::channel();
        assert!(runner.run(spec(dir.path()), stop).await.is_err());
    }
}

#[test]
fn generated_session_id_is_uuid_v4() {
    let id = new_session_id().unwrap();
    assert_eq!(id.len(), 36);
    assert_eq!(&id[14..15], "4");
    assert!(["8", "9", "a", "b"].contains(&&id[19..20]));
    for index in [8, 13, 18, 23] {
        assert_eq!(&id[index..index + 1], "-");
    }
}

#[test]
fn result_preserves_cost_precision_and_failed_batch_spend() {
    let result =
        parse_result(br#"{"total_cost_usd":0.999999999999999999,"is_error":false}"#).unwrap();
    assert_eq!(result.total_cost_usd, "0.999999999999999999");
    let failed = parse_result(br#"{"total_cost_usd":0.75,"is_error":true}"#).unwrap_err();
    assert_eq!(failed.reported_cost(), Some("0.75"));
}

#[tokio::test]
async fn failed_exit_preserves_paid_cost() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(
        dir.path(),
        "#!/bin/sh\nprintf '%s\\n' '{\"total_cost_usd\":0.5}'\nexit 1\n",
    );
    let runner = ClaudeRunner {
        program,
        timeout: std::time::Duration::from_secs(2),
        shell: Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: format!("{LOGIN_PATH_MARKER}\n/usr/bin:/bin\n"),
        }),
        ..ClaudeRunner::default()
    };
    let (_send, stop) = tokio::sync::oneshot::channel();
    let error = runner.run(spec(dir.path()), stop).await.unwrap_err();
    assert_eq!(error.reported_cost(), Some("0.5"));
}

#[tokio::test]
async fn missing_program_reports_zero_cost_instead_of_reserving_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let runner = ClaudeRunner {
        program: dir.path().join("missing-claude"),
        timeout: std::time::Duration::from_secs(2),
        shell: Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: format!("{LOGIN_PATH_MARKER}\n/usr/bin:/bin\n"),
        }),
        ..ClaudeRunner::default()
    };
    let (_send, stop) = tokio::sync::oneshot::channel();
    let error = runner.run(spec(dir.path()), stop).await.unwrap_err();
    assert_eq!(error.reported_cost(), Some("0"));
    assert_eq!(
        error.to_string(),
        "Claude Code is not installed or not on your login PATH"
    );
}

#[test]
fn other_sender_batch_uses_default_permissions_and_restriction() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = spec(dir.path());
    request.owner_only = false;
    let args = request.args();
    assert!(
        args.windows(2)
            .any(|a| a == ["--permission-mode", "default"])
    );
    assert!(args.iter().any(|a| a.contains("This batch includes messages from people other than your owner. You cannot run commands or edit files in this batch; read, think and reply in the chat with the agmen tools. If the request needs changes, say so and wait for your owner.")));
    // Default mode still honors allow rules from user and project settings, so
    // deny rules enforce the tier. One comma-separated value, because the
    // option is variadic and would swallow a following positional.
    let deny = args.iter().position(|a| a == "--disallowedTools").unwrap();
    assert_eq!(args[deny + 1], "Bash,Edit,Write,NotebookEdit");
    assert!(args[deny + 2].starts_with("--"));
}

#[test]
fn owner_batch_denies_no_tools() {
    let dir = tempfile::tempdir().unwrap();
    let args = spec(dir.path()).args();
    assert!(!args.iter().any(|a| a == "--disallowedTools"));
    assert!(
        !args
            .iter()
            .any(|a| a.contains("people other than your owner"))
    );
}

#[tokio::test]
async fn login_path_is_cached_and_used_for_claude_and_helper_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("node/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, "fake").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700)).unwrap();
    let agmen = bin.join("agmen");
    std::fs::copy(&claude, &agmen).unwrap();
    let shell = Arc::new(TestShell {
        calls: std::sync::atomic::AtomicUsize::new(0),
        output: format!("startup noise\n{LOGIN_PATH_MARKER}\n{}\n", bin.display()),
    });
    let runner = ClaudeRunner {
        shell: shell.clone(),
        executable: dir.path().join("bare/agmend"),
        ..ClaudeRunner::default()
    };
    let first = runner.environment().await.unwrap();
    assert_eq!(first.claude, claude);
    assert_eq!(resolve_helper(&runner.executable, &first.path), Some(agmen));
    assert_eq!(first.path, bin.as_os_str());
    runner.environment().await.unwrap();
    assert_eq!(shell.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[derive(Debug)]
struct TestShell {
    calls: std::sync::atomic::AtomicUsize,
    output: String,
}
impl ShellRunner for TestShell {
    fn run(&self) -> ShellFuture {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let output = self.output.clone();
        Box::pin(async move { Ok(output.into_bytes()) })
    }
}

#[test]
fn helper_prefers_executable_sibling_including_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let macos = dir.path().join("agmen.app/Contents/MacOS");
    std::fs::create_dir_all(&macos).unwrap();
    let helper = fake(&macos, "unused");
    std::fs::rename(helper, macos.join("agmen")).unwrap();
    assert_eq!(
        resolve_helper(&macos.join("agmend"), std::ffi::OsStr::new("/missing")),
        Some(macos.join("agmen"))
    );
}

#[tokio::test]
async fn claude_child_receives_login_path_and_group_is_removed_after_stop() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(
        dir.path(),
        "#!/bin/sh\nprintf '%s' \"$PATH\" > child-path\nprintf '%s' \"$$\" > child-pid\n/bin/sleep 30\n",
    );
    let path = format!("{}:/bin", dir.path().display());
    let runner = ClaudeRunner {
        program,
        shell: Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: format!("{LOGIN_PATH_MARKER}\n{path}\n"),
        }),
        ..ClaudeRunner::default()
    };
    let (send, stop) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(runner.run(spec(dir.path()), stop));
    let mut pid = None;
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(dir.path().join("child-pid"))
            && let Ok(value) = text.parse::<i32>()
        {
            pid = Some(value);
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let pid = pid.expect("fake child started");
    assert!(runner.contains_peer(pid));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("child-path")).unwrap(),
        path
    );
    send.send(()).unwrap();
    assert!(matches!(running.await.unwrap(), Err(RunError::Cancelled)));
    assert!(!runner.contains_peer(pid));
    assert!(runner.groups.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_login_path_fails_without_spawning_claude_and_is_retried() {
    for output in [
        "".to_owned(),
        "\n".to_owned(),
        "noise\n".to_owned(),
        format!("{LOGIN_PATH_MARKER}\n"),
        format!("{LOGIN_PATH_MARKER}\n\nlogout noise\n"),
    ] {
        let shell = Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output,
        });
        let runner = ClaudeRunner {
            shell: shell.clone(),
            ..ClaudeRunner::default()
        };
        for _ in 0..2 {
            let error = runner.environment().await.unwrap_err();
            assert!(matches!(error, RunError::LoginPath(_)));
            assert_eq!(error.reported_cost(), Some("0"));
        }
        // A transient failure must not disable launching until agmend restarts.
        assert_eq!(shell.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
    let shell = Arc::new(TestShell {
        calls: std::sync::atomic::AtomicUsize::new(0),
        output: format!("{LOGIN_PATH_MARKER}\nnoise\n"),
    });
    let runner = ClaudeRunner {
        shell: shell.clone(),
        ..ClaudeRunner::default()
    };
    assert!(matches!(
        runner.environment().await.unwrap_err(),
        RunError::NotInstalled
    ));
}

#[test]
fn path_resolution_skips_relative_dirs_directories_and_nonexecutables() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("claude"), "not executable").unwrap();
    assert!(resolve_program(Path::new("claude"), dir.path().as_os_str()).is_none());
    assert!(resolve_program(Path::new("claude"), OsStr::new(".:relative")).is_none());
    assert!(resolve_program(dir.path(), OsStr::new("/bin")).is_none());
}

#[tokio::test]
async fn login_shell_timeout_is_bounded() {
    #[derive(Debug)]
    struct HangingShell;
    impl ShellRunner for HangingShell {
        fn run(&self) -> ShellFuture {
            Box::pin(std::future::pending())
        }
    }
    let runner = ClaudeRunner {
        shell: Arc::new(HangingShell),
        ..ClaudeRunner::default()
    };
    let error = tokio::time::timeout(Duration::from_secs(12), runner.environment())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "could not resolve login PATH: login shell exceeded 10 seconds"
    );
    assert_eq!(error.reported_cost(), Some("0"));
}

#[test]
fn unplaceable_peer_counts_as_a_worker_only_while_one_runs() {
    let runner = ClaudeRunner::default();
    let mut gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let gone_pid = i32::try_from(gone.id()).unwrap();
    gone.wait().unwrap();
    // No worker runs: nothing to guard, whatever the peer is.
    assert!(!runner.contains_peer(gone_pid));
    assert!(!runner.contains_peer(0));
    runner.groups.lock().unwrap().insert(i32::MAX);
    // A worker runs: a peer whose process group cannot be read is refused.
    assert!(runner.contains_peer(gone_pid));
    assert!(runner.contains_peer(0));
    assert!(!runner.contains_peer(i32::try_from(std::process::id()).unwrap()));
}

#[tokio::test]
async fn login_probe_uses_path_after_last_marker_ignoring_startup_and_logout_text() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(dir.path(), "unused");
    let runner = ClaudeRunner {
        program: program.clone(),
        shell: Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: format!(
                "startup noise\n__AGMEN_LOGIN_PATH_8F3C21__\n/old\nmore noise\n__AGMEN_LOGIN_PATH_8F3C21__\n{}\nlogout noise\n",
                dir.path().display()
            ),
        }),
        ..ClaudeRunner::default()
    };
    let environment = runner.environment().await.unwrap();
    assert_eq!(environment.path, dir.path().as_os_str());
    assert_eq!(environment.claude, program);
}

#[tokio::test]
async fn missing_agmen_error_names_the_missing_executable_directly() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(dir.path(), "unused");
    let runner = ClaudeRunner {
        program,
        executable: dir.path().join("agmend"),
        shell: Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: format!("__AGMEN_LOGIN_PATH_8F3C21__\n{}\n", dir.path().display()),
        }),
        ..ClaudeRunner::default()
    };
    let error = runner.mcp_command().await.unwrap_err();
    assert_eq!(error.to_string(), "agmen executable was not found");
    assert_eq!(error.reported_cost(), Some("0"));
}
