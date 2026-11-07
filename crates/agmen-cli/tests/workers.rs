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

#[tokio::test]
async fn launch_sends_agent_launch_with_this_worktree() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let wt = joined(&home);
    let fake = Fake::start(
        &home,
        json!({"identity":"agent001","worktree":"/fake/worker","branch":"agent/worker"}),
    );
    let sub = wt.path().join("nested");
    std::fs::create_dir(&sub).unwrap();
    let out = home
        .run(
            &sub,
            &[
                "launch", "--name", "worker", "--role", "reviewer", "--model", "sonnet",
                "--budget", "2.5",
            ],
        )
        .await;
    assert_exit(&out, 0);
    assert_eq!(
        fake.request("agent_launch").params,
        json!({"project":"project1","name":"worker","root":wt.path(),"role":"reviewer","model":"sonnet","budget_usd":2.5})
    );
    for value in ["agent001", "/fake/worker", "agent/worker"] {
        assert!(stdout(&out).contains(value));
    }
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|req| req.as_.is_none())
    );
}

#[tokio::test]
async fn launch_as_an_agent_sends_child_launch() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(
        &home,
        json!({"identity":"agent001","worktree":"/fake/worker","branch":"agent/worker"}),
    );
    let token = "a".repeat(64);
    let out = home
        .run_env(
            home.path(),
            &[
                "launch",
                "--name",
                "k1",
                "--role",
                "reviewer",
                "--brief",
                "look at a.rs",
            ],
            &[
                ("AGMEN_IDENTITY", "parent01"),
                ("AGMEN_LOCAL_TOKEN", &token),
            ],
        )
        .await;
    assert_exit(&out, 0);
    assert_eq!(stdout(&out), "agent001 /fake/worker agent/worker\n");
    assert!(!stdout(&out).contains(&token));
    assert!(!stderr(&out).contains(&token));
    let mut request = fake.request("child_launch");
    assert_eq!(request.as_.unwrap().as_str(), "parent01");
    assert!(
        request
            .params
            .as_object_mut()
            .unwrap()
            .remove("local_token")
            .is_some_and(|value| value == token)
    );
    assert_eq!(
        request.params,
        json!({"name":"k1","role":"reviewer","brief":"look at a.rs"})
    );
}

#[tokio::test]
async fn launch_with_one_variable_is_a_usage_error() {
    let token = "a".repeat(64);
    for env in [
        vec![("AGMEN_IDENTITY", "parent01")],
        vec![("AGMEN_LOCAL_TOKEN", token.as_str())],
    ] {
        let home = Home::new_in(std::path::Path::new("/tmp"));
        let wt = joined(&home);
        let fake = Fake::start(
            &home,
            json!({"identity":"agent001","worktree":"/fake/worker","branch":"agent/worker"}),
        );
        let out = home
            .run_env(&wt.path(), &["launch", "--name", "k1"], &env)
            .await;
        assert_exit(&out, 2);
        assert_eq!(
            stderr(&out),
            "AGMEN_IDENTITY and AGMEN_LOCAL_TOKEN must be set together\n"
        );
        assert!(!stdout(&out).contains(&token));
        assert!(!stderr(&out).contains(&token));
        assert!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.method == "hello")
        );
    }
}

#[tokio::test]
async fn launch_as_an_agent_refuses_project() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let wt = joined(&home);
    let fake = Fake::start(
        &home,
        json!({"identity":"agent001","worktree":"/fake/worker","branch":"agent/worker"}),
    );
    let token = "a".repeat(64);
    let out = home
        .run_env(
            &wt.path(),
            &["launch", "--name", "k1", "--project", "project2"],
            &[
                ("AGMEN_IDENTITY", "parent01"),
                ("AGMEN_LOCAL_TOKEN", &token),
            ],
        )
        .await;
    assert_exit(&out, 2);
    assert_eq!(
        stderr(&out),
        "--project does not apply to an agent's launch: the child joins its parent's project\n"
    );
    assert!(!stdout(&out).contains(&token));
    assert!(!stderr(&out).contains(&token));
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.method == "hello")
    );
}

#[tokio::test]
async fn launch_as_an_agent_requires_a_role() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(
        &home,
        json!({"identity":"agent001","worktree":"/fake/worker","branch":"agent/worker"}),
    );
    let token = "a".repeat(64);
    let out = home
        .run_env(
            home.path(),
            &["launch", "--name", "k1"],
            &[
                ("AGMEN_IDENTITY", "parent01"),
                ("AGMEN_LOCAL_TOKEN", &token),
            ],
        )
        .await;
    assert_exit(&out, 2);
    assert_eq!(stderr(&out), "--role is required for an agent's launch\n");
    assert!(!stdout(&out).contains(&token));
    assert!(!stderr(&out).contains(&token));
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.method == "hello")
    );
}

