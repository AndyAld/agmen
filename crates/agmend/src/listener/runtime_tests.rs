#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use agmen_client::{HumanTokens, Login, OutboxState, TokenStore, store_tokens};
use axum::response::IntoResponse as _;
use std::sync::Mutex as StdMutex;

const FAKE: &str = r#"
printf '%s\n' "$@" >argv
printf '%s\n' "$AGMEN_MANAGED" >managed-env
printf '%s\n' "$$" >pid
for _turn in 1 2 3 4 5 6 7 8; do
    IFS= read -r message || exit 0
    printf '%s\n' "$message" >>batches
    printf '{"type":"result","model_usage_since_last_result":{"haiku":{"costUSD":0.01}},"is_error":false}\n'
done
"#;

struct Fixture {
    dir: tempfile::TempDir,
    daemon: crate::Agmend,
    hub_task: tokio::task::JoinHandle<()>,
    mints: Arc<StdMutex<Vec<Value>>>,
    posts: Arc<StdMutex<Vec<ClientEnvelope>>>,
    conflict: Arc<std::sync::atomic::AtomicBool>,
    post_failure: Arc<std::sync::atomic::AtomicBool>,
    roster: Arc<StdMutex<Value>>,
    clock: Arc<StdMutex<jiff::Zoned>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.hub_task.abort();
    }
}

fn git(repo: &Path, args: &[&str]) {
    let result = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(result.status.success(), "temporary repo git failed");
}

