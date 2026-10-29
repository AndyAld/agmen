#![cfg(unix)]
#![cfg(test)]

mod support;

use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agmen_local::{API, ErrorCode, Request, Response, frame};
use serde_json::{Value, json};
use support::{Home, Worktree, assert_exit, stderr, stdout};

struct Fake {
    requests: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fake {
    fn start(home: &Home, answer: Value) -> Self {
        Self::reply(home, move |request| {
            let answer = if request.method == "console_snapshot" {
                json!({})
            } else {
                answer.clone()
            };
            Response::ok(request.id, answer)
        })
    }

    fn reply(home: &Home, reply: impl Fn(&Request) -> Response + Send + 'static) -> Self {
        let path = home.state_base().join(agmen_client::AGMEND_SOCKET);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(10), async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                for _ in 0..4 {
                    let Ok(request) = frame::read_async::<Request>(&mut socket).await else {
                        return;
                    };
                    recorded.lock().unwrap().push(request.clone());
                    let answer = match request.method.as_str() {
                        "hello" => Response::ok(request.id, json!({"api":API})),
                        _ => reply(&request),
                    };
                    frame::write_async(&mut socket, &answer).await.unwrap();
                }
            })
            .await
            .unwrap();
        });
        Self { requests, task }
    }

    fn request(&self, method: &str) -> Request {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .find(|req| req.method == method)
            .unwrap()
            .clone()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn joined(home: &Home) -> Worktree {
    let wt = Worktree::new(home);
    std::fs::write(wt.path().join(".git/info/exclude"), ".agmen/\n").unwrap();
    std::fs::create_dir(wt.path().join(".agmen")).unwrap();
    std::fs::write(wt.path().join(".agmen/identity.json"), serde_json::to_vec(&json!({
        "tenant":"tenant01", "project":"project1", "identity_id":"parent01", "owner_id":"owner001", "handle":"owner/parent", "hub_url":"http://127.0.0.1:9", "broker_url":"mqtt://127.0.0.1:9"
    })).unwrap()).unwrap();
    wt
}

const LOCAL_CREDENTIAL: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn listener_start_sends_project_root_and_model() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let wt = joined(&home);
    let fake = Fake::start(
        &home,
        json!({"identity_id":"listen01","handle":"owner/listener"}),
    );
    let nested = wt.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let out = home
        .run(
            &nested,
            &[
                "listener",
                "start",
                "--project",
                "project1",
                "--model",
                "claude-haiku-5-5",
            ],
        )
        .await;
    assert_exit(&out, 0);
    assert_eq!(stdout(&out), "listen01 owner/listener\n");
    assert_eq!(
        fake.request("listener_start").params,
        json!({"project":"project1","root":wt.path(),"model":"claude-haiku-5-5"})
    );
}

#[tokio::test]
async fn listener_start_has_no_coordinator_flag() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({}));
    let out = home
        .run(
            home.path(),
            &[
                "listener",
                "start",
                "--project",
                "project1",
                "--coordinator",
            ],
        )
        .await;
    assert_exit(&out, 2);
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delegate_is_not_a_command() {
    let home = Home::new();
    let out = home.run(home.path(), &["delegate"]).await;
    assert_exit(&out, 2);
}

#[tokio::test]
async fn listener_status_prints_rows_and_json() {
    let rows = json!([
        {"project":"project1","handle":"owner/listener","state":"running","model":"haiku","today_usd":"0.10","daily_usd":"2.00"},
        {"project":"project2","handle":"owner/listener","state":"failed","model":"sonnet","today_usd":"0.20","daily_usd":"3.00","error":"a\u{1b}[31mb"}
    ]);
    for json_output in [false, true] {
        let home = Home::new_in(std::path::Path::new("/tmp"));
        let fake = Fake::start(&home, rows.clone());
        let wt = joined(&home);
        let args = if json_output {
            vec!["listener", "status", "--json"]
        } else {
            vec!["listener", "status"]
        };
        let out = home.run(&wt.path(), &args).await;
        assert_exit(&out, 0);
        if json_output {
            assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap(), rows);
        } else {
            assert_eq!(
                stdout(&out),
                "project1 owner/listener running haiku 0.10/2.00 USD\nproject2 owner/listener failed sonnet 0.20/3.00 USD  error: a[31mb\n"
            );
        }
        assert_eq!(
            fake.request("listener_status").params,
            json!({"project":"project1"})
        );
    }
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let _fake = Fake::start(&home, json!([]));
    let out = home
        .run(
            home.path(),
            &["listener", "status", "--project", "project1"],
        )
        .await;
    assert_exit(&out, 0);
    assert_eq!(stdout(&out), "no listener\n");
}