#[tokio::test]
async fn launch_without_a_credential_keeps_role_optional() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let wt = joined(&home);
    let fake = Fake::start(
        &home,
        json!({"identity":"agent001","worktree":"/fake/worker","branch":"agent/worker"}),
    );
    let out = home.run(&wt.path(), &["launch", "--name", "k1"]).await;
    assert_exit(&out, 0);
    assert_eq!(
        fake.request("agent_launch").params,
        json!({"project":"project1","name":"k1","root":wt.path()})
    );
}

#[tokio::test]
async fn brief_without_a_credential_is_a_usage_error() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({}));
    let out = home
        .run(
            home.path(),
            &["launch", "--name", "k1", "--brief", "look at a.rs"],
        )
        .await;
    assert_exit(&out, 2);
    assert_eq!(
        stderr(&out),
        "--brief is for an agent's launch; message your agent after launching it\n"
    );
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.method == "hello")
    );
}

#[tokio::test]
async fn stop_as_an_agent_sends_child_stop() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({"identity":"agent001","state":"offline"}));
    let token = "a".repeat(64);
    let out = home
        .run_env(
            home.path(),
            &["workers", "stop", "agent001"],
            &[
                ("AGMEN_IDENTITY", "parent01"),
                ("AGMEN_LOCAL_TOKEN", &token),
            ],
        )
        .await;
    assert_exit(&out, 0);
    assert_eq!(stdout(&out), "offline agent001\n");
    assert!(!stdout(&out).contains(&token));
    assert!(!stderr(&out).contains(&token));
    let mut request = fake.request("child_stop");
    assert_eq!(request.as_.unwrap().as_str(), "parent01");
    assert!(
        request
            .params
            .as_object_mut()
            .unwrap()
            .remove("local_token")
            .is_some_and(|value| value == token)
    );
    assert_eq!(request.params, json!({"identity":"agent001"}));
}

#[tokio::test]
async fn stop_keep_children_sends_subtree_false() {
    let token = "a".repeat(64);
    for env in [
        vec![],
        vec![
            ("AGMEN_IDENTITY", "parent01"),
            ("AGMEN_LOCAL_TOKEN", token.as_str()),
        ],
    ] {
        let home = Home::new_in(std::path::Path::new("/tmp"));
        let fake = Fake::start(&home, json!({"identity":"agent001","state":"offline"}));
        let out = home
            .run_env(
                home.path(),
                &["workers", "stop", "agent001", "--keep-children"],
                &env,
            )
            .await;
        assert_exit(&out, 0);
        assert_eq!(stdout(&out), "offline agent001\n");
        assert!(!stdout(&out).contains(&token));
        assert!(!stderr(&out).contains(&token));
        let mut request = fake.request(if env.is_empty() {
            "agent_stop"
        } else {
            "child_stop"
        });
        if env.is_empty() {
            assert!(request.as_.is_none());
        } else {
            assert_eq!(request.as_.unwrap().as_str(), "parent01");
            assert!(
                request
                    .params
                    .as_object_mut()
                    .unwrap()
                    .remove("local_token")
                    .is_some_and(|value| value == token)
            );
        }
        assert_eq!(
            request.params,
            json!({"identity":"agent001","subtree":false})
        );
    }
}

#[tokio::test]
async fn launch_as_an_agent_forwards_optional_model_and_budget() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(
        &home,
        json!({"identity":"agent001","worktree":"/fake/worker","branch":"agent/worker"}),
    );
    let token = "a".repeat(64);
    let out = home
        .run_env(
            home.path(),
            &[
                "launch", "--name", "k1", "--role", "reviewer", "--model", "sonnet", "--budget",
                "2.5",
            ],
            &[
                ("AGMEN_IDENTITY", "parent01"),
                ("AGMEN_LOCAL_TOKEN", &token),
            ],
        )
        .await;
    assert_exit(&out, 0);
    assert!(!stdout(&out).contains(&token));
    assert!(!stderr(&out).contains(&token));
    let mut request = fake.request("child_launch");
    assert!(
        request
            .params
            .as_object_mut()
            .unwrap()
            .remove("local_token")
            .is_some_and(|value| value == token)
    );
    assert_eq!(
        request.params,
        json!({"name":"k1","role":"reviewer","model":"sonnet","budget_usd":2.5})
    );
}