impl Fixture {
    #[expect(
        clippy::too_many_lines,
        reason = "temporary hub, git repo and fake process form one isolated fixture"
    )]
    async fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("readme"), "listener test\n").unwrap();
        std::fs::write(repo.join(".gitignore"), ".agmen/\n").unwrap();
        git(&repo, &["add", "readme"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "fixture commit",
            ],
        );
        let program = dir.path().join("fake-claude");
        std::fs::write(&program, format!("#!/bin/sh\nset -eu\n{}\n", script.trim())).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mints = Arc::new(StdMutex::new(Vec::new()));
        let conflict = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let roster = Arc::new(StdMutex::new(
            json!([{"identity_id":"coord001","handle":"adam/coord","kind":"coordinator","status":"online"}]),
        ));
        let captured = mints.clone();
        let rejected = conflict.clone();
        let rosters = roster.clone();
        let posts = Arc::new(StdMutex::new(Vec::new()));
        let posted = posts.clone();
        let app = axum::Router::new()
            .route("/v1/me", axum::routing::get(||async { axum::Json(json!({"broker_url":"mqtt://127.0.0.1:9"})) }))
            .route("/v1/identities", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                captured.lock().unwrap().push(body.clone());
                let conflict = rejected.load(std::sync::atomic::Ordering::SeqCst);
                async move {
                    if conflict {
                        return (axum::http::StatusCode::CONFLICT, axum::Json(json!({"error":"listener_exists"}))).into_response();
                    }
                    axum::Json(json!({"id":"listen01","handle":body["handle"],"project":"project1","tenant":"tenant01","token":String::from("fake-hub-credential"),"verbs":["FYI","ALERT","DELEGATED"]})).into_response()
                }
            }))
            .route("/v1/projects/{project}/state", axum::routing::get(move || {
                let roster = rosters.lock().unwrap().clone();
                async move { axum::Json(json!({"value":roster})) }
            }))
            .route("/published", axum::routing::post(move |axum::Json(envelope): axum::Json<ClientEnvelope>| {
                posted.lock().unwrap().push(envelope);
                async { axum::Json(json!({})) }
            }));
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub_url: url::Url = format!("http://{}", socket.local_addr().unwrap())
            .parse()
            .unwrap();
        let hub_task = tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
        let login = Login {
            hub_url: hub_url.clone(),
            human_id: "owner001".parse().unwrap(),
            handle: "owner".into(),
        };
        let login_file = dir.path().join("login.json");
        std::fs::write(&login_file, serde_json::to_vec(&login).unwrap()).unwrap();
        let tokens = TokenStore::Dir(dir.path().join("tokens"));
        store_tokens(
            &tokens,
            &login,
            &HumanTokens::new("access".into(), "refresh".into(), 3600),
        )
        .await
        .unwrap();
        std::fs::create_dir(repo.join(".agmen")).unwrap();
        std::fs::write(repo.join(".agmen/identity.json"),serde_json::to_vec(&json!({"tenant":"tenant01","project":"project1","identity_id":"parent01","owner_id":"owner001","handle":"owner/parent","hub_url":hub_url,"broker_url":"mqtt://127.0.0.1:9"})).unwrap()).unwrap();
        let credentials = Arc::new(crate::login::Credentials::new(
            login_file.clone(),
            tokens.clone(),
            Hub::http().unwrap(),
        ));
        let config = crate::Config::new(dir.path().join("agmend.sock"), login_file, tokens)
            .personal_roles(dir.path().join("roles"));
        let mut daemon = crate::Agmend::bind(config).unwrap();
        let core = Arc::get_mut(&mut daemon.core).unwrap();
        // Replace before Listeners holds an Arc to the service.
        let mut identities = Identities::new(
            credentials,
            Arc::new(StdMutex::new(
                crate::state::Store::open(&dir.path().join("test.db")).unwrap(),
            )),
            dir.path().to_owned(),
            core.console.hub.clone(),
        );
        let post_url = hub_url.join("published").unwrap();
        let post_failure = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fails = post_failure.clone();
        identities.publisher = Some(Arc::new(move |_, envelope| {
            let post_url = post_url.clone();
            let fails = fails.clone();
            Box::pin(async move {
                if fails.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(ApiError {
                        code: ErrorCode::Hub,
                        message: "simulated outbox failure".into(),
                    });
                }
                reqwest::Client::new()
                    .post(post_url)
                    .json(&envelope)
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap();
                Ok(OutboxState::Pending { attempts: 0 })
            })
        }));
        core.identities = Arc::new(identities);
        let clock = Arc::new(StdMutex::new(
            "2026-10-29T12:00:00-06:00[America/Denver]"
                .parse::<jiff::Zoned>()
                .unwrap(),
        ));
        let mut listeners = Listeners::new(
            core.console.hub.clone(),
            dir.path().to_owned(),
            dir.path().join("settings.toml"),
        );
        listeners.program = program;
        listeners.helper = Some(PathBuf::from("/fake/agmen"));
        listeners.environment = Some(vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("AGMEN_MANAGED".into(), "1".into()),
        ]);
        listeners.tick = Duration::from_millis(10);
        listeners.time_scale = 3000;
        listeners.restart_delay = Duration::from_millis(10);
        let now = clock.clone();
        listeners.now = Arc::new(move || now.lock().unwrap().clone());
        core.listeners = Arc::new(listeners);
        core.listeners.attach(&core.identities, &core.managed).await;
        Self {
            dir,
            daemon,
            hub_task,
            mints,
            posts,
            conflict,
            post_failure,
            roster,
            clock,
        }
    }

    fn listeners(&self) -> &Arc<Listeners> {
        &self.daemon.core.listeners
    }
    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }
    fn config(&self) -> PathBuf {
        self.dir.path().join("listener/project1/mcp.json")
    }
    async fn human(&self, method: &str, raw: Value) -> Result<Value, ApiError> {
        self.listeners().human_call(method, raw).await
    }
    async fn start(&self) -> Value {
        self.human(
            "listener_start",
            json!({"project":"project1","root":self.repo()}),
        )
        .await
        .unwrap()
    }
    async fn identity(&self) -> ManagedIdentity {
        let id = "listen01".parse().unwrap();
        let secret = self
            .daemon
            .core
            .identities
            .local_token(&id)
            .await
            .unwrap()
            .unwrap();
        let caller = self
            .daemon
            .core
            .identities
            .authorize(Some(&id), Some(secret.expose()))
            .await
            .unwrap();
        match caller {
            crate::identities::Caller::Identity(identity) => identity,
            _ => panic!("expected listener identity"),
        }
    }
    async fn tool(&self, method: &str, raw: Value) -> Result<Value, ApiError> {
        self.listeners()
            .identity_call(&self.identity().await, method, raw)
            .await
    }
    async fn enqueue(&self, message: StampedEnvelope) {
        self.daemon.core.identities.enqueue_test(message).await;
    }
    async fn wait_batches(&self, n: usize) -> Vec<Value> {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let text = tokio::fs::read_to_string(self.repo().join("batches"))
                    .await
                    .unwrap_or_default();
                let lines: Vec<Value> = text
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                if lines.len() >= n {
                    return lines;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake claude must receive a batch")
    }
    async fn wait_state(&self, state: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let rows = self.human("listener_status", json!({})).await.unwrap();
                if rows[0]["state"] == state {
                    return rows[0].clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("listener must reach expected state")
    }
    async fn wait_spend(&self, amount: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            for _ in 0..300 {
                if self.human("listener_status", json!({})).await.unwrap()[0]["today_usd"] == amount
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("listener must account for the completed turn");
        })
        .await
        .unwrap();
    }
    async fn stop(&self) {
        self.human("listener_stop", json!({"project":"project1"}))
            .await
            .unwrap();
    }
    async fn request(&self, changes: Value) -> Result<Value, ApiError> {
        let message = event(1, "owner001");
        self.enqueue(message.clone()).await;
        let lines = self.wait_batches(1).await;
        let batch: Value =
            serde_json::from_str(lines[0]["message"]["content"].as_str().unwrap()).unwrap();
        self.tool(
            "settings_apply",
            json!({"reply_to":message.client.id,"batch_id":batch["batch_id"],"changes":changes}),
        )
        .await
    }
}

