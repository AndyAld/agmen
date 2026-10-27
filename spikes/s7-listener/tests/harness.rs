use std::path::PathBuf;
use std::time::Duration;

#[cfg(unix)]
use s7_listener::harness::{Harness, read_calls};
use s7_listener::harness::{ListenerOptions, RuntimeMode};
use s7_listener::{DelegateArgs, Tier};
use serde_json::json;

fn options(dir: &std::path::Path) -> ListenerOptions {
    ListenerOptions {
        program: PathBuf::from("/bin/sh"),
        model: "offline".into(),
        mcp_config: dir.join("mcp.json"),
        prompt: dir.join("listener.md"),
        log: dir.join("calls.jsonl"),
        mode: RuntimeMode::Stream,
        timeout: Duration::from_secs(2),
    }
}

#[test]
fn listener_argv_locks_tools_and_uses_prompt_file() {
    let dir = tempfile::tempdir().unwrap();
    let opts = options(dir.path());
    let args = opts.args();
    for pair in [
        ["--tools", "Read,Grep,Glob"],
        ["--max-budget-usd", "2.00"],
        ["--input-format", "stream-json"],
        ["--output-format", "stream-json"],
    ] {
        assert!(args.windows(2).any(|w| w == pair));
    }
    assert!(args.contains(&"--strict-mcp-config".into()));
    assert!(args.contains(&"--append-system-prompt-file".into()));
    assert!(!args.contains(&"--append-system-prompt".into()));
    for (resume, flag) in [(false, "--session-id"), (true, "--resume")] {
        let mut opts = options(dir.path());
        opts.mode = RuntimeMode::Batch {
            session_id: "session-1".into(),
            resume,
        };
        let args = opts.args();
        assert!(args.windows(2).any(|w| w == [flag, "session-1"]));
        assert!(!args.contains(&"--input-format".into()));
    }
}

#[test]
fn delegation_requires_role_and_accepts_optional_agent() {
    let args = json!({"reply_to":"request1", "brief":"fix it", "role":"engineer",
        "agent":"claude", "tier":"standard", "budget_usd":1.5});
    let parsed: DelegateArgs = serde_json::from_value(args.clone()).unwrap();
    assert_eq!(parsed.role, "engineer");
    assert_eq!(parsed.agent.as_deref(), Some("claude"));
    assert_eq!(parsed.tier, Tier::Standard);
    assert_eq!(
        serde_json::to_value(&parsed).unwrap()["reply_to"],
        "request1"
    );
    let mut absent = args.clone();
    absent.as_object_mut().unwrap().remove("role");
    assert!(serde_json::from_value::<DelegateArgs>(absent).is_err());
    let mut absent = args;
    absent.as_object_mut().unwrap().remove("agent");
    assert!(
        serde_json::from_value::<DelegateArgs>(absent)
            .unwrap()
            .agent
            .is_none()
    );
}

// /bin/sh is an offline fixture, never a model executable.
#[cfg(unix)]
#[expect(clippy::unwrap_used, reason = "the offline test fixture must start")]
async fn spawn_fixture(dir: &std::path::Path, model: &str) -> Harness {
    let mut opts = options(dir);
    opts.model = model.into();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/listener.sh");
    Harness::spawn_command(opts, |cmd| {
        cmd.arg(script);
    })
    .await
    .unwrap()
}

#[tokio::test]
#[cfg(unix)]
async fn stream_batches_parse_turns_tools_cost_and_log() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = spawn_fixture(dir.path(), "offline").await;
    for n in 1..=3 {
        harness
            .send_batch(&json!([{"seq":n,"body":"offline batch"}]))
            .await
            .unwrap();
        let turn = harness.next_turn().await.unwrap();
        assert_eq!(turn.result["result"], "offline batch");
        assert_eq!(turn.total_cost_usd, Some(0.001));
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0]["name"], "mcp__agmen__answer");
        assert!(turn.latency < Duration::from_secs(2));
    }
    assert!(harness.close().await.unwrap().success());
    assert!(
        read_calls(&dir.path().join("calls.jsonl"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[cfg(unix)]
async fn batch_mode_sends_plain_prompt_and_closes_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path());
    opts.mode = RuntimeMode::Batch {
        session_id: "session-1".into(),
        resume: false,
    };
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/listener.sh");
    let mut harness = Harness::spawn_command(opts, |cmd| {
        cmd.arg(script);
    })
    .await
    .unwrap();
    harness
        .send_batch(&json!([{"body":"batch"}]))
        .await
        .unwrap();
    assert_eq!(
        harness.next_turn().await.unwrap().total_cost_usd,
        Some(0.001)
    );
    assert!(harness.close().await.unwrap().success());
}

#[tokio::test]
#[cfg(unix)]
async fn rejects_bad_output_and_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    for model in ["malformed", "eof", "oversize"] {
        let mut harness = spawn_fixture(dir.path(), model).await;
        harness.send_batch(&json!([])).await.unwrap();
        assert!(harness.next_turn().await.is_err(), "{model}");
        harness.close().await.unwrap();
    }
    let mut harness = spawn_fixture(dir.path(), "failure").await;
    harness.send_batch(&json!([])).await.unwrap();
    assert!(harness.next_turn().await.is_err());
    assert!(harness.close().await.is_err());
}

#[tokio::test]
#[cfg(unix)]
async fn rejects_oversize_batch_and_malformed_call_log() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = spawn_fixture(dir.path(), "offline").await;
    assert!(harness.send_batch(&json!("x".repeat(65537))).await.is_err());
    harness.close().await.unwrap();
    let path = dir.path().join("bad.jsonl");
    tokio::fs::write(&path, "not json\n").await.unwrap();
    assert!(read_calls(&path).await.is_err());
}

#[tokio::test]
#[cfg(unix)]
async fn stalled_process_times_out_and_is_reaped() {
    let dir = tempfile::tempdir().unwrap();
    let mut harness = spawn_fixture(dir.path(), "stall").await;
    harness.send_batch(&json!([])).await.unwrap();
    assert!(
        harness
            .next_turn()
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    assert!(harness.close().await.unwrap().success());
}

#[tokio::test]
async fn mcp_config_points_at_fake_server_and_private_log() {
    let dir = tempfile::tempdir().unwrap();
    let opts = options(dir.path());
    opts.write_mcp_config(std::path::Path::new("/offline/fake-agmen-mcp"))
        .await
        .unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&tokio::fs::read(&opts.mcp_config).await.unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"]["agmen"]["command"],
        "/offline/fake-agmen-mcp"
    );
    assert_eq!(
        config["mcpServers"]["agmen"]["env"]["S7_LOG"],
        opts.log.to_str().unwrap()
    );
}
