use super::*;
use serde_json::{Value, json};

fn rule(action: &str, effect: &str) -> Value {
    json!({"action": action, "resource": "*", "effect": effect})
}

fn role(actions: &[&'static str], dir: &Path) -> RoleRun {
    RoleRun {
        tools: String::new(),
        actions: actions.to_vec(),
        prompt_file: dir.join("role-prompt.md"),
    }
}

fn opencode_spec(dir: &Path) -> RunSpec {
    std::fs::write(
        dir.join("mcp.json"),
        r#"{"mcpServers":{"agmen":{"command":"/opt/agmen","args":["mcp"],"env":{"AGMEN_IDENTITY":"agent001","AGMEN_MANAGED":"1"}}}}"#,
    )
    .unwrap();
    RunSpec {
        provider: Provider::OpenCode,
        model: Some("anthropic/claude-haiku-5-5".into()),
        ..spec(dir)
    }
}

fn runner(program: std::path::PathBuf) -> ClaudeRunner {
    ClaudeRunner {
        opencode: program,
        timeout: std::time::Duration::from_secs(20),
        shell: Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: format!("{LOGIN_PATH_MARKER}\n/usr/bin:/bin\n"),
        }),
        ..ClaudeRunner::default()
    }
}

/// A `step_finish` line as OpenCode 2.2.6 printed it on 2026-11-13.
const STEP: &str = r#"{"type":"step_finish","timestamp":1794589133900,"sessionID":"ses_x","part":{"id":"prt_1","sessionID":"ses_x","messageID":"msg_1","type":"step-finish","reason":"stop","snapshot":"85","cost":0.010700025,"tokens":{"input":4,"output":4,"reasoning":0,"cache":{"read":0,"write":85581}}}}"#;

#[test]
fn args_name_the_session_model_and_prompt_and_only_owner_batches_auto_approve() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = opencode_spec(dir.path());
    assert_eq!(
        opencode::args(&request),
        [
            "run",
            "--standalone",
            "--format",
            "json",
            "--agent",
            "agmen",
            "--session",
            "ses_a6cddf07a8fd46e195852571af65af24",
            "--model",
            "anthropic/claude-haiku-5-5",
            "--auto",
            "message with spaces and $(literal)",
        ]
    );
    request.owner_only = false;
    request.model = None;
    let args = opencode::args(&request);
    assert!(!args.iter().any(|arg| arg == "--auto" || arg == "--model"));
    assert_eq!(args.last().unwrap(), "message with spaces and $(literal)");
}

#[test]
fn rules_lock_a_batch_from_others_and_a_role_to_what_each_may_use() {
    let dir = tempfile::tempdir().unwrap();
    let mut request = opencode_spec(dir.path());
    // The owner's own batch with no role: everything but inbox.
    assert_eq!(
        opencode::permissions(&request),
        [rule("agmen_inbox", "deny")]
    );
    request.owner_only = false;
    assert_eq!(
        opencode::permissions(&request),
        [
            rule("*", "deny"),
            rule("read", "allow"),
            rule("glob", "allow"),
            rule("grep", "allow"),
            // Only the tools that send a reply.
            rule("agmen_chat", "allow"),
            rule("agmen_answer", "allow"),
            rule("agmen_ask", "allow"),
            rule("agmen_dm", "allow"),
        ]
    );
    request.role = Some(role(&["read", "edit", "shell"], dir.path()));
    assert_eq!(
        opencode::permissions(&request),
        [
            rule("*", "deny"),
            rule("read", "allow"),
            rule("agmen_chat", "allow"),
            rule("agmen_answer", "allow"),
            rule("agmen_ask", "allow"),
            rule("agmen_dm", "allow"),
        ]
    );
    request.owner_only = true;
    assert_eq!(
        opencode::permissions(&request),
        [
            rule("*", "deny"),
            rule("read", "allow"),
            rule("edit", "allow"),
            rule("shell", "allow"),
            rule("agmen_*", "allow"),
            // Only the owner's batch may reach outside the worktree.
            rule("external_directory", "allow"),
            // Last, so it wins over the agmen tools' allow.
            rule("agmen_inbox", "deny"),
        ]
    );
}

