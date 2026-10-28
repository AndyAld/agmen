#![cfg(unix)]

use super::*;
use std::os::unix::fs::PermissionsExt as _;
use tokio::time::Instant;

fn args(program: PathBuf) -> ListenerArgs {
    ListenerArgs {
        program,
        model: "claude-haiku-5-5".into(),
        mcp_config: "listener.json".into(),
        prompt_file: "role prompt.xml".into(),
        max_budget_usd: Money::from_usd("2.000000").unwrap(),
    }
}

async fn fake(script: &str, deadline: Duration) -> (tempfile::TempDir, ListenerProcess) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claude");
    std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{}\n", script.trim())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let process = ListenerProcess::spawn(
        &args(path),
        dir.path(),
        &[("PATH".into(), "/usr/bin:/bin".into())],
        deadline,
    )
    .await
    .unwrap();
    (dir, process)
}

const TWO_TURNS: &str = r#"
for turn in 1 2; do
    IFS= read -r _message
    if [ "$turn" -eq 1 ]; then
        total=0.003
        cost=0.003
    else
        total=0.004
        cost=0.001
    fi
    printf '{"type":"result","turn":%s,"pid":%s,"total_cost_usd":%s,"model_usage_since_last_result":{"haiku":{"costUSD":%s}},"is_error":false}\n' "$turn" "$$" "$total" "$cost"
done
"#;

#[test]
fn listener_argv_matches_adr_0009() {
    assert_eq!(
        args("/fake/claude".into()).argv(),
        [
            "-p",
            "--model",
            "claude-haiku-5-5",
            "--output-format",
            "stream-json",
            "--verbose",
            "--tools",
            "Read,Grep,Glob",
            "--strict-mcp-config",
            "--mcp-config",
            "listener.json",
            "--append-system-prompt-file",
            "role prompt.xml",
            "--max-budget-usd",
            "2.000000",
            "--input-format",
            "stream-json"
        ]
        .map(OsString::from)
    );
}

#[tokio::test]
async fn turn_cost_uses_incremental_usage() {
    let (_dir, mut process) = fake(TWO_TURNS, Duration::from_secs(3)).await;
    for cost in ["0.003000", "0.001000"] {
        process.send(b"{}\n").await.unwrap();
        let turn = process.next_turn().await.unwrap();
        assert_eq!(turn.cost.usd(), cost);
        assert!(!turn.is_error);
    }
    process.stop().await;
}

#[tokio::test]
async fn turn_collects_tool_calls() {
    let (_dir, mut process) = fake(r#"
IFS= read -r _message
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"thinking"},{"type":"tool_use","id":"1","name":"git_log","input":{}},{"type":"tool_use","id":"2","name":"git_status","input":{}}]}}' '{"type":"result","model_usage_since_last_result":{},"is_error":true}'
"#, Duration::from_secs(3)).await;
    process.send(b"{}\n").await.unwrap();
    let turn = process.next_turn().await.unwrap();
    assert_eq!(turn.tool_calls.len(), 2);
    assert_eq!(turn.tool_calls[0]["name"], "git_log");
    assert_eq!(turn.tool_calls[1]["name"], "git_status");
    assert!(turn.is_error);
    process.stop().await;
}

#[tokio::test]
async fn process_keeps_context_across_batches() {
    let (_dir, mut process) = fake(TWO_TURNS, Duration::from_secs(3)).await;
    process.send(b"{}\n").await.unwrap();
    let first = process.next_turn().await.unwrap();
    process.send(b"{}\n").await.unwrap();
    let second = process.next_turn().await.unwrap();
    assert_eq!(first.result["turn"], 1);
    assert_eq!(second.result["turn"], 2);
    assert_eq!(first.result["pid"], second.result["pid"]);
    process.stop().await;
}

