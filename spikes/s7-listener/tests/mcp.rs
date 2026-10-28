use std::process::Stdio;
use std::time::Duration;

use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use s7_listener::harness::read_calls;
use serde_json::json;

#[tokio::test]
async fn fake_mcp_serves_six_tools_and_records_calls_over_stdio() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls.jsonl");
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_fake-agmen-mcp"))
            .env("S7_LOG", &log)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let io = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
        let client = ().serve(io).await.unwrap();
        let mut tools = client.list_all_tools().await.unwrap();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(
            tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>(),
            ["answer", "delegate", "dm", "post", "state", "workers"]
        );
        let delegate = tools.iter().find(|t| t.name == "delegate").unwrap();
        let schema = serde_json::to_value(&delegate.input_schema).unwrap();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("role"))
        );
        assert!(schema["properties"].get("agent").is_some());
        for (tool, args) in [
            ("answer", json!({"reply_to":"request1", "body":"answer"})),
            ("dm", json!({"to":["person1"], "body":"forward"})),
            ("post", json!({"verb":"ALERT", "body":"escalate"})),
            ("state", json!({"key":"roster"})),
            ("workers", json!({})),
            (
                "delegate",
                json!({"reply_to":"request1", "brief":"fix it", "role":"engineer",
                "agent":"claude", "tier":"standard", "budget_usd":1.0}),
            ),
        ] {
            let result = client
                .call_tool(
                    CallToolRequestParams::new(tool)
                        .with_arguments(args.as_object().unwrap().clone()),
                )
                .await
                .unwrap();
            assert_ne!(result.is_error, Some(true), "{tool}: {result:?}");
        }
        let calls = read_calls(&log).await.unwrap();
        assert_eq!(calls.len(), 6);
        assert_eq!(calls[5].tool, "delegate");
        assert_eq!(calls[5].args["role"], "engineer");
        assert_eq!(calls[5].args["agent"], "claude");
        let invalid = client
            .call_tool(
                CallToolRequestParams::new("delegate").with_arguments(
                    json!({"reply_to":"request1", "brief":"x", "tier":"standard", "budget_usd":1})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await;
        assert!(invalid.is_err() || invalid.unwrap().is_error == Some(true));
        assert_eq!(read_calls(&log).await.unwrap().len(), 6);
        client.cancel().await.unwrap();
        assert!(child.wait().await.unwrap().success());
    })
    .await
    .expect("offline MCP test completes within 10 seconds");
}

#[tokio::test]
async fn fake_mcp_requires_log_path() {
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_fake-agmen-mcp"))
            .env_remove("S7_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!status.success());
}