#[tokio::test]
async fn launch_with_invalid_credentials_is_a_usage_error() {
    let token = "a".repeat(64);
    for (identity, credential, message) in [
        (
            "INVALID!",
            token.as_str(),
            "AGMEN_IDENTITY must be a valid identity id\n",
        ),
        (
            "parent01",
            "INVALID!",
            "AGMEN_LOCAL_TOKEN must be 64 lowercase hex digits\n",
        ),
        (
            "parent01",
            "",
            "AGMEN_LOCAL_TOKEN must be 64 lowercase hex digits\n",
        ),
    ] {
        let home = Home::new_in(std::path::Path::new("/tmp"));
        let wt = joined(&home);
        let fake = Fake::start(
            &home,
            json!({"identity":"agent001","worktree":"/fake/worker","branch":"agent/worker"}),
        );
        let out = home
            .run_env(
                &wt.path(),
                &["launch", "--name", "k1"],
                &[
                    ("AGMEN_IDENTITY", identity),
                    ("AGMEN_LOCAL_TOKEN", credential),
                ],
            )
            .await;
        assert_exit(&out, 2);
        assert_eq!(stderr(&out), message);
        assert!(!stdout(&out).contains(&token));
        assert!(!stderr(&out).contains(&token));
        assert!(!stderr(&out).contains("INVALID!"));
        assert!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.method == "hello")
        );
    }
}

#[tokio::test]
async fn stop_with_one_variable_is_a_usage_error() {
    let token = "a".repeat(64);
    for env in [
        vec![("AGMEN_IDENTITY", "parent01")],
        vec![("AGMEN_LOCAL_TOKEN", token.as_str())],
    ] {
        let home = Home::new_in(std::path::Path::new("/tmp"));
        let fake = Fake::start(&home, json!({"identity":"agent001","state":"offline"}));
        let out = home
            .run_env(home.path(), &["workers", "stop", "agent001"], &env)
            .await;
        assert_exit(&out, 2);
        assert_eq!(
            stderr(&out),
            "AGMEN_IDENTITY and AGMEN_LOCAL_TOKEN must be set together\n"
        );
        assert!(!stdout(&out).contains(&token));
        assert!(!stderr(&out).contains(&token));
        assert!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.method == "hello")
        );
    }
}

#[tokio::test]
async fn workers_lists_agents() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let wt = joined(&home);
    let answer = json!({"agents":[{"state":"idle","handle":"owner/worker","identity":"agent001","branch":"agent/worker","spent_usd":"0.250000","budget_usd":"2","role":"reviewer"}]});
    let fake = Fake::start(&home, answer.clone());
    let out = home.run(&wt.path(), &["workers"]).await;
    assert_exit(&out, 0);
    assert_eq!(
        stdout(&out),
        "idle owner/worker agent001 agent/worker 0.250000/2 reviewer\n"
    );
    assert_eq!(fake.request("agent_list").params, json!({}));
    let json_home = Home::new_in(std::path::Path::new("/tmp"));
    let _fake = Fake::start(&json_home, answer.clone());
    let out = json_home
        .run(
            json_home.path(),
            &["workers", "--project", "project2", "--json"],
        )
        .await;
    assert_exit(&out, 0);
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        answer
    );
}

#[tokio::test]
async fn workers_stop_sends_agent_stop() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({"identity":"agent001","state":"offline"}));
    let out = home
        .run(home.path(), &["workers", "stop", "agent001"])
        .await;
    assert_exit(&out, 0);
    assert_eq!(
        fake.request("agent_stop").params,
        json!({"identity":"agent001"})
    );
}

#[tokio::test]
async fn worktree_prune_prints_removed_and_kept() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let wt = joined(&home);
    let fake = Fake::start(
        &home,
        json!({"removed":["agent/done"],"kept":[{"branch":"agent/dirty","reason":"uncommitted"}]}),
    );
    let out = home
        .run(
            &wt.path(),
            &[
                "worktree",
                "prune",
                "--base",
                "main",
                "--confirm",
                "agent/done",
            ],
        )
        .await;
    assert_exit(&out, 0);
    assert_eq!(
        fake.request("prune").params,
        json!({"project":"project1","base":"main","confirm":"agent/done"})
    );
    assert_eq!(
        fake.request("console_snapshot").params,
        json!({"project":"project1","root":wt.path()})
    );
    assert_eq!(
        stdout(&out),
        "removed agent/done\nkept agent/dirty: uncommitted\n"
    );
}