#[test]
fn only_a_step_finish_line_adds_cost() {
    assert_eq!(opencode::step_cost(STEP.as_bytes()).usd(), "0.010700");
    let small = STEP.replace("0.010700025", "3.01145e-4");
    assert_eq!(opencode::step_cost(small.as_bytes()).usd(), "0.000301");
    for line in [
        STEP.replace("step_finish", "step_start"),
        r#"{"type":"error","error":{"type":"provider.auth","message":"invalid x-api-key"}}"#.into(),
        r#"{"type":"step_finish","part":{"cost":"1"}}"#.into(),
        r#"{"type":"step_finish","part":{"cost":-1}}"#.into(),
        "not json".into(),
    ] {
        assert_eq!(opencode::step_cost(line.as_bytes()), Money::ZERO, "{line}");
    }
}

#[tokio::test]
async fn batch_gets_its_config_and_its_step_costs_are_summed() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(
        dir.path(),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > args-seen\nprintf '%s' \"$OPENCODE_CONFIG_CONTENT\" > config-seen\nprintf '%s' \"$AGMEN_IDENTITY\" > identity-seen\nprintf '%s' \"$OPENCODE_DISABLE_PROJECT_CONFIG\" > project-config-off\nprintf '%s' \"$PWD\" > pwd-seen\nprintf '%s\\n' '{STEP}'\nprintf '%s\\n' '{{\"type\":\"text\",\"part\":{{\"text\":\"ok\"}}}}'\nprintf '%s\\n' '{more}'\n",
            more = STEP.replace("0.010700025", "0.5"),
        ),
    );
    let mut request = opencode_spec(dir.path());
    request.role = Some(role(&["read"], dir.path()));
    let (_send, stop) = tokio::sync::oneshot::channel();
    let result = runner(program).run(request, stop).await.unwrap();
    assert_eq!(result.total_cost_usd, "0.510700");
    let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
    assert!(
        read("args-seen")
            .starts_with("run\n--standalone\n--format\njson\n--agent\nagmen\n--session\nses_")
    );
    assert_eq!(read("identity-seen"), "agent001");
    // The worktree's own OpenCode config is the repository's, not the person's.
    assert_eq!(read("project-config-off"), "1");
    // OpenCode works where PWD says, so it must name the agent's worktree.
    assert_eq!(read("pwd-seen"), dir.path().to_str().unwrap());
    let config: Value = serde_json::from_str(&read("config-seen")).unwrap();
    assert_eq!(
        config["mcp"]["servers"]["agmen"],
        json!({"type": "local", "command": ["/opt/agmen", "mcp"], "cwd": dir.path(),
               "environment": {"AGMEN_IDENTITY": "agent001", "AGMEN_MANAGED": "1"},
               "codemode": false})
    );
    assert_eq!(config["permissions"][0], rule("*", "deny"));
    // The batch's own agent carries the same rules, after any other agent's.
    assert_eq!(
        config["agents"]["agmen"],
        json!({"mode": "primary", "permissions": config["permissions"]})
    );
    let owner = dir.path().join("opencode-owner.md");
    assert_eq!(
        config["instructions"],
        json!([owner, dir.path().join("role-prompt.md")])
    );
    let text = std::fs::read_to_string(&owner).unwrap();
    assert!(text.contains("exactly \"owner001\""), "{text}");
    assert!(!text.contains("other than your owner"));
    assert_eq!(
        std::fs::metadata(&owner).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn batch_from_others_is_told_so_and_a_failed_exit_keeps_the_cost() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(
        dir.path(),
        &format!(
            "#!/bin/sh\nprintf '%s' \"$OPENCODE_CONFIG_CONTENT\" > config-seen\nprintf '%s\\n' '{STEP}'\nprintf '%s\\n' '{{\"type\":\"error\",\"error\":{{\"type\":\"provider.auth\",\"message\":\"invalid x-api-key\"}}}}'\nexit 1\n"
        ),
    );
    let mut request = opencode_spec(dir.path());
    request.owner_only = false;
    let (_send, stop) = tokio::sync::oneshot::channel();
    let error = runner(program).run(request, stop).await.unwrap_err();
    assert_eq!(error.reported_cost(), Some("0.010700"));
    assert_eq!(
        error.text(Provider::OpenCode),
        "OpenCode batch failed after reporting $0.010700"
    );
    let text = std::fs::read_to_string(dir.path().join("opencode-others.md")).unwrap();
    assert!(text.contains("other than your owner"), "{text}");
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("config-seen")).unwrap())
            .unwrap();
    assert_eq!(config["permissions"].as_array().unwrap().len(), 8);
}

