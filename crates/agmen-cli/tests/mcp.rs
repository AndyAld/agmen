//! `agmen mcp` without a hub: the tools it lists, and staying inert (the
//! not-joined answer, no request, no connection, no token sent) outside a
//! joined worktree, with an identity file the trust test refuses (rulings
//! R42 to R42b), or with `--identity` naming another identity; and the
//! `agmen mcp` binary itself over stdio.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::path::Path;
use std::process::Stdio;

use agmen_cli::McpOptions;
use agmen_client::TokenKey;
use agmen_proto::IdentityId;
use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use support::mcp::{Mcp, NOT_JOINED, Recorder, TOOLS, every_tool_call, text_of};
use support::{Home, Worktree, assert_exit, stderr};

const ID: &str = "idn7k2q9";

#[cfg(unix)]
mod tree_tools {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use agmen_local::{API, ErrorCode, Request, Response, frame};
    use rmcp::model::CallToolResult;
    use serde_json::{Value, json};

    use super::*;

    struct FakeAgmend {
        requests: Arc<Mutex<Vec<Request>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl FakeAgmend {
        fn start(home: &Home, reply: impl Fn(&Request) -> Response + Send + 'static) -> Self {
            let path = home.state_base().join(agmen_client::AGMEND_SOCKET);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let listener = tokio::net::UnixListener::bind(path).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded = requests.clone();
            let task = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(20), async move {
                    // Each tool uses one hello and one identity-scoped call.
                    for _ in 0..4 {
                        let (mut socket, _) = listener.accept().await.unwrap();
                        for _ in 0..2 {
                            let request: Request = frame::read_async(&mut socket).await.unwrap();
                            let answer = if request.method == "hello" {
                                Response::ok(request.id, json!({"api": API}))
                            } else {
                                reply(&request)
                            };
                            recorded.lock().unwrap().push(request);
                            frame::write_async(&mut socket, &answer).await.unwrap();
                        }
                    }
                })
                .await
                .unwrap();
            });
            Self { requests, task }
        }

        fn request(&self, method: &str, token: &str) -> Request {
            let requests = self.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].method, "hello");
            let mut request = requests[1].clone();
            assert_eq!(request.method, method);
            assert_eq!(request.as_.as_ref().unwrap().as_str(), "parent01");
            let credential = request
                .params
                .as_object_mut()
                .unwrap()
                .remove("local_token");
            // A failed comparison must not print the credential.
            assert!(credential.is_some_and(|value| value == token));
            request
        }
    }

    impl Drop for FakeAgmend {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    struct AgentMcp {
        client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
        child: tokio::process::Child,
    }

    impl AgentMcp {
        async fn start(home: &Home, env: &[(&str, &str)]) -> Self {
            let mut cmd = home.agmen(home.path(), &["mcp"]);
            // Model Task 7's explicit mcp.json env, with no inherited credential.
            cmd.env_remove("AGMEN_IDENTITY")
                .env_remove("AGMEN_LOCAL_TOKEN")
                .envs(env.iter().copied());
            let mut cmd = tokio::process::Command::from(cmd);
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = cmd.spawn().unwrap();
            let io = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
            let client = tokio::time::timeout(Duration::from_secs(10), ().serve(io))
                .await
                .unwrap()
                .expect("the MCP handshake");
            Self { client, child }
        }

        async fn call(&self, tool: &'static str, args: Value, token: &str) -> CallToolResult {
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                self.client.call_tool(
                    CallToolRequestParams::new(tool)
                        .with_arguments(args.as_object().unwrap().clone()),
                ),
            )
            .await
            .unwrap()
            .expect("a tool result");
            assert!(!serde_json::to_string(&result).unwrap().contains(token));
            result
        }

        async fn assert_launch_tools(&self, visible: bool) {
            let tools = self.client.list_all_tools().await.unwrap();
            for name in ["launch", "stop_agent"] {
                assert_eq!(
                    tools.iter().any(|tool| tool.name == name),
                    visible,
                    "{name}"
                );
            }
        }

        async fn stop(self, token: &str) -> String {
            self.client.cancel().await.unwrap();
            let output =
                tokio::time::timeout(Duration::from_secs(20), self.child.wait_with_output())
                    .await
                    .unwrap()
                    .unwrap();
            assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
            assert!(output.status.success());
            String::from_utf8(output.stderr).unwrap()
        }
    }

    fn launched() -> Value {
        json!({
            "identity": "child001", "identity_id": "child001", "handle": "andy/k1",
            "worktree": "/fake/k1", "branch": "agent/k1", "state": "idle",
            "parent": "parent01", "depth": 2
        })
    }

    #[tokio::test]
    async fn launch_tools_hidden_without_a_credential() {
        let home = Home::new_in(Path::new("/tmp"));
        let mcp = AgentMcp::start(&home, &[]).await;
        mcp.assert_launch_tools(false).await;
        mcp.stop(&"a".repeat(64)).await;
    }

    #[tokio::test]
    async fn launch_tool_calls_child_launch_as_the_agent() {
        let home = Home::new_in(Path::new("/tmp"));
        let fake = FakeAgmend::start(&home, |request| Response::ok(request.id, launched()));
        let token = "a".repeat(64);
        let mcp = AgentMcp::start(
            &home,
            &[
                ("AGMEN_IDENTITY", "parent01"),
                ("AGMEN_LOCAL_TOKEN", &token),
            ],
        )
        .await;
        mcp.assert_launch_tools(true).await;
        let params = json!({
            "name": "k1", "role": "reviewer", "model": "sonnet",
            "budget_usd": 2.5, "brief": "look at a.rs"
        });
        let result = mcp.call("launch", params.clone(), &token).await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            text_of(&result),
            "launched @andy/k1 (child001) at depth 2 on agent/k1; brief sent"
        );
        assert_eq!(fake.request("child_launch", &token).params, params);
        assert!(mcp.stop(&token).await.is_empty());
    }

    #[tokio::test]
    async fn launch_tool_omits_unsupplied_options() {
        let home = Home::new_in(Path::new("/tmp"));
        let fake = FakeAgmend::start(&home, |request| Response::ok(request.id, launched()));
        let token = "a".repeat(64);
        let mcp = AgentMcp::start(
            &home,
            &[
                ("AGMEN_IDENTITY", "parent01"),
                ("AGMEN_LOCAL_TOKEN", &token),
            ],
        )
        .await;
        let params = json!({"name": "k1", "role": "reviewer"});
        let result = mcp.call("launch", params.clone(), &token).await;
        assert_eq!(result.is_error, Some(false));
        assert_eq!(
            text_of(&result),
            "launched @andy/k1 (child001) at depth 2 on agent/k1"
        );
        assert_eq!(fake.request("child_launch", &token).params, params);
        mcp.stop(&token).await;
    }

    #[tokio::test]
    async fn stop_agent_tool_calls_child_stop() {
        for subtree in [None, Some(false), Some(true)] {
            let home = Home::new_in(Path::new("/tmp"));
            let fake = FakeAgmend::start(&home, |request| {
                Response::ok(
                    request.id,
                    json!({
                        "identity": "child001", "state": "offline", "stopped": ["grand001", "child001"]
                    }),
                )
            });
            let token = "a".repeat(64);
            let mcp = AgentMcp::start(
                &home,
                &[
                    ("AGMEN_IDENTITY", "parent01"),
                    ("AGMEN_LOCAL_TOKEN", &token),
                ],
            )
            .await;
            let mut params = json!({"identity": "child001"});
            if let Some(subtree) = subtree {
                params["subtree"] = json!(subtree);
            }
            let result = mcp.call("stop_agent", params.clone(), &token).await;
            assert_eq!(result.is_error, Some(false));
            assert_eq!(text_of(&result), "stopped grand001, child001");
            assert_eq!(fake.request("child_stop", &token).params, params);
            mcp.stop(&token).await;
        }
    }

    #[tokio::test]
    async fn refusal_text_reaches_the_agent() {
        for (tool, params) in [
            ("launch", json!({"name": "k1", "role": "reviewer"})),
            ("stop_agent", json!({"identity": "child001"})),
        ] {
            let home = Home::new_in(Path::new("/tmp"));
            let _fake = FakeAgmend::start(&home, |request| {
                Response::err(
                    request.id,
                    ErrorCode::Forbidden,
                    "tree methods are refused from an others-tier batch",
                )
            });
            let token = "a".repeat(64);
            let mcp = AgentMcp::start(
                &home,
                &[
                    ("AGMEN_IDENTITY", "parent01"),
                    ("AGMEN_LOCAL_TOKEN", &token),
                ],
            )
            .await;
            let result = mcp.call(tool, params, &token).await;
            assert_eq!(result.is_error, Some(true));
            assert!(
                text_of(&result).contains("tree methods are refused from an others-tier batch")
            );
            mcp.stop(&token).await;
        }
    }

    const IDENTITY_REASON: &str = "AGMEN_IDENTITY must be a valid identity id";
    const TOKEN_REASON: &str = "AGMEN_LOCAL_TOKEN must be 64 lowercase hex digits";
    const PAIR_REASON: &str = "AGMEN_IDENTITY and AGMEN_LOCAL_TOKEN must be set together";

    /// `logged` is what the server says on stderr about the pair: nothing when
    /// neither variable is set, else the reason, which never names a value.
    async fn assert_no_credential(tool: &'static str, env: &[(&str, &str)], logged: &str) {
        let home = Home::new_in(Path::new("/tmp"));
        let fake = FakeAgmend::start(&home, |request| Response::ok(request.id, launched()));
        let token = "a".repeat(64);
        let mcp = AgentMcp::start(&home, env).await;
        mcp.assert_launch_tools(false).await;
        let params = if tool == "launch" {
            json!({"name": "k1", "role": "reviewer"})
        } else {
            json!({"identity": "child001"})
        };
        let result = mcp.call(tool, params, &token).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text_of(&result),
            "no agent identity in this MCP server's environment"
        );
        let stderr = mcp.stop(&token).await;
        assert!(fake.requests.lock().unwrap().is_empty());
        assert_eq!(stderr.is_empty(), logged.is_empty(), "{stderr}");
        assert!(stderr.contains(logged), "{stderr}");
        assert!(!stderr.contains("INVALID!"), "{stderr}");
    }

    #[tokio::test]
    async fn launch_tool_without_credential_sends_no_request() {
        assert_no_credential("launch", &[], "").await;
    }

    #[tokio::test]
    async fn stop_agent_tool_without_credential_sends_no_request() {
        assert_no_credential("stop_agent", &[], "").await;
    }

    #[tokio::test]
    async fn launch_tool_with_empty_token_sends_no_request() {
        assert_no_credential(
            "launch",
            &[("AGMEN_IDENTITY", "parent01"), ("AGMEN_LOCAL_TOKEN", "")],
            TOKEN_REASON,
        )
        .await;
    }

    #[tokio::test]
    async fn stop_agent_tool_with_empty_token_sends_no_request() {
        assert_no_credential(
            "stop_agent",
            &[("AGMEN_IDENTITY", "parent01"), ("AGMEN_LOCAL_TOKEN", "")],
            TOKEN_REASON,
        )
        .await;
    }

    #[tokio::test]
    async fn launch_tools_with_empty_identity_send_no_request() {
        let token = "a".repeat(64);
        for tool in ["launch", "stop_agent"] {
            assert_no_credential(
                tool,
                &[("AGMEN_IDENTITY", ""), ("AGMEN_LOCAL_TOKEN", &token)],
                IDENTITY_REASON,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn launch_tools_with_invalid_credentials_send_no_request() {
        let token = "a".repeat(64);
        for (env, logged) in [
            (vec![("AGMEN_IDENTITY", "parent01")], PAIR_REASON),
            (vec![("AGMEN_LOCAL_TOKEN", token.as_str())], PAIR_REASON),
            (
                vec![
                    ("AGMEN_IDENTITY", "INVALID!"),
                    ("AGMEN_LOCAL_TOKEN", token.as_str()),
                ],
                IDENTITY_REASON,
            ),
            (
                vec![
                    ("AGMEN_IDENTITY", "parent01"),
                    ("AGMEN_LOCAL_TOKEN", "INVALID!"),
                ],
                TOKEN_REASON,
            ),
        ] {
            for tool in ["launch", "stop_agent"] {
                assert_no_credential(tool, &env, logged).await;
            }
        }
    }

    #[tokio::test]
    async fn launch_tools_redact_tokens_in_answers_and_refusals() {
        let token = "a".repeat(64);
        for tool in ["launch", "stop_agent"] {
            for refused in [false, true] {
                let home = Home::new_in(Path::new("/tmp"));
                let echoed = token.clone();
                let fake = FakeAgmend::start(&home, move |request| {
                    if refused {
                        Response::err(
                            request.id,
                            ErrorCode::Forbidden,
                            format!("refused {echoed}\u{1b}\n\u{202e}"),
                        )
                    } else if request.method == "child_launch" {
                        let mut answer = launched();
                        answer["handle"] = json!(format!("andy/{echoed}\u{1b}\n\u{202e}"));
                        Response::ok(request.id, answer)
                    } else {
                        Response::ok(
                            request.id,
                            json!({"stopped": [format!("{echoed}\u{1b}\n\u{202e}")]}),
                        )
                    }
                });
                let mcp = AgentMcp::start(
                    &home,
                    &[
                        ("AGMEN_IDENTITY", "parent01"),
                        ("AGMEN_LOCAL_TOKEN", &token),
                    ],
                )
                .await;
                let params = if tool == "launch" {
                    json!({"name": "k1", "role": "reviewer"})
                } else {
                    json!({"identity": "child001"})
                };
                let result = mcp.call(tool, params, &token).await;
                assert_eq!(result.is_error, Some(refused));
                assert!(!text_of(&result).contains(['\u{1b}', '\n', '\u{202e}']));
                assert!(text_of(&result).contains("<redacted>"));
                assert_eq!(fake.requests.lock().unwrap().len(), 2);
                mcp.stop(&token).await;
            }
        }
    }

    #[tokio::test]
    async fn launch_tools_reject_malformed_daemon_answers() {
        for (tool, answer) in [
            ("launch", json!({})),
            (
                "launch",
                json!({"identity": "child001", "handle": "andy/k1", "branch": "agent/k1", "depth": "two"}),
            ),
            ("stop_agent", json!({})),
            ("stop_agent", json!({"stopped": []})),
            ("stop_agent", json!({"stopped": [2]})),
            (
                "stop_agent",
                json!({"stopped": vec!["child001"; agmen_local::safeguards::MAX_MANAGED + 1]}),
            ),
        ] {
            let home = Home::new_in(Path::new("/tmp"));
            let fake = FakeAgmend::start(&home, move |request| {
                Response::ok(request.id, answer.clone())
            });
            let token = "a".repeat(64);
            let mcp = AgentMcp::start(
                &home,
                &[
                    ("AGMEN_IDENTITY", "parent01"),
                    ("AGMEN_LOCAL_TOKEN", &token),
                ],
            )
            .await;
            let params = if tool == "launch" {
                json!({"name": "k1", "role": "reviewer"})
            } else {
                json!({"identity": "child001"})
            };
            let result = mcp.call(tool, params, &token).await;
            assert_eq!(result.is_error, Some(true));
            assert!(text_of(&result).contains("agmend's answer"));
            assert_eq!(fake.requests.lock().unwrap().len(), 2);
            mcp.stop(&token).await;
        }
    }

    #[tokio::test]
    async fn launch_tool_requires_role_before_contacting_agmend() {
        let home = Home::new_in(Path::new("/tmp"));
        let fake = FakeAgmend::start(&home, |request| Response::ok(request.id, launched()));
        let token = "a".repeat(64);
        let mcp = AgentMcp::start(
            &home,
            &[
                ("AGMEN_IDENTITY", "parent01"),
                ("AGMEN_LOCAL_TOKEN", &token),
            ],
        )
        .await;
        let result = mcp.call("launch", json!({"name": "k1"}), &token).await;
        assert_eq!(result.is_error, Some(true));
        assert!(text_of(&result).contains("role"));
        mcp.stop(&token).await;
        assert!(fake.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn launch_tools_redact_tokens_in_argument_errors() {
        let token = "a".repeat(64);
        for tool in ["launch", "stop_agent"] {
            let home = Home::new_in(Path::new("/tmp"));
            let fake = FakeAgmend::start(&home, |request| Response::ok(request.id, launched()));
            let mcp = AgentMcp::start(
                &home,
                &[
                    ("AGMEN_IDENTITY", "parent01"),
                    ("AGMEN_LOCAL_TOKEN", &token),
                ],
            )
            .await;
            let mut params = if tool == "launch" {
                json!({"name": "k1", "role": "reviewer"})
            } else {
                json!({"identity": "child001"})
            };
            params[&token] = json!(true);
            let result = mcp.call(tool, params, &token).await;
            assert_eq!(result.is_error, Some(true));
            assert!(text_of(&result).contains("<redacted>"));
            mcp.stop(&token).await;
            assert!(fake.requests.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn launch_tools_report_missing_daemon() {
        let token = "a".repeat(64);
        for tool in ["launch", "stop_agent"] {
            let home = Home::new_in(Path::new("/tmp"));
            let mcp = AgentMcp::start(
                &home,
                &[
                    ("AGMEN_IDENTITY", "parent01"),
                    ("AGMEN_LOCAL_TOKEN", &token),
                ],
            )
            .await;
            let params = if tool == "launch" {
                json!({"name": "k1", "role": "reviewer"})
            } else {
                json!({"identity": "child001"})
            };
            let result = mcp.call(tool, params, &token).await;
            assert_eq!(result.is_error, Some(true));
            assert!(text_of(&result).contains("agmend is not running"));
            assert!(mcp.stop(&token).await.is_empty());
        }
    }
}

/// Options for a server in `cwd` with `home`'s token store and state.
fn options(home: &Home, cwd: &Path) -> McpOptions {
    McpOptions::new(cwd, home.tokens(), home.state_base())
}

/// An identity file for `ID` whose hub and broker are `recorder`'s.
fn identity_json(recorder: &Recorder) -> String {
    serde_json::json!({
        "tenant": "tenant01", "project": "project1", "identity_id": ID,
        "handle": "andy/sol", "owner_id": "andyandy",
        "hub_url": recorder.hub, "broker_url": recorder.broker,
    })
    .to_string()
}

/// Stores a token for `ID` at `recorder`'s hub in `home`, so a server
/// that trusted the identity file would send it there.
fn store_token(home: &Home, recorder: &Recorder) {
    let id: IdentityId = ID.parse().unwrap();
    home.tokens()
        .set(&TokenKey::new(&recorder.hub, &id).unwrap(), "secret-token")
        .unwrap();
}

/// Writes `.agmen/identity.json` in `wt` and excludes `.agmen/` as
/// `agmen join` does.
fn join_like_agmen(wt: &Worktree, recorder: &Recorder) {
    std::fs::create_dir(wt.path().join(".agmen")).unwrap();
    std::fs::write(
        wt.path().join(".agmen/identity.json"),
        identity_json(recorder),
    )
    .unwrap();
    std::fs::write(wt.path().join(".git/info/exclude"), ".agmen/\n").unwrap();
}

/// Calls every tool and asserts each answers the not-joined text without
/// marking it an error; then stops the server.
async fn assert_inert(mcp: Mcp) {
    for (tool, args) in every_tool_call() {
        let result = mcp.call(tool, args).await;
        assert_eq!(text_of(&result), NOT_JOINED, "{tool}");
        assert_eq!(result.is_error, Some(false), "{tool}");
    }
    mcp.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_tools() {
    let home = Home::new();
    let mcp = Mcp::start(options(&home, home.path())).await;
    assert_eq!(mcp.tools().await, TOOLS);
    mcp.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_lists_only_role_tools() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    let catalog = agmen_roles::Layers::default().load().unwrap();
    for (role, expected) in [
        (
            "reviewer",
            vec![
                "answer",
                "ask",
                "chat",
                "check_path",
                "dm",
                "found",
                "inbox",
                "lanes",
                "post",
                "read_chat",
                "review",
                "state",
                "template_list",
                "template_save",
            ],
        ),
        ("engineer", TOOLS.to_vec()),
    ] {
        let wt = Worktree::new(&home);
        join_like_agmen(&wt, &recorder);
        let mut file: serde_json::Value = serde_json::from_str(&identity_json(&recorder)).unwrap();
        file["verbs"] = serde_json::to_value(&catalog.get(role).unwrap().verbs).unwrap();
        std::fs::write(wt.path().join(".agmen/identity.json"), file.to_string()).unwrap();
        let mcp = Mcp::start(options(&home, &wt.path())).await;
        assert_eq!(mcp.tools().await, expected, "{role}");
        mcp.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_identity_lists_twelve() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    let wt = Worktree::new(&home);
    join_like_agmen(&wt, &recorder);
    let mcp = Mcp::start(options(&home, &wt.path())).await;
    assert_eq!(mcp.tools().await, TOOLS);
    mcp.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn not_joined_answers_every_tool() {
    let home = Home::new();
    // Outside any git worktree, then in a worktree with no identity file.
    assert_inert(Mcp::start(options(&home, home.path())).await).await;
    let wt = Worktree::new(&home);
    assert_inert(Mcp::start(options(&home, &wt.path())).await).await;
}

/// The control for the inert tests below: with a trusted identity file the
/// server does ask the recorder for a broker token, so a count of zero
/// there means something.
#[tokio::test(flavor = "multi_thread")]
async fn trusted_identity_file_reaches_the_hub() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    join_like_agmen(&wt, &recorder);
    let mcp = Mcp::start(options(&home, &wt.path())).await;
    let result = mcp.call("lanes", serde_json::json!({})).await;
    assert_ne!(text_of(&result), NOT_JOINED);
    assert_eq!(result.is_error, Some(true), "{}", text_of(&result));
    mcp.stop().await;
    assert!(recorder.hits().0 > 0, "{:?}", recorder.hits());
}

#[tokio::test(flavor = "multi_thread")]
async fn tracked_identity_file_leaves_mcp_inert() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    std::fs::create_dir(wt.path().join(".agmen")).unwrap();
    std::fs::write(
        wt.path().join(".agmen/identity.json"),
        identity_json(&recorder),
    )
    .unwrap();
    wt.git(&home, &["add", ".agmen/identity.json"]);
    wt.git(&home, &["commit", "-q", "-m", "an identity file"]);
    assert_inert(Mcp::start(options(&home, &wt.path())).await).await;
    assert_eq!(recorder.hits(), (0, 0));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn symlinked_agmen_dir_leaves_mcp_inert() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::create_dir(elsewhere.path().join(".agmen")).unwrap();
    std::fs::write(
        elsewhere.path().join(".agmen/identity.json"),
        identity_json(&recorder),
    )
    .unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join(".agmen"), wt.path().join(".agmen")).unwrap();
    std::fs::write(wt.path().join(".git/info/exclude"), ".agmen/\n").unwrap();
    assert_inert(Mcp::start(options(&home, &wt.path())).await).await;
    assert_eq!(recorder.hits(), (0, 0));
}

/// A committed `.AGMEN/identity.json` beside a committed `.gitignore`
/// naming `.agmen/`: on a case-insensitive file system it opens as
/// `.agmen/identity.json` and passes check-ignore; on a case-sensitive one
/// there is no `.agmen` at all. Either way nothing connects.
#[tokio::test(flavor = "multi_thread")]
async fn case_variant_identity_dir_leaves_mcp_inert() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    std::fs::create_dir(wt.path().join(".AGMEN")).unwrap();
    std::fs::write(
        wt.path().join(".AGMEN/identity.json"),
        identity_json(&recorder),
    )
    .unwrap();
    std::fs::write(wt.path().join(".gitignore"), ".agmen/\n").unwrap();
    wt.git(&home, &["add", "-f", ".gitignore", ".AGMEN/identity.json"]);
    wt.git(&home, &["commit", "-q", "-m", "a case variant"]);
    assert_inert(Mcp::start(options(&home, &wt.path())).await).await;
    assert_eq!(recorder.hits(), (0, 0));
}

/// A trusted identity file that does not parse is a tool error that says
/// so, not the not-joined text: the worktree is joined, and the model
/// should report the broken file rather than suggest joining.
#[tokio::test(flavor = "multi_thread")]
async fn malformed_identity_file_is_a_tool_error() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    join_like_agmen(&wt, &recorder);
    std::fs::write(wt.path().join(".agmen/identity.json"), "{ not json").unwrap();
    let mcp = Mcp::start(options(&home, &wt.path())).await;
    for (tool, args) in every_tool_call() {
        let result = mcp.call(tool, args).await;
        let text = text_of(&result);
        assert_eq!(result.is_error, Some(true), "{tool}: {text}");
        assert!(
            text.starts_with("agmen: loading .agmen/identity.json failed: "),
            "{tool}: {text}"
        );
        assert!(
            text.contains("is not a valid identity file"),
            "{tool}: {text}"
        );
    }
    mcp.stop().await;
    assert_eq!(recorder.hits(), (0, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn identity_override_must_match_the_worktree() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    join_like_agmen(&wt, &recorder);
    let other: IdentityId = "idother99".parse().unwrap();
    let mcp = Mcp::start(options(&home, &wt.path()).with_identity(other)).await;
    for (tool, args) in every_tool_call() {
        let result = mcp.call(tool, args).await;
        assert_eq!(
            text_of(&result),
            format!("not joined as idother99: this worktree's identity is {ID}"),
            "{tool}"
        );
        assert_eq!(result.is_error, Some(false), "{tool}");
    }
    mcp.stop().await;
    assert_eq!(recorder.hits(), (0, 0));
}

/// The binary, as Claude Code starts it: `agmen mcp` on stdin and stdout,
/// its working directory and environment deciding the identity.
#[tokio::test(flavor = "multi_thread")]
async fn binary_serves_over_stdio() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    let mut cmd = tokio::process::Command::from(home.agmen(&wt.path(), &["mcp"]));
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = cmd.spawn().unwrap();
    let io = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
    let client = ().serve(io).await.expect("the MCP handshake");
    let info = client.peer_info().expect("the server introduced itself");
    let name = info.server_info.as_ref().map(|server| server.name.as_str());
    assert_eq!(name, Some("agmen"));
    assert_eq!(client.list_all_tools().await.unwrap().len(), TOOLS.len());
    let args = serde_json::json!({ "verb": "FYI", "body": "hi" })
        .as_object()
        .unwrap()
        .clone();
    let result = client
        .call_tool(CallToolRequestParams::new("post").with_arguments(args))
        .await
        .unwrap();
    assert_eq!(text_of(&result), NOT_JOINED);
    client.cancel().await.unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(20), child.wait())
        .await
        .expect("agmen mcp exits once stdin closes")
        .unwrap();
    assert!(status.success(), "{status}");
}

async fn call_binary_tool(
    home: &Home,
    cwd: &Path,
    cli_args: &[&str],
    tool: &'static str,
) -> rmcp::model::CallToolResult {
    let mut cmd = tokio::process::Command::from(home.agmen(cwd, cli_args));
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = cmd.spawn().unwrap();
    let io = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
    let client = ().serve(io).await.expect("the MCP handshake");
    let result = client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(serde_json::Map::new()))
        .await
        .unwrap();
    client.cancel().await.unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(20), child.wait())
        .await
        .expect("agmen mcp exits once stdin closes")
        .unwrap();
    assert!(status.success(), "{status}");
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn worktree_flag_finds_the_identity() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    join_like_agmen(&wt, &recorder);
    let outside = tempfile::tempdir().unwrap();
    let wt_path = wt.path();
    let result = call_binary_tool(
        &home,
        outside.path(),
        &["mcp", "--worktree", wt_path.to_str().unwrap()],
        "lanes",
    )
    .await;
    assert_ne!(text_of(&result), NOT_JOINED);
    assert_eq!(result.is_error, Some(true), "{}", text_of(&result));
    assert!(recorder.hits().0 > 0, "{:?}", recorder.hits());
}

#[test]
fn worktree_flag_refuses_relative_and_unexpanded() {
    let home = Home::new();
    let relative = home
        .agmen(home.path(), &["mcp", "--worktree", "repo"])
        .output()
        .unwrap();
    assert_exit(&relative, 2);
    assert!(
        stderr(&relative).contains("absolute"),
        "{}",
        stderr(&relative)
    );

    let unexpanded = home
        .agmen(home.path(), &["mcp", "--worktree", "${workspaceFolder}"])
        .output()
        .unwrap();
    assert_exit(&unexpanded, 2);
    let err = stderr(&unexpanded);
    assert!(err.contains("${workspaceFolder}"), "{err}");
    assert!(err.contains("did not expand"), "{err}");

    let missing = home.path().join("no-such-worktree");
    let missing_out = home
        .agmen(
            home.path(),
            &["mcp", "--worktree", missing.to_str().unwrap()],
        )
        .output()
        .unwrap();
    assert_exit(&missing_out, 2);

    let file = home.path().join("not-a-directory");
    std::fs::write(&file, "hello\n").unwrap();
    let file_out = home
        .agmen(home.path(), &["mcp", "--worktree", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_exit(&file_out, 2);
}

#[test]
fn worktree_flag_needs_a_joined_worktree() {
    let home = Home::new();
    let outside = tempfile::tempdir().unwrap();
    let wt = Worktree::new(&home);
    for dir in [outside.path(), wt.path().as_path()] {
        let out = home
            .agmen(home.path(), &["mcp", "--worktree", dir.to_str().unwrap()])
            .output()
            .unwrap();
        assert_exit(&out, 3);
        assert_eq!(
            stderr(&out).trim(),
            format!(
                "not joined: {} holds no agmen identity; run agmen join there",
                dir.display()
            )
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn worktree_flag_keeps_the_trust_checks() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);

    let wt = Worktree::new(&home);
    let sub = wt.path().join("sub");
    std::fs::create_dir_all(sub.join(".agmen")).unwrap();
    std::fs::write(sub.join(".agmen/identity.json"), identity_json(&recorder)).unwrap();
    std::fs::write(
        wt.path().join(".git/info/exclude"),
        ".agmen/\nsub/.agmen/\n",
    )
    .unwrap();
    let out = home
        .agmen(home.path(), &["mcp", "--worktree", sub.to_str().unwrap()])
        .output()
        .unwrap();
    assert_exit(&out, 1);
    assert!(
        stderr(&out).contains("agmen does not use"),
        "{}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("not at the worktree root"),
        "{}",
        stderr(&out)
    );

    let tracked = Worktree::new(&home);
    std::fs::create_dir(tracked.path().join(".agmen")).unwrap();
    std::fs::write(
        tracked.path().join(".agmen/identity.json"),
        identity_json(&recorder),
    )
    .unwrap();
    tracked.git(&home, &["add", ".agmen/identity.json"]);
    let tracked_path = tracked.path();
    let out = home
        .agmen(
            home.path(),
            &["mcp", "--worktree", tracked_path.to_str().unwrap()],
        )
        .output()
        .unwrap();
    assert_exit(&out, 1);
    assert!(
        stderr(&out).contains("agmen does not use"),
        "{}",
        stderr(&out)
    );

    #[cfg(unix)]
    {
        let linked = Worktree::new(&home);
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir(elsewhere.path().join(".agmen")).unwrap();
        std::fs::write(
            elsewhere.path().join(".agmen/identity.json"),
            identity_json(&recorder),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            elsewhere.path().join(".agmen"),
            linked.path().join(".agmen"),
        )
        .unwrap();
        std::fs::write(linked.path().join(".git/info/exclude"), ".agmen/\n").unwrap();
        let linked_path = linked.path();
        let out = home
            .agmen(
                home.path(),
                &["mcp", "--worktree", linked_path.to_str().unwrap()],
            )
            .output()
            .unwrap();
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains("agmen does not use"),
            "{}",
            stderr(&out)
        );
    }

    assert_eq!(recorder.hits(), (0, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn worktree_flag_with_identity_must_match() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    join_like_agmen(&wt, &recorder);
    let outside = tempfile::tempdir().unwrap();
    let wt_path = wt.path();

    let mismatch = call_binary_tool(
        &home,
        outside.path(),
        &[
            "mcp",
            "--worktree",
            wt_path.to_str().unwrap(),
            "--identity",
            "idother99",
        ],
        "lanes",
    )
    .await;
    assert_eq!(
        text_of(&mismatch),
        format!("not joined as idother99: this worktree's identity is {ID}")
    );
    assert_eq!(mismatch.is_error, Some(false));
    assert_eq!(recorder.hits(), (0, 0));

    let matched = call_binary_tool(
        &home,
        outside.path(),
        &[
            "mcp",
            "--worktree",
            wt_path.to_str().unwrap(),
            "--identity",
            ID,
        ],
        "lanes",
    )
    .await;
    assert!(!text_of(&matched).starts_with("not joined"));
    assert_eq!(matched.is_error, Some(true), "{}", text_of(&matched));
    assert!(recorder.hits().0 > 0, "{:?}", recorder.hits());
}