#[tokio::test]
async fn worktree_prune_prints_the_unlisted_line() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let wt = joined(&home);
    let fake = Fake::start(
        &home,
        json!({
            "removed": ["agent/done"],
            "kept": [{"branch": "agent/later", "reason": "over the per-call limit"}],
            "unlisted": 3
        }),
    );
    let out = home.run(&wt.path(), &["worktree", "prune"]).await;
    assert_exit(&out, 0);
    assert_eq!(fake.request("prune").params, json!({"project":"project1"}));
    assert_eq!(
        stdout(&out),
        "removed agent/done\nkept agent/later: over the per-call limit\n3 more agent worktrees over the per-call limit; prune again after these are handled\n"
    );
}

#[tokio::test]
async fn worktree_prune_prints_at_most_512_kept_lines() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let wt = joined(&home);
    let kept: Vec<Value> = (0..600)
        .map(|index| json!({"branch": format!("agent/k{index:03}"), "reason": "uncommitted"}))
        .collect();
    let _fake = Fake::start(&home, json!({"removed": [], "kept": kept}));
    let out = home.run(&wt.path(), &["worktree", "prune"]).await;
    assert_exit(&out, 0);
    let text = stdout(&out);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 512);
    assert_eq!(lines[0], "kept agent/k000: uncommitted");
    assert_eq!(lines[511], "kept agent/k511: uncommitted");
}

#[tokio::test]
async fn budget_add_sends_agent_budget() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(
        &home,
        json!({"identity":"agent001","budget_usd":"4.500000"}),
    );
    let out = home
        .run(home.path(), &["budget", "agent001", "--add", "2.5"])
        .await;
    assert_exit(&out, 0);
    assert_eq!(
        fake.request("agent_budget").params,
        json!({"identity":"agent001","add_usd":2.5})
    );
    assert!(stdout(&out).contains("4.500000"));
}

#[tokio::test]
async fn agmend_not_running_exits_3() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let out = home
        .run(home.path(), &["workers", "--project", "project1"])
        .await;
    assert_exit(&out, 3);
    assert_eq!(
        stderr(&out),
        "agmend is not running: open the agmen app or run agmend service install\n"
    );
}

#[tokio::test]
async fn agmend_refusals_are_sanitized_and_exit_1() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let _fake = Fake::reply(&home, |request| {
        Response::err(
            request.id,
            ErrorCode::Forbidden,
            "owner\u{1b}\n\u{202e} only",
        )
    });
    let out = home
        .run(home.path(), &["budget", "agent001", "--add", "1"])
        .await;
    assert_exit(&out, 1);
    assert!(stderr(&out).contains("owner only"));
    assert!(!stderr(&out).contains(['\u{1b}', '\u{202e}']));
}

#[tokio::test]
async fn default_socket_check_refuses_open_modes_before_hello() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({"agents":[]}));
    std::fs::set_permissions(
        home.state_base().join(agmen_client::AGMEND_SOCKET),
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    let out = home
        .run(home.path(), &["workers", "--project", "project1"])
        .await;
    assert_exit(&out, 1);
    assert!(stderr(&out).contains("0666"));
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn workers_handles_empty_agents() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let _fake = Fake::start(&home, json!({"agents":[]}));
    let out = home
        .run(home.path(), &["workers", "--project", "project1"])
        .await;
    assert_exit(&out, 0);
    assert_eq!(stdout(&out), "");
}

#[tokio::test]
async fn workers_without_project_lists_all_agents_outside_checkout() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let fake = Fake::start(&home, json!({"agents":[]}));
    let out = home.run(home.path(), &["workers"]).await;
    assert_exit(&out, 0);
    assert_eq!(fake.request("agent_list").params, json!({}));
}

#[tokio::test]
async fn worktree_prune_continues_after_registration_hub_failure() {
    let home = Home::new_in(std::path::Path::new("/tmp"));
    let wt = joined(&home);
    let fake = Fake::reply(&home, |request| {
        if request.method == "console_snapshot" {
            Response::err(request.id, ErrorCode::Hub, "hub is offline")
        } else {
            Response::ok(request.id, json!({"removed":[],"kept":[]}))
        }
    });
    let out = home.run(&wt.path(), &["worktree", "prune"]).await;
    assert_exit(&out, 0);
    assert_eq!(fake.request("prune").params, json!({"project":"project1"}));
}