fn event(seq: u64, from: &str) -> StampedEnvelope {
    serde_json::from_value(json!({"v":1,"id":format!("{seq:026}"),"verb":"FYI","data":{},"body":format!("message {seq}"),"sent_at":"2026-10-29T16:00:00Z","stream":"feed","seq":seq,"from":from,"from_handle":if from=="owner001" {"owner"} else {"stranger"},"from_kind":"human","hub_at":"2026-10-29T16:00:00Z"})).unwrap()
}

#[tokio::test]
async fn listener_start_mints_registers_and_spawns() {
    let f = Fixture::new(FAKE).await;
    let result = f.start().await;
    assert_eq!(
        result,
        json!({"identity_id":"listen01","handle":"owner/listener"})
    );
    let body = f.mints.lock().unwrap()[0].clone();
    assert_eq!(body["kind"], "listener");
    assert_eq!(
        body["edits"],
        serde_json::to_value(agmen_proto::EditScope::None).unwrap()
    );
    assert_eq!(body["role"], "listener");
    let role = agmen_roles::Layers::default().load().unwrap();
    assert_eq!(body["role_sha256"], role.get("listener").unwrap().sha256);
    let id = f.identity().await;
    let secret = f
        .daemon
        .core
        .identities
        .local_token(&id.identity_id)
        .await
        .unwrap()
        .unwrap();
    let config: Value = serde_json::from_slice(&std::fs::read(f.config()).unwrap()).unwrap();
    assert!(
        config["mcpServers"]["agmen"]["env"]["AGMEN_LOCAL_TOKEN"].as_str() == Some(secret.expose())
    );
    assert_eq!(
        config["mcpServers"]["agmen"]["env"]["AGMEN_IDENTITY"],
        "listen01"
    );
    assert_eq!(
        config["mcpServers"]["agmen"]["args"],
        json!(["mcp", "--listener"])
    );
    f.enqueue(event(1, "owner001")).await;
    f.wait_batches(1).await;
    let argv = std::fs::read_to_string(f.repo().join("argv")).unwrap();
    let expected = ListenerArgs {
        program: PathBuf::new(),
        model: "claude-haiku-5-5".into(),
        mcp_config: f.config(),
        prompt_file: f.dir.path().join("agents/listen01/role-prompt.md"),
        max_budget_usd: Money::from_usd("2").unwrap(),
    }
    .argv();
    assert!(
        argv.lines()
            .eq(expected.iter().map(|s| s.to_str().unwrap()))
    );
    assert!(!argv.contains(secret.expose()));
    assert_eq!(
        std::fs::read_to_string(f.repo().join("managed-env")).unwrap(),
        "1\n"
    );
    f.stop().await;
}

#[tokio::test]
async fn listener_start_twice_says_already_running() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    let error = f
        .human(
            "listener_start",
            json!({"project":"project1","root":f.repo()}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.message, "a listener already runs in this project");
    assert_eq!(f.mints.lock().unwrap().len(), 1);
    f.stop().await;
    f.start().await;
    assert_eq!(f.mints.lock().unwrap().len(), 1);
    f.stop().await;
}