#[tokio::test]
async fn listener_stop_sends_project() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({}));
    let wt = joined(&home);
    let out = home.run(&wt.path(), &["listener", "stop"]).await;
    assert_exit(&out, 0);
    assert_eq!(stdout(&out), "stopped project1\n");
    assert_eq!(
        fake.request("listener_stop").params,
        json!({"project":"project1"})
    );
}

#[tokio::test]
async fn listener_set_sends_only_given_keys() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({}));
    let out = home
        .run(
            home.path(),
            &[
                "listener",
                "set",
                "--project",
                "project1",
                "--daily",
                "8",
                "--accept-from",
                "+adam/coord",
                "--accept-from",
                "-eve/x",
                "--max-workers",
                "2",
            ],
        )
        .await;
    assert_exit(&out, 0);
    assert_eq!(
        fake.request("listener_set").params,
        json!({"project":"project1","daily_usd":"8","accept_from":["+adam/coord","-eve/x"],"max_workers":2})
    );
    let out = home
        .run(home.path(), &["listener", "set", "--project", "project1"])
        .await;
    assert_exit(&out, 2);
}

#[tokio::test]
async fn listener_set_coordinator_refusal_reaches_stderr() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let _fake = Fake::reply(&home, |request| {
        Response::err(
            request.id,
            ErrorCode::Forbidden,
            "coordinators always reach your listener",
        )
    });
    let out = home
        .run(
            home.path(),
            &[
                "listener",
                "set",
                "--project",
                "project1",
                "--accept-from",
                "-adam/coord",
            ],
        )
        .await;
    assert_exit(&out, 1);
    assert!(stderr(&out).contains("coordinators always reach your listener"));
}

#[tokio::test]
async fn settings_apply_cancel_undo_send_decide_and_undo() {
    for (command, method, apply) in [
        ("apply", "settings_decide", Some(true)),
        ("cancel", "settings_decide", Some(false)),
        ("undo", "settings_undo", None),
    ] {
        let home = Home::new_in(std::path::Path::new("/tmp"));
        let fake = Fake::start(&home, json!({}));
        let wt = joined(&home);
        let out = home
            .run(&wt.path(), &["settings", command, "change01"])
            .await;
        assert_exit(&out, 0);
        let mut expected = json!({"project":"project1","id":"change01"});
        if let Some(apply) = apply {
            expected["apply"] = json!(apply);
        }
        assert_eq!(fake.request(method).params, expected);
        let verb = match command {
            "apply" => "applied",
            "cancel" => "cancelled",
            _ => "undone",
        };
        assert_eq!(stdout(&out), format!("{verb} change01\n"));
    }
}