#[tokio::test]
async fn oversized_line_is_an_error() {
    let (_dir, mut process) = fake(
        r#"
IFS= read -r _message
printf '%02097152d\n' 0
"#,
        Duration::from_secs(3),
    )
    .await;
    process.send(b"{}\n").await.unwrap();
    let started = Instant::now();
    assert!(matches!(
        process.next_turn().await,
        Err(ProcessError::OversizeLine { .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
    process.stop().await;
}

#[tokio::test]
async fn turn_times_out() {
    let (_dir, mut process) = fake(
        r#"
IFS= read -r _message
sleep 30
"#,
        Duration::from_secs(1),
    )
    .await;
    process.send(b"{}\n").await.unwrap();
    let started = Instant::now();
    let error = process.next_turn().await.unwrap_err();
    assert!(matches!(error, ProcessError::Timeout { .. }));
    assert!(std::error::Error::source(&error).is_some());
    assert!(started.elapsed() < Duration::from_secs(5));
    process.stop().await;
}

async fn recorded_pids(dir: &Path) -> Vec<u32> {
    tokio::time::timeout(Duration::from_secs(3), async {
        for _ in 0..300 {
            if let Ok(text) = tokio::fs::read_to_string(dir.join("pids")).await
                && text.lines().count() == 2
            {
                return text.lines().map(|pid| pid.parse().unwrap()).collect();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fake did not record pids");
    })
    .await
    .unwrap()
}

async fn assert_dead(pids: &[u32]) {
    tokio::time::timeout(Duration::from_secs(5), async {
        for _ in 0..100 {
            let mut alive = false;
            for pid in pids {
                alive |= tokio::process::Command::new("/bin/kill")
                    .args(["-0", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .status()
                    .await
                    .unwrap()
                    .success();
            }
            if !alive {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fake process group still alive: {pids:?}");
    })
    .await
    .unwrap();
}

const WITH_CHILD: &str = r#"
sleep 30 &
child=$!
printf '%s\n%s\n' "$$" "$child" >pids
wait "$child"
"#;

#[tokio::test]
async fn stop_kills_the_process_group() {
    let (dir, process) = fake(WITH_CHILD, Duration::from_secs(3)).await;
    let pids = recorded_pids(dir.path()).await;
    process.stop().await;
    assert_dead(&pids).await;
}

#[tokio::test]
async fn process_drop_kills_the_process_group() {
    let (dir, process) = fake(WITH_CHILD, Duration::from_secs(3)).await;
    let pids = recorded_pids(dir.path()).await;
    drop(process);
    assert_dead(&pids).await;
}

#[tokio::test]
async fn process_rejects_invalid_timeout_and_oversized_input() {
    for deadline in [
        Duration::ZERO,
        Duration::from_millis(999),
        Duration::from_secs(181),
    ] {
        assert!(matches!(
            ListenerProcess::spawn(&args("/nonexistent".into()), Path::new("/"), &[], deadline)
                .await,
            Err(ProcessError::InvalidTimeout)
        ));
    }
    let (_dir, mut process) = fake("sleep 30", Duration::from_secs(1)).await;
    assert!(matches!(
        process.send(&vec![b'x'; 64 * 1024 + 1]).await,
        Err(ProcessError::OversizeInput)
    ));
    process.stop().await;
}

#[tokio::test]
async fn turn_missing_usage_is_zero() {
    let (_dir, mut process) = fake(
        r#"
printf '%s\n' '{"type":"result","total_cost_usd":900}'
"#,
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(process.next_turn().await.unwrap().cost.usd(), "0.000000");
    process.stop().await;
}

#[tokio::test]
async fn turn_cost_preserves_raw_precision_and_saturates() {
    let (_dir, mut process) = fake(r#"
printf '%s\n' '{"type":"result","model_usage_since_last_result":{"a":{"costUSD":0.00000099999999999999999999999999999},"b":{"costUSD":0.00123499999999999999999999999999999}}}' '{"type":"result","model_usage_since_last_result":{"a":{"costUSD":1e30},"b":{"costUSD":1e30}}}'
"#, Duration::from_secs(3)).await;
    assert_eq!(process.next_turn().await.unwrap().cost.usd(), "0.001234");
    assert_eq!(
        process.next_turn().await.unwrap().cost,
        Money::from_usd("1e30").unwrap()
    );
    process.stop().await;
}

#[tokio::test]
async fn turn_invalid_cost_is_an_error() {
    for cost in ["\"0.1\"", "-0.1", "null", "{}"] {
        let script = format!(
            "printf '%s\\n' '{{\"type\":\"result\",\"model_usage_since_last_result\":{{\"a\":{{\"costUSD\":{cost}}}}}}}'"
        );
        let (_dir, mut process) = fake(&script, Duration::from_secs(3)).await;
        assert!(matches!(
            process.next_turn().await,
            Err(ProcessError::InvalidCost { .. })
        ));
        process.stop().await;
    }
}

#[tokio::test]
async fn turn_invalid_json_and_too_many_events_are_errors() {
    for (script, too_many) in [
        ("printf '%s\\n' 'not-json'", false),
        (
            r#"
for _event in $(seq 1 1024); do
    printf '%s\n' '{"type":"system"}'
done
"#,
            true,
        ),
    ] {
        let (_dir, mut process) = fake(script, Duration::from_secs(3)).await;
        let error = process.next_turn().await.unwrap_err();
        if too_many {
            assert!(matches!(error, ProcessError::TooManyEvents { .. }));
        } else {
            assert!(matches!(error, ProcessError::InvalidJson { .. }));
        }
        process.stop().await;
    }
}

#[tokio::test]
async fn process_early_exit_retains_only_stderr_tail() {
    let (_dir, mut process) = fake(
        r#"
printf '%016384d' 0 >&2
printf 'tail-marker' >&2
exit 7
"#,
        Duration::from_secs(3),
    )
    .await;
    let error = process.next_turn().await.unwrap_err();
    let ProcessError::EarlyExit { stderr, .. } = error else {
        panic!("unexpected error: {error:?}");
    };
    assert_eq!(stderr.len(), 8 * 1024);
    assert!(stderr.ends_with("tail-marker"));
    process.stop().await;
}

#[tokio::test]
async fn process_write_timeout_kills_the_process_group() {
    let (dir, mut process) = fake(WITH_CHILD, Duration::from_secs(3)).await;
    let pids = recorded_pids(dir.path()).await;
    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(12), async {
        let message = vec![b'x'; MAX_BATCH_BYTES];
        // Fill the pipe across platforms before asserting the write deadline.
        for _ in 0..32 {
            process.send(&message).await?;
        }
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        matches!(
            result,
            Err(ProcessError::Timeout {
                operation: "writing input",
                ..
            })
        ),
        "unexpected send result: {result:?}"
    );
    assert!(started.elapsed() >= WRITE_TIMEOUT);
    process.stop().await;
    assert_dead(&pids).await;
}

#[tokio::test]
async fn process_clears_environment_and_uses_cwd() {
    let (dir, mut process) = fake(
        r#"
printf '{"type":"result","home":"%s","cwd":"%s"}\n' "${HOME-unset}" "$(pwd -P)"
"#,
        Duration::from_secs(3),
    )
    .await;
    let turn = process.next_turn().await.unwrap();
    assert_eq!(turn.result["home"], "unset");
    assert_eq!(
        turn.result["cwd"],
        dir.path().canonicalize().unwrap().to_str().unwrap()
    );
    process.stop().await;
}

#[tokio::test]
async fn turn_malformed_usage_map_is_an_error() {
    for usage in ["[]", r#"{"a":{}}"#, r#"{"a":true}"#] {
        let script = format!(
            "printf '%s\\n' '{{\"type\":\"result\",\"model_usage_since_last_result\":{usage}}}'"
        );
        let (_dir, mut process) = fake(&script, Duration::from_secs(3)).await;
        assert!(matches!(
            process.next_turn().await,
            Err(ProcessError::InvalidCost { .. })
        ));
        process.stop().await;
    }
}

#[tokio::test]
async fn process_drains_stderr_before_result() {
    let (_dir, mut process) = fake(
        r#"
printf '%0131072d' 0 >&2
printf '%s\n' '{"type":"result","model_usage_since_last_result":{}}'
"#,
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(process.next_turn().await.unwrap().cost.usd(), "0.000000");
    process.stop().await;
}