#[tokio::test]
async fn listener_exists_from_the_hub_says_already_running() {
    let f = Fixture::new(FAKE).await;
    f.conflict.store(true, std::sync::atomic::Ordering::SeqCst);
    let error = f
        .human(
            "listener_start",
            json!({"project":"project1","root":f.repo()}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.message, "a listener already runs in this project");
    assert!(!f.config().exists());
}

#[tokio::test]
async fn listener_start_refuses_coordinator() {
    let f = Fixture::new(FAKE).await;
    let error = f
        .human(
            "listener_start",
            json!({"project":"project1","root":f.repo(),"coordinator":true}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert_eq!(error.message, "coordinator listeners are not supported yet");
    assert!(f.mints.lock().unwrap().is_empty());
}

#[tokio::test]
async fn listener_start_refuses_a_root_outside_git() {
    let f = Fixture::new(FAKE).await;
    let error = f
        .human(
            "listener_start",
            json!({"project":"project1","root":f.dir.path()}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(f.mints.lock().unwrap().is_empty());
}

#[tokio::test]
async fn listener_start_requires_listener_kind() {
    let f = Fixture::new(FAKE).await;
    std::fs::create_dir(f.dir.path().join("roles")).unwrap();
    std::fs::write(f.dir.path().join("roles/listener.xml"),"<role name=\"listener\" extends=\"base\" kind=\"agent\" tier=\"small\" version=\"1\"><edits scope=\"none\" /></role>").unwrap();
    let error = f
        .human(
            "listener_start",
            json!({"project":"project1","root":f.repo()}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert_eq!(error.message, "the listener role must have kind listener");
    assert!(f.mints.lock().unwrap().is_empty());
}

#[tokio::test]
async fn queue_includes_persons_plain_chat() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    let live = f
        .listeners()
        .entries
        .lock()
        .await
        .get(&"project1".parse().unwrap())
        .unwrap()
        .live
        .clone();
    let held = live.lock().await;
    let msg = event(1, "owner001");
    f.enqueue(msg.clone()).await;
    f.enqueue(msg).await;
    let mut agent = event(2, "agent001");
    agent.from_kind = IdentityKind::Agent;
    f.enqueue(agent).await;
    let store = rusqlite::Connection::open(f.dir.path().join("test.db")).unwrap();
    let pending: u32 = store
        .query_row(
            "SELECT count(*) FROM pending WHERE identity='listen01'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 1);
    drop(store);
    drop(held);
    let lines = f.wait_batches(1).await;
    let batch: Value =
        serde_json::from_str(lines[0]["message"]["content"].as_str().unwrap()).unwrap();
    assert_eq!(batch["events"].as_array().unwrap().len(), 1);
    assert_eq!(batch["events"][0]["body"], "message 1");
    assert!(
        f.daemon
            .core
            .identities
            .pending(&f.identity().await)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        std::fs::read_to_string(f.repo().join("batches"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    f.stop().await;
}

#[tokio::test]
async fn listener_never_reads_the_broadcast_directly() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    let tx = f
        .daemon
        .core
        .identities
        .project_channel(&"project1".parse().unwrap())
        .await
        .unwrap();
    let _sent = tx.send((
        None,
        Ok(agmen_client::ClientEvent::Message {
            message: Box::new(event(1, "owner001")),
        }),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!f.repo().join("batches").exists());
    f.stop().await;
}

#[tokio::test]
async fn listener_thread_cap() {
    let bytes = Arc::new(StdMutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(TraceBytes(bytes.clone()))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let f = Fixture::new(FAKE).await;
    f.start().await;
    let root = event(1, "owner001");
    f.enqueue(root.clone()).await;
    f.wait_batches(1).await;
    let id = f.identity().await;
    for seq in 2..=5 {
        let mut envelope = event(seq, "owner001").client;
        envelope.reply_to = Some(root.client.id.clone());
        let result = f.listeners().post(&id, envelope).await.unwrap();
        if seq == 5 {
            assert_eq!(result, json!({"dropped":"thread_cap"}));
        }
    }
    assert_eq!(f.posts.lock().unwrap().len(), 3);
    let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(logs.contains(root.client.id.as_str()));
    assert!(logs.contains("ThreadCap"));
    f.stop().await;
}

#[tokio::test]
async fn git_read_tools_answer_only_the_listener() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    assert!(
        f.tool("git_log", json!({})).await.unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("fixture commit")
    );
    let mut other = f.identity().await;
    other.identity_id = "agent001".parse().unwrap();
    other.kind = IdentityKind::Agent;
    for method in [
        "git_log",
        "git_show",
        "git_diff",
        "git_status",
        "settings_get",
        "settings_apply",
        "roster",
    ] {
        assert_eq!(
            f.listeners()
                .identity_call(&other, method, json!({}))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Forbidden
        );
    }
    assert_eq!(
        f.tool("git_show", json!({"rev":"--output=x"}))
            .await
            .unwrap_err()
            .code,
        ErrorCode::BadRequest
    );
    f.stop().await;
}

#[tokio::test]
async fn settings_rejects_message_outside_batch() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    let error=f.tool("settings_apply",json!({"reply_to":event(9,"owner001").client.id,"batch_id":0,"changes":[{"target":"listener","key":"max_workers","value":1}]})).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleRequest);
    assert!(!f.dir.path().join("settings.toml").exists());
    f.stop().await;
}

#[tokio::test]
async fn settings_rejects_other_person() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    let mut msg = event(1, "stranger");
    msg.mentioned.push("listen01".parse().unwrap());
    f.human(
        "listener_set",
        json!({"project":"project1","accept_from":["+stranger"]}),
    )
    .await
    .unwrap();
    f.enqueue(msg.clone()).await;
    let lines = f.wait_batches(1).await;
    let batch: Value =
        serde_json::from_str(lines[0]["message"]["content"].as_str().unwrap()).unwrap();
    let before = std::fs::read(f.dir.path().join("settings.toml")).unwrap();
    let error=f.tool("settings_apply",json!({"reply_to":msg.client.id,"batch_id":batch["batch_id"],"changes":[{"target":"listener","key":"max_workers","value":1}]})).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(
        std::fs::read(f.dir.path().join("settings.toml")).unwrap(),
        before
    );
    f.stop().await;
}

#[tokio::test]
async fn narrowing_applies_with_undo() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    let result = f
        .request(json!([{"target":"listener","key":"max_workers","value":1}]))
        .await
        .unwrap();
    assert_eq!(result["state"], "applied");
    assert_eq!(
        f.tool("settings_get", json!({})).await.unwrap()["max_workers"],
        1
    );
    assert!(
        f.posts
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .body
            .as_str()
            .ends_with(&format!("undo:{}", result["id"].as_str().unwrap()))
    );
    assert!(
        f.posts
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .body
            .as_str()
            .contains("max_workers: 3 -> 1")
    );
    f.human(
        "settings_undo",
        json!({"project":"project1","id":result["id"]}),
    )
    .await
    .unwrap();
    assert_eq!(
        f.tool("settings_get", json!({})).await.unwrap()["max_workers"],
        3
    );
    f.stop().await;
}

#[tokio::test]
async fn widening_waits_for_apply() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    let result = f
        .request(json!([{"target":"listener","key":"listener_daily_usd","value":"3.00"}]))
        .await
        .unwrap();
    assert_eq!(result["state"], "pending");
    assert!(!f.dir.path().join("settings.toml").exists());
    assert!(
        f.posts
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .body
            .as_str()
            .ends_with(&format!("apply:{}", result["id"].as_str().unwrap()))
    );
    f.human(
        "settings_decide",
        json!({"project":"project1","id":result["id"],"apply":true}),
    )
    .await
    .unwrap();
    assert_eq!(
        f.tool("settings_get", json!({})).await.unwrap()["listener_daily_usd"],
        "3.00"
    );
    let project = "project1".parse().unwrap();
    assert_eq!(
        f.listeners()
            .changes
            .lock()
            .unwrap()
            .audit(&project)
            .count(),
        2
    );
    f.stop().await;
}

#[tokio::test]
async fn coordinator_cannot_be_removed() {
    let f = Fixture::new(FAKE).await;
    assert!(f.roster.lock().unwrap().as_array().unwrap().len() == 1);
    let error = f
        .human(
            "listener_set",
            json!({"project":"project1","accept_from":["-adam/coord"]}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.message, "coordinators always reach your listener");
}

#[tokio::test]
async fn daily_spend_resets_at_local_midnight() {
    let f = Fixture::new(FAKE).await;
    *f.clock.lock().unwrap() = "2026-10-31T23:59:00-06:00[America/Denver]".parse().unwrap();
    f.start().await;
    f.enqueue(event(1, "owner001")).await;
    f.wait_batches(1).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        f.human("listener_status", json!({})).await.unwrap()[0]["today_usd"],
        "0.01"
    );
    for (at, expected) in [
        ("2026-11-01T00:00:00-06:00[America/Denver]", "0.00"),
        ("2026-11-01T00:30:00-06:00[America/Denver]", "0.01"),
        ("2026-11-01T01:30:00-06:00[America/Denver]", "0.01"),
        ("2026-11-01T01:30:00-07:00[America/Denver]", "0.01"),
        ("2026-11-01T17:00:00-07:00[America/Denver]", "0.01"),
        ("2026-11-01T23:59:00-07:00[America/Denver]", "0.01"),
        ("2026-11-02T00:00:00-07:00[America/Denver]", "0.00"),
    ] {
        *f.clock.lock().unwrap() = at.parse().unwrap();
        if at == "2026-11-01T00:30:00-06:00[America/Denver]" {
            f.enqueue(event(2, "owner001")).await;
            f.wait_batches(2).await;
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert_eq!(
            f.human("listener_status", json!({})).await.unwrap()[0]["today_usd"],
            expected
        );
    }
    f.stop().await;
}

#[tokio::test]
async fn listener_stops_at_daily_cap() {
    let script = r#"
for cost in 1.50 0.60; do
    IFS= read -r message
    printf '%s\n' "$message" >>batches
    printf '{"type":"result","model_usage_since_last_result":{"haiku":{"costUSD":%s}}}\n' "$cost"
done
sleep 5
"#;
    let f = Fixture::new(script).await;
    f.start().await;
    f.enqueue(event(1, "owner001")).await;
    f.wait_batches(1).await;
    f.enqueue(event(2, "owner001")).await;
    f.wait_batches(2).await;
    let status = f.wait_state("capped").await;
    assert_eq!(status["today_usd"], "2.10");
    tokio::time::sleep(Duration::from_millis(50)).await;
    {
        let posts = f.posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].payload.verb(), Some(agmen_proto::Verb::Alert));
        assert_eq!(
            posts[0].body.as_str(),
            "listener stopped: daily cap 2.00 reached"
        );
    }
    assert!(!f.config().exists());
    f.stop().await;
}

#[tokio::test]
async fn listener_restarts_at_most_three_times_an_hour() {
    let f =
        Fixture::new("printf 'spawn\\n' >>spawns\nprintf 'early exit diagnostic\\n' >&2\nexit 1")
            .await;
    f.start().await;
    let status = f.wait_state("failed").await;
    assert_eq!(
        std::fs::read_to_string(f.repo().join("spawns"))
            .unwrap()
            .lines()
            .count(),
        4
    );
    assert!(
        status["error"]
            .as_str()
            .unwrap()
            .contains("early exit diagnostic")
    );
    f.stop().await;
}

#[tokio::test]
async fn listener_crash_mid_turn_restarts_without_redelivery() {
    // The first process dies after reading its batch; the restarted one lives.
    let script = format!(
        "if [ ! -f crashed ]; then\n    : >crashed\n    IFS= read -r message\n    printf '%s\\n' \"$message\" >>batches\n    exit 1\nfi\n{FAKE}"
    );
    let f = Fixture::new(&script).await;
    f.start().await;
    f.enqueue(event(1, "owner001")).await;
    f.wait_batches(1).await;
    // The crash must not end the listener: it restarts and takes the next message.
    f.enqueue(event(2, "owner001")).await;
    let lines = f.wait_batches(2).await;
    let bodies: Vec<String> = lines
        .iter()
        .map(|line| {
            let batch: Value =
                serde_json::from_str(line["message"]["content"].as_str().unwrap()).unwrap();
            batch["events"].as_array().unwrap().len().to_string()
                + batch["events"][0]["body"].as_str().unwrap()
        })
        .collect();
    assert_eq!(bodies, ["1message 1", "1message 2"]);
    assert_eq!(f.wait_state("running").await["state"], "running");
    f.stop().await;
}

#[tokio::test]
async fn listener_stop_removes_the_mcp_config() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    assert!(f.config().exists());
    f.stop().await;
    assert!(!f.config().exists());
    assert_eq!(
        f.human("listener_status", json!({})).await.unwrap()[0]["state"],
        "stopped"
    );
    f.identity().await;
}

#[tokio::test]
async fn mcp_config_is_private_from_creation() {
    let mut f = Fixture::new(FAKE).await;
    let observed = Arc::new(StdMutex::new(Vec::new()));
    let captured = observed.clone();
    let core = Arc::get_mut(&mut f.daemon.core).unwrap();
    let listeners = Arc::get_mut(&mut core.listeners).unwrap();
    listeners.created = Some(Arc::new(move |path| {
        captured.lock().unwrap().push((
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        ));
    }));
    listeners.program = f.dir.path().join("missing-claude");
    assert!(
        f.human(
            "listener_start",
            json!({"project":"project1","root":f.repo()})
        )
        .await
        .is_err()
    );
    assert_eq!(*observed.lock().unwrap(), [(0o700, 0o600)]);
    assert!(!f.config().exists());
}

#[derive(Clone)]
struct TraceBytes(Arc<StdMutex<Vec<u8>>>);
impl std::io::Write for TraceBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TraceBytes {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn local_token_is_never_logged() {
    use tracing::instrument::WithSubscriber as _;
    let bytes = Arc::new(StdMutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(TraceBytes(bytes.clone()))
        .finish();
    async {
        let f = Fixture::new(FAKE).await;
        f.start().await;
        let secret = f
            .daemon
            .core
            .identities
            .local_token(&"listen01".parse().unwrap())
            .await
            .unwrap()
            .unwrap();
        f.enqueue(event(1, "owner001")).await;
        f.wait_batches(1).await;
        f.stop().await;
        let logs = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(!logs.is_empty());
        assert!(!logs.contains(secret.expose()));
        assert!(!format!("{:?}", f.listeners()).contains(secret.expose()));
    }
    .with_subscriber(subscriber)
    .await;
}

#[tokio::test]
async fn listener_api_dispatch_and_peer_authorization() {
    use agmen_local::{Outcome, Request, Response, frame};
    let f = Fixture::new(FAKE).await;
    f.start().await;
    f.enqueue(event(1, "owner001")).await;
    f.wait_batches(1).await;
    let pid: i32 = std::fs::read_to_string(f.repo().join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let secret = f
        .daemon
        .core
        .identities
        .local_token(&"listen01".parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
    let task = tokio::spawn(crate::api::serve_with_peer(
        server,
        f.daemon.core.clone(),
        move |_| Ok(pid),
    ));
    for (id, method, as_identity, raw) in [
        (1, "hello", false, json!({"api":1})),
        (2, "listener_status", false, json!({})),
        (3, "git_log", true, json!({"local_token":secret.expose()})),
        (
            4,
            "listener_stop",
            true,
            json!({"project":"project1","local_token":secret.expose()}),
        ),
    ] {
        let mut request = Request::new(id, method, raw);
        if as_identity {
            request.as_ = Some("listen01".parse().unwrap());
        }
        frame::write_async(&mut client, &request).await.unwrap();
        let response: Response = frame::read_async(&mut client).await.unwrap();
        match (id, response.outcome) {
            (1, Outcome::Ok { .. }) => {}
            (3, Outcome::Ok { result }) => {
                assert!(result["text"].as_str().unwrap().contains("fixture commit"))
            }
            (2 | 4, Outcome::Err { error }) => assert_eq!(error.code, ErrorCode::Forbidden),
            _ => panic!("unexpected local API response"),
        }
    }
    drop(client);
    task.await.unwrap();
    f.stop().await;
}

#[tokio::test]
async fn listener_large_burst_keeps_overflow_for_next_batch() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    let live = f
        .listeners()
        .entries
        .lock()
        .await
        .get(&"project1".parse().unwrap())
        .unwrap()
        .live
        .clone();
    let held = live.lock().await;
    for seq in 1..=64 {
        let mut message = event(seq, "owner001");
        message.client.body = "x".repeat(1024).try_into().unwrap();
        f.enqueue(message).await;
    }
    drop(held);
    let lines = f.wait_batches(2).await;
    let batches: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line["message"]["content"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        batches
            .iter()
            .map(|batch| batch["events"].as_array().unwrap().len())
            .sum::<usize>(),
        64
    );
    assert_ne!(batches[0]["batch_id"], batches[1]["batch_id"]);
    for line in std::fs::read_to_string(f.repo().join("batches"))
        .unwrap()
        .lines()
    {
        assert!(line.len() < batch::MAX_BATCH_BYTES);
    }
    assert_eq!(
        f.human("listener_status", json!({})).await.unwrap()[0]["state"],
        "running"
    );
    f.stop().await;
}

#[tokio::test]
async fn listener_receives_persons_inbox() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    let mut message = event(1, "coord001");
    message.from_kind = IdentityKind::Coordinator;
    message.from_handle = "adam/coord".into();
    message.stream = agmen_proto::Stream::Inbox("owner001".parse().unwrap());
    message.client.payload = Payload::Dm {};
    message.client.to.push("owner001".parse().unwrap());
    f.enqueue(message).await;
    let lines = f.wait_batches(1).await;
    let batch: Value =
        serde_json::from_str(lines[0]["message"]["content"].as_str().unwrap()).unwrap();
    assert_eq!(batch["events"][0]["body"], "message 1");
    f.stop().await;
}

#[tokio::test]
async fn listener_cap_survives_notification_failure_and_cap_raise() {
    let f = Fixture::new(FAKE).await;
    f.human(
        "listener_set",
        json!({"project":"project1","listener_daily_usd":"0.00"}),
    )
    .await
    .unwrap();
    f.post_failure
        .store(true, std::sync::atomic::Ordering::SeqCst);
    f.start().await;
    // Let the failing notification complete, then the capped task must remain.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        f.human("listener_status", json!({})).await.unwrap()[0]["state"],
        "capped"
    );
    f.post_failure
        .store(false, std::sync::atomic::Ordering::SeqCst);
    f.human(
        "listener_set",
        json!({"project":"project1","listener_daily_usd":"1.00"}),
    )
    .await
    .unwrap();
    f.wait_state("running").await;
    f.enqueue(event(1, "owner001")).await;
    f.wait_batches(1).await;
    f.stop().await;
}

#[tokio::test]
async fn listener_start_accepts_a_git_root_without_join_metadata() {
    let f = Fixture::new(FAKE).await;
    std::fs::remove_file(f.repo().join(".agmen/identity.json")).unwrap();
    f.start().await;
    f.stop().await;
}

#[tokio::test]
async fn listener_applies_cap_even_when_change_card_post_fails() {
    let f = Fixture::new(FAKE).await;
    f.start().await;
    f.post_failure
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let result = f
        .request(json!([{"target":"listener","key":"listener_daily_usd","value":"0.00"}]))
        .await
        .unwrap();
    assert_eq!(result["state"], "applied");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        f.human("listener_status", json!({})).await.unwrap()[0]["state"],
        "capped"
    );
    f.stop().await;
}

#[tokio::test]
async fn listener_renews_process_budget_at_local_midnight() {
    let f = Fixture::new(&format!("printf 'spawn\\n' >>spawns\n{FAKE}")).await;
    *f.clock.lock().unwrap() = "2026-10-31T23:59:00-06:00[America/Denver]".parse().unwrap();
    f.start().await;
    f.enqueue(event(1, "owner001")).await;
    f.wait_batches(1).await;
    f.wait_spend("0.01").await;
    *f.clock.lock().unwrap() = "2026-11-01T00:00:00-06:00[America/Denver]".parse().unwrap();
    // Status may observe midnight first; the process still needs a new budget.
    f.human("listener_status", json!({})).await.unwrap();
    f.enqueue(event(2, "owner001")).await;
    f.wait_batches(2).await;
    assert_eq!(
        std::fs::read_to_string(f.repo().join("spawns"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(
        std::fs::read_to_string(f.repo().join("argv"))
            .unwrap()
            .contains("--max-budget-usd\n2.000000\n")
    );
    f.stop().await;
}

#[tokio::test]
async fn listener_updates_process_budget_after_cap_change() {
    let f = Fixture::new(&format!("printf 'spawn\\n' >>spawns\n{FAKE}")).await;
    f.start().await;
    f.enqueue(event(1, "owner001")).await;
    f.wait_batches(1).await;
    f.wait_spend("0.01").await;
    f.human(
        "listener_set",
        json!({"project":"project1","listener_daily_usd":"3.00"}),
    )
    .await
    .unwrap();
    f.enqueue(event(2, "owner001")).await;
    f.wait_batches(2).await;
    assert_eq!(
        std::fs::read_to_string(f.repo().join("spawns"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(
        std::fs::read_to_string(f.repo().join("argv"))
            .unwrap()
            .contains("--max-budget-usd\n2.990000\n")
    );
    f.stop().await;
}

#[tokio::test]
async fn listener_path_lookup_uses_managed_login_shell_flags() {
    const CHILD_DIR: &str = "AGMEN_TEST_LISTENER_SHELL_DIRECTORY";
    if let Some(dir) = std::env::var_os(CHILD_DIR) {
        let launch = resolve(Path::new("fake-claude"), Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(launch.program, PathBuf::from(dir).join("fake-claude"));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for (name, script) in [
        (
            "shell",
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >\"${0%/*}/flags\"\nprintf '\\n__AGMEN_LISTENER_PATH__\\n%s\\n' \"${0%/*}\"\n",
        ),
        ("fake-claude", "#!/bin/sh\nexit 1\n"),
        ("agmen", "#!/bin/sh\nexit 1\n"),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // A single child scopes SHELL without changing this test runner's environment.
    let child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "listener::runtime::tests::listener_path_lookup_uses_managed_login_shell_flags",
        ])
        .env(CHILD_DIR, dir.path())
        .env("SHELL", dir.path().join("shell"))
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(3), child)
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "isolated login PATH lookup failed");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("flags")).unwrap(),
        "-lic\n"
    );
}

#[tokio::test]
async fn listener_stops_when_login_changes_during_a_turn() {
    for change in ["logout", "owner", "hub"] {
        let f =
            Fixture::new("IFS= read -r message\nprintf '%s\\n' \"$message\" >>batches\nsleep 5")
                .await;
        f.start().await;
        f.enqueue(event(1, "owner001")).await;
        f.wait_batches(1).await;
        let path = f.dir.path().join("login.json");
        if change == "logout" {
            std::fs::remove_file(path).unwrap();
        } else {
            let mut login: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if change == "owner" {
                login["human_id"] = json!("other001");
            } else {
                login["hub_url"] = json!("http://127.0.0.1:1");
            }
            std::fs::write(path, serde_json::to_vec(&login).unwrap()).unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..200 {
                if !f.config().exists() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("listener MCP config must be removed after login changes");
        })
        .await
        .expect("listener must stop even while its turn is pending");
        let entries = f.listeners().entries.lock().await;
        assert_eq!(
            entries
                .get(&"project1".parse().unwrap())
                .unwrap()
                .live
                .lock()
                .await
                .state,
            "stopped"
        );
        drop(entries);
        f.listeners().shutdown().await;
    }
}
