#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use agmen_local::{API, ErrorCode, Request, Response, frame};
use rmcp::model::CallToolRequestParams;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Mutex;
use std::time::Duration;

const LOCAL_CREDENTIAL: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Fake {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    requests: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Fake {
    fn new(answer: Value) -> Self {
        Self::reply(move |request| Response::ok(request.id, answer.clone()))
    }
    fn reply(reply: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agmend.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async {
                for _ in 0..8 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    for _ in 0..2 {
                        let request: Request = frame::read_async(&mut stream).await.unwrap();
                        let answer = if request.method == "hello" {
                            Response::ok(request.id, json!({"api":API}))
                        } else {
                            reply(&request)
                        };
                        recorded.lock().unwrap().push(request);
                        frame::write_async(&mut stream, &answer).await.unwrap();
                    }
                }
            })
            .await
            .expect("fake lifetime bounded by test");
        });
        Self {
            _dir: dir,
            socket,
            requests,
            task,
        }
    }
    fn server(&self) -> Server {
        Server::new(
            LaunchCredential {
                identity: "listen01".parse().unwrap(),
                token: LOCAL_CREDENTIAL.parse().unwrap(),
            },
            self.socket.clone(),
        )
    }
    async fn call(&self, name: &'static str, args: Value) -> String {
        let (server_io, client_io) = tokio::io::duplex(65536);
        let server = self.server();
        let task = tokio::spawn(async move {
            server
                .serve(server_io)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let client = ().serve(client_io).await.unwrap();
        let result = client
            .call_tool(
                CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(false));
        let text = result
            .content
            .iter()
            .map(|block| block.as_text().unwrap().text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        client.cancel().await.unwrap();
        task.await.unwrap();
        text
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn listener_tools_are_exactly_these() {
    let fake = Fake::new(json!({}));
    let mut names: Vec<_> = fake
        .server()
        .tool_router
        .list_all()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "answer",
            "dm",
            "git_diff",
            "git_log",
            "git_show",
            "git_status",
            "post",
            "roster",
            "settings_apply",
            "settings_get"
        ]
    );
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn token_is_not_sent_to_an_unchecked_socket() {
    for regular_file in [false, true] {
        let fake = Fake::new(json!({}));
        if regular_file {
            std::fs::remove_file(&fake.socket).unwrap();
            std::fs::write(&fake.socket, "not a socket").unwrap();
        } else {
            std::fs::set_permissions(&fake.socket, std::fs::Permissions::from_mode(0o666)).unwrap();
        }
        let answer = fake.call("git_status", json!({})).await;
        assert!(answer.starts_with("refused: "));
        assert!(answer.contains(if regular_file { "socket" } else { "0666" }));
        assert!(!answer.contains(LOCAL_CREDENTIAL));
        assert!(fake.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn git_tool_marks_truncation_and_cleans_text() {
    let fake = Fake::new(json!({"text":"a\u{1b}[31mb","truncated":true}));
    assert_eq!(
        fake.call("git_log", json!({})).await,
        "ab\n(truncated at 32 KiB)"
    );
}

#[tokio::test]
async fn post_reports_dropped_thread_cap() {
    let fake = Fake::new(json!({"dropped":"thread_cap"}));
    assert_eq!(
        fake.call("post", json!({"verb":"FYI","body":"hello"}))
            .await,
        "dropped: thread_cap"
    );
}

#[tokio::test]
async fn settings_apply_answers_applied_and_pending() {
    for (state, expected) in [
        ("applied", "applied; undo id change01"),
        ("pending", "waiting for your person to apply; id change01"),
    ] {
        let fake = Fake::new(json!({"state":state,"id":"change01"}));
        assert_eq!(fake.call("settings_apply",json!({"reply_to":format!("{:026}",1),"batch_id":1,"changes":[{"target":"listener","key":"max_workers","value":1}]})).await, expected);
    }
    let fake =
        Fake::reply(|request| Response::err(request.id, ErrorCode::Forbidden, "a\u{1b}[31mb"));
    assert_eq!(fake.call("settings_get", json!({})).await, "refused: ab");
}

#[tokio::test]
async fn listener_post_refuses_verbs_outside_its_role() {
    let fake = Fake::new(json!({"stream":"feed","seq":1}));
    let text = fake
        .call("post", json!({"verb":"CLAIM","body":"claim"}))
        .await;
    assert!(text.starts_with("refused: "));
    assert!(text.contains("CLAIM"));
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn listener_post_tools_build_envelopes_and_report_recorded_or_queued() {
    let reply_to = format!("{:026}", 1);
    for (tool, args, verb, to) in [
        (
            "answer",
            json!({"reply_to":reply_to,"body":"answer"}),
            "ANSWER",
            json!([]),
        ),
        (
            "dm",
            json!({"to":["person01"],"reply_to":reply_to,"body":"direct"}),
            "DM",
            json!(["person01"]),
        ),
        (
            "post",
            json!({"verb":"STATUS","body":"working"}),
            "STATUS",
            json!([]),
        ),
        (
            "post",
            json!({"verb":"ALERT","body":"blocked"}),
            "ALERT",
            json!([]),
        ),
    ] {
        let fake = Fake::new(json!({"stream":"feed","seq":7}));
        assert_eq!(fake.call(tool, args.clone()).await, "recorded #7 (feed)");
        let requests = fake.requests.lock().unwrap();
        let request = &requests[1];
        assert_eq!(request.method, "post");
        assert_eq!(request.as_.as_ref().unwrap().as_str(), "listen01");
        let envelope: agmen_proto::ClientEnvelope =
            serde_json::from_value(request.params["envelope"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&envelope.payload).unwrap()["verb"],
            verb
        );
        assert_eq!(serde_json::to_value(&envelope.to).unwrap(), to);
        assert_eq!(envelope.body.as_str(), args["body"]);
        assert_eq!(
            serde_json::to_value(&envelope.reply_to).unwrap(),
            args["reply_to"]
        );
    }
    let fake = Fake::new(json!({"queued":true,"id":reply_to}));
    assert_eq!(
        fake.call("post", json!({"verb":"ASK","body":"why?"})).await,
        format!("queued id={reply_to} (not yet recorded)")
    );
    let fake = Fake::new(json!({"dropped":"capacity"}));
    assert_eq!(
        fake.call("post", json!({"verb":"FYI","body":"hello"}))
            .await,
        "dropped: capacity"
    );
}

#[tokio::test]
async fn listener_tools_bound_inputs_and_clean_structured_outputs() {
    let fake = Fake::new(json!({}));
    for to in [vec![], vec!["person01"; 9]] {
        assert!(
            fake.call("dm", json!({"to":to,"body":"hello"}))
                .await
                .starts_with("refused: ")
        );
    }
    let changes = vec![json!({"target":"listener","key":"max_workers","value":1}); 17];
    assert!(
        fake.call(
            "settings_apply",
            json!({"reply_to":format!("{:026}",1),"batch_id":1,"changes":changes})
        )
        .await
        .starts_with("refused: ")
    );
    assert!(fake.requests.lock().unwrap().is_empty());
    let fake = Fake::new(
        json!([{"handle":"ow\u{1b}[31mner","kind":"human"},{"handle":"agent","kind":"agent","role":"reviewer"}]),
    );
    assert_eq!(
        fake.call("roster", json!({})).await,
        "owner human -\nagent agent reviewer"
    );
    let fake = Fake::new(json!({"model":"hai\u{1b}[31mku","nested":["a\u{7}b"]}));
    let text = fake.call("settings_get", json!({})).await;
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"model":"haiku","nested":["ab"]})
    );
}

#[tokio::test]
async fn listener_git_tools_forward_only_their_arguments() {
    for (name, args) in [
        ("git_diff", json!({"base":"main","head":"HEAD"})),
        ("git_log", json!({"rev_range":"main..HEAD","n":5})),
        ("git_show", json!({"rev":"HEAD","stat":true})),
        ("git_status", json!({})),
    ] {
        let fake = Fake::new(json!({"text":"first\nsecond","truncated":false}));
        assert_eq!(fake.call(name, args.clone()).await, "first\nsecond");
        let requests = fake.requests.lock().unwrap();
        let request = &requests[1];
        assert_eq!(request.method, name);
        let mut params = request.params.clone();
        params.as_object_mut().unwrap().remove("local_token");
        assert_eq!(params, args);
    }
}

#[tokio::test]
async fn git_tool_keeps_truncation_notice_after_unterminated_escape() {
    let fake = Fake::new(json!({"text":"a\u{1b}]unfinished","truncated":true}));
    assert_eq!(
        fake.call("git_log", json!({})).await,
        "a\n(truncated at 32 KiB)"
    );
    let fake =
        Fake::new(json!([{"handle":"owner\u{1b}]unfinished","kind":"human","role":"reviewer"}]));
    assert_eq!(fake.call("roster", json!({})).await, "owner human reviewer");
}
