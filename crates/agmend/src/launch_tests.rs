#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::os::unix::fs::PermissionsExt as _;

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
    };
    let (_send, stop) = tokio::sync::oneshot::channel();
    let error = runner.run(spec(dir.path()), stop).await.unwrap_err();
    assert_eq!(error.reported_cost(), Some("0"));
    assert!(error.to_string().contains("could not start"));
}