#[tokio::test]
async fn batch_is_stopped_when_its_steps_reach_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let step = STEP.replace("0.010700025", "0.6");
    // It would run for a minute; the cap ends it after the second step.
    let program = fake(
        dir.path(),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' '{step}'\nprintf '%s\\n' '{step}'\n/bin/sleep 60\nprintf '%s\\n' '{step}'\n"
        ),
    );
    let mut request = opencode_spec(dir.path());
    request.budget_cap = Some(Money::from_usd("1").unwrap());
    let (_send, stop) = tokio::sync::oneshot::channel();
    let started = std::time::Instant::now();
    let error = runner(program.clone())
        .run(request, stop)
        .await
        .unwrap_err();
    assert_eq!(error.reported_cost(), Some("1.200000"));
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    // With budgets off the same output is only counted.
    let quick = fake(
        dir.path(),
        &format!("#!/bin/sh\nprintf '%s\\n' '{step}'\nprintf '%s\\n' '{step}'\n"),
    );
    let mut request = opencode_spec(dir.path());
    request.cap_budget = false;
    let (_send, stop) = tokio::sync::oneshot::channel();
    let result = runner(quick).run(request, stop).await.unwrap();
    assert_eq!(result.total_cost_usd, "1.200000");
}

#[tokio::test]
async fn a_line_too_long_to_parse_is_skipped_and_later_steps_still_count() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(
        dir.path(),
        &format!(
            "#!/bin/sh\n/usr/bin/head -c 3000000 /dev/zero | /usr/bin/tr '\\0' 'x'\nprintf '\\n%s\\n' '{STEP}'\n"
        ),
    );
    let (_send, stop) = tokio::sync::oneshot::channel();
    let result = runner(program)
        .run(opencode_spec(dir.path()), stop)
        .await
        .unwrap();
    assert_eq!(result.total_cost_usd, "0.010700");
}

#[tokio::test]
async fn missing_opencode_is_not_installed_and_costs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (_send, stop) = tokio::sync::oneshot::channel();
    let error = runner(dir.path().join("missing-opencode"))
        .run(opencode_spec(dir.path()), stop)
        .await
        .unwrap_err();
    assert_eq!(error.reported_cost(), Some("0"));
    assert_eq!(
        error.text(Provider::OpenCode),
        "OpenCode is not installed or not on your login PATH"
    );
}

#[tokio::test]
async fn providers_list_what_is_installed_and_opencodes_models() {
    let dir = tempfile::tempdir().unwrap();
    // Prints model names, a line that is not one, and a name agmend would refuse.
    let program = fake(
        dir.path(),
        "#!/bin/sh\n[ \"$1 $2\" = \"models --standalone\" ] || exit 2\nprintf '%s\\n' 'anthropic/claude-haiku-5-5' 'ollama/qwen3:32b' 'Loading models' '--bad/flag' ''\n",
    );
    let mut both = runner(program.clone());
    both.program = dir.path().join("missing-claude");
    assert_eq!(
        both.providers().await,
        [json!({"name": "opencode", "title": "OpenCode",
                "models": ["anthropic/claude-haiku-5-5", "ollama/qwen3:32b"]})]
    );
    let mut claude_only = runner(dir.path().join("missing-opencode"));
    claude_only.program = program;
    assert_eq!(
        claude_only.providers().await,
        [json!({"name": "claude", "title": "Claude Code", "models": []})]
    );
}

#[tokio::test]
async fn a_batch_whose_config_cannot_be_built_never_starts_and_costs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake(dir.path(), "#!/bin/sh\ntouch started\n");
    for broken in ["", "not json", r#"{"mcpServers":{}}"#] {
        let request = opencode_spec(dir.path());
        if broken.is_empty() {
            std::fs::remove_file(dir.path().join("mcp.json")).unwrap();
        } else {
            std::fs::write(dir.path().join("mcp.json"), broken).unwrap();
        }
        let (_send, stop) = tokio::sync::oneshot::channel();
        let error = runner(program.clone())
            .run(request, stop)
            .await
            .unwrap_err();
        assert!(!error.started(), "{broken}: {error}");
        assert_eq!(error.reported_cost(), Some("0"), "{broken}");
    }
    assert!(!dir.path().join("started").exists());
}