#[tokio::test]
async fn listener_commands_check_the_socket() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({}));
    std::fs::set_permissions(
        home.state_base().join(agmen_client::AGMEND_SOCKET),
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    let out = home
        .run(home.path(), &["listener", "stop", "--project", "project1"])
        .await;
    assert_exit(&out, 1);
    assert!(stderr(&out).contains("0666"));
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mcp_listener_without_credentials_exits_2() {
    for partial in [false, true] {
        let home = Home::new();
        let mut command = home.agmen(home.path(), &["mcp", "--listener"]);
        command
            .env_remove("AGMEN_IDENTITY")
            .env_remove("AGMEN_LOCAL_TOKEN");
        if partial {
            command.env("AGMEN_IDENTITY", "listen01");
        }
        let out = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        assert_exit(&out, 2);
        let expected = if partial {
            "AGMEN_IDENTITY and AGMEN_LOCAL_TOKEN must be set together"
        } else {
            "agmen mcp --listener needs AGMEN_IDENTITY and AGMEN_LOCAL_TOKEN; agmend sets them in the listener's MCP config"
        };
        assert!(stderr(&out).contains(expected));
        assert!(!stderr(&out).contains(LOCAL_CREDENTIAL));
    }
}

#[tokio::test]
async fn mcp_listener_reads_identity_and_token_from_env() {
    use rmcp::ServiceExt as _;
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({"text":"fake log","truncated":false}));
    // A corrupt identity file proves listener mode does not load joined state.
    std::fs::create_dir(home.path().join(".agmen")).unwrap();
    std::fs::write(home.path().join(".agmen/identity.json"), "invalid").unwrap();
    let mut command =
        tokio::process::Command::from(home.agmen(home.path(), &["mcp", "--listener"]));
    command
        .env("AGMEN_IDENTITY", "listen01")
        .env("AGMEN_LOCAL_TOKEN", LOCAL_CREDENTIAL);
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().unwrap();
    let transport = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
    let client = tokio::time::timeout(Duration::from_secs(3), ().serve(transport))
        .await
        .unwrap()
        .unwrap();
    let answer = client
        .call_tool(
            rmcp::model::CallToolRequestParams::new("git_log")
                .with_arguments(serde_json::Map::new()),
        )
        .await
        .unwrap();
    assert_eq!(support::mcp::text_of(&answer), "fake log");
    let request = fake.request("git_log");
    assert_eq!(request.as_.unwrap().as_str(), "listen01");
    assert!(request.params["local_token"].as_str() == Some(LOCAL_CREDENTIAL));
    client.cancel().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn listener_set_maps_every_setting_and_bounds_lists() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({}));
    let out = home
        .run(
            home.path(),
            &[
                "listener",
                "set",
                "--project",
                "project1",
                "--model",
                "haiku",
                "--effort",
                "low",
                "--per-delegation",
                "1.25",
                "--listener-daily",
                "3",
                "--reviews",
                "off",
                "--worker-role",
                "reviewer",
                "--worker-role",
                "engineer",
            ],
        )
        .await;
    assert_exit(&out, 0);
    assert_eq!(
        fake.request("listener_set").params,
        json!({"project":"project1","model":"haiku","effort":"low","per_delegation_usd":"1.25","listener_daily_usd":"3","reviews":"off","worker_roles":["reviewer","engineer"]})
    );
    for (flag, count) in [("--accept-from", 65), ("--worker-role", 33)] {
        let mut args = vec!["listener", "set", "--project", "project1"];
        for _ in 0..count {
            args.extend([flag, "value"]);
        }
        let out = home.run(home.path(), &args).await;
        assert_exit(&out, 2);
    }
    assert_eq!(fake.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn mcp_listener_invalid_credentials_and_flag_conflicts() {
    for (identity, credential) in [
        (None, Some(LOCAL_CREDENTIAL)),
        (Some("INVALID!"), Some(LOCAL_CREDENTIAL)),
        (Some("listen01"), Some("INVALID!")),
    ] {
        let home = Home::new();
        let mut command = home.agmen(home.path(), &["mcp", "--listener"]);
        command
            .env_remove("AGMEN_IDENTITY")
            .env_remove("AGMEN_LOCAL_TOKEN");
        if let Some(identity) = identity {
            command.env("AGMEN_IDENTITY", identity);
        }
        if let Some(credential) = credential {
            command.env("AGMEN_LOCAL_TOKEN", credential);
        }
        let out = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        assert_exit(&out, 2);
        assert!(!stderr(&out).contains(LOCAL_CREDENTIAL));
        assert!(!stderr(&out).contains("INVALID!"));
    }
    for (flag, value) in [("--identity", "listen01"), ("--worktree", "/missing")] {
        let home = Home::new();
        let out = home
            .run(home.path(), &["mcp", "--listener", flag, value])
            .await;
        assert_exit(&out, 2);
    }
}
