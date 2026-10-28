#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use agmen_client::{HumanTokens, Login, OutboxState, TokenStore, store_tokens};
use axum::response::IntoResponse as _;
use serde_json::{Value, json};

use crate::managed::Managed;

#[derive(Debug, Default)]
struct FakeRunner {
    runs: Mutex<Vec<crate::launch::RunSpec>>,
    mode: AtomicUsize,
    cancelled: Arc<AtomicUsize>,
}

impl crate::launch::Runner for FakeRunner {
    fn run(
        &self,
        spec: crate::launch::RunSpec,
        stop: tokio::sync::oneshot::Receiver<()>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::launch::RunResult, crate::launch::RunError>,
                > + Send,
        >,
    > {
        self.runs.lock().unwrap().push(spec);
        let mode = self.mode.load(Ordering::SeqCst);
        let cancelled = self.cancelled.clone();
        Box::pin(async move {
            if mode == 1 {
                return Err(crate::launch::RunError::Failed {
                    total_cost_usd: "0.25".into(),
                });
            }
            if mode == 2 {
                let _stopped = stop.await;
                cancelled.fetch_add(1, Ordering::SeqCst);
                return Err(crate::launch::RunError::Cancelled);
            }
            Ok(crate::launch::RunResult {
                total_cost_usd: "1".into(),
            })
        })
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    daemon: crate::Agmend,
    runner: Arc<FakeRunner>,
    posts: Arc<Mutex<Vec<agmen_proto::ClientEnvelope>>>,
    post_delay: Arc<AtomicUsize>,
    hub: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.hub.abort();
    }
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.invalid"],
        ] {
            git(&repo, &args);
        }
        std::fs::write(repo.join(".gitignore"), ".agmen/\n.worktrees/\n").unwrap();
        git(&repo, &["add", ".gitignore"]);
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-m", "initial"],
        );
        let app = axum::Router::new().route("/v1/identities", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            assert_eq!(body["kind"], "agent");
            if body["handle"]=="owner/reject" {return (axum::http::StatusCode::CONFLICT,axum::Json(json!({"error":"handle_taken"}))).into_response();}
            let id=if body["handle"]=="owner/worker" {"agent001"} else {"agent002"};
            axum::Json(json!({"id":id,"handle":body["handle"],"tenant":"tenant01","project":"project1","token":"fake-agent-token"})).into_response()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub_url: url::Url = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let hub = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
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
        let runner = Arc::new(FakeRunner::default());
        let mut config = crate::Config::new(dir.path().join("agmend.sock"), login_file, tokens);
        config.runner = runner.clone();
        let mut daemon = crate::Agmend::bind(config).unwrap();
        let posts = Arc::new(Mutex::new(Vec::new()));
        let captured = posts.clone();
        let post_delay = Arc::new(AtomicUsize::new(0));
        let delay = post_delay.clone();
        Arc::get_mut(&mut Arc::get_mut(&mut daemon.core).unwrap().identities)
            .unwrap()
            .publisher = Some(Arc::new(move |_, envelope| {
            let wait = if matches!(envelope.payload, agmen_proto::Payload::Bye {}) {
                delay.load(Ordering::SeqCst)
            } else {
                0
            };
            captured.lock().unwrap().push(envelope);
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(wait as u64)).await;
                Ok(OutboxState::Pending { attempts: 0 })
            })
        }));
        daemon.core.managed.attach(&daemon.core.identities).await;
        Self {
            dir,
            daemon,
            runner,
            posts,
            post_delay,
            hub,
        }
    }

    async fn launch(&self) -> Value {
        self.daemon.core.managed.launch(json!({"project":"project1","name":"worker","root":self.dir.path().join("repo"),"budget_usd":2})).await.unwrap()
    }
}

#[tokio::test]
async fn mint_refusal_removes_the_unused_worktree_and_branch() {
    let fixture = Fixture::new().await;
    let repo = fixture.dir.path().join("repo");
    let inspect = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap()
            .stdout
    };
    let trees = inspect(&["worktree", "list", "--porcelain"]);
    let branches = inspect(&["branch", "--list"]);
    let refusal = fixture
        .daemon
        .core
        .managed
        .launch(json!({"project":"project1","name":"reject","root":repo}))
        .await
        .unwrap_err();
    assert!(refusal.message.contains("handle_taken"));
    assert_eq!(inspect(&["worktree", "list", "--porcelain"]), trees);
    assert_eq!(inspect(&["branch", "--list"]), branches);
}

#[tokio::test]
async fn shutdown_posts_goodbyes_concurrently() {
    let fixture = Fixture::new().await;
    fixture.launch().await;
    fixture
        .daemon
        .core
        .managed
        .launch(json!({"project":"project1","name":"other","root":fixture.dir.path().join("repo")}))
        .await
        .unwrap();
    fixture.post_delay.store(500, Ordering::SeqCst);
    tokio::time::timeout(
        std::time::Duration::from_millis(900),
        fixture.daemon.core.managed.shutdown(),
    )
    .await
    .expect("two goodbye posts share one deadline rather than running serially");
    assert_eq!(
        fixture
            .posts
            .lock()
            .unwrap()
            .iter()
            .filter(|post| matches!(post.payload, agmen_proto::Payload::Bye {}))
            .count(),
        2
    );
}

async fn enqueue(fixture: &Fixture, seq: u32, recipient: &str) {
    fixture.daemon.core.identities.enqueue_test(serde_json::from_value(json!({"v":1,"id":format!("01JBX6Q4T7W3C2M8Y9K5P{seq:05}"),"verb":"FYI","data":{},"body":"do the work","sent_at":"2026-10-25T16:04:11Z","stream":"feed","seq":seq,"from":"owner001","from_handle":"owner","from_kind":"human","mentioned":[recipient],"hub_at":"2026-10-25T16:04:12Z"})).unwrap()).await;
}

#[tokio::test]
async fn failed_run_is_idle_and_charges_reported_cost() {
    let fixture = Fixture::new().await;
    fixture.runner.mode.store(1, Ordering::SeqCst);
    fixture.launch().await;
    enqueue(&fixture, 1, "agent001").await;
    fixture.daemon.core.managed.tick().await;
    for _ in 0..100 {
        let listed = fixture.daemon.core.managed.list(json!({})).await.unwrap();
        if listed["agents"][0]["state"] == "idle" {
            assert_eq!(listed["agents"][0]["spent_usd"], "0.250000");
            fixture.daemon.core.managed.shutdown().await;
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    panic!("failed batch stayed running");
}

#[tokio::test]
async fn stopping_active_agent_cancels_batch_and_shutdown_stops_every_agent() {
    let fixture = Fixture::new().await;
    fixture.runner.mode.store(2, Ordering::SeqCst);
    fixture.launch().await;
    fixture
        .daemon
        .core
        .managed
        .launch(json!({"project":"project1","name":"other","root":fixture.dir.path().join("repo")}))
        .await
        .unwrap();
    enqueue(&fixture, 1, "agent001").await;
    enqueue(&fixture, 2, "agent002").await;
    fixture.daemon.core.managed.tick().await;
    for _ in 0..100 {
        if fixture.runner.runs.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    fixture
        .daemon
        .core
        .managed
        .stop(json!({"identity":"agent001"}))
        .await
        .unwrap();
    assert_eq!(fixture.runner.cancelled.load(Ordering::SeqCst), 1);
    fixture.daemon.core.managed.shutdown().await;
    assert_eq!(fixture.runner.cancelled.load(Ordering::SeqCst), 2);
    let listed = fixture.daemon.core.managed.list(json!({})).await.unwrap();
    assert!(
        listed["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|agent| agent["state"] == "offline")
    );
    assert_eq!(
        fixture
            .posts
            .lock()
            .unwrap()
            .iter()
            .filter(|post| matches!(post.payload, agmen_proto::Payload::Bye {}))
            .count(),
        2
    );
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn launch_creates_worktree_identity_and_managed_record() {
    let fixture = Fixture::new().await;
    let launched = fixture.launch().await;
    let path = std::path::PathBuf::from(launched["worktree"].as_str().unwrap());
    assert!(path.join(".git").is_file());
    let joined = agmen_client::find_identity(&path).unwrap().unwrap();
    assert_eq!(joined.identity.handle, "owner/worker");
    let listed = fixture.daemon.core.managed.list(json!({})).await.unwrap();
    assert_eq!(listed["agents"][0]["identity"], "agent001");
    assert_eq!(listed["agents"][0]["state"], "idle");
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn launch_preserves_tracked_project_role_directory() {
    let fixture = Fixture::new().await;
    let repo = fixture.dir.path().join("repo");
    std::fs::create_dir(repo.join(".agmen/roles")).unwrap();
    std::fs::write(repo.join(".agmen/roles/project.xml"), "<role/>").unwrap();
    git(&repo, &["add", "-f", ".agmen/roles/project.xml"]);
    git(
        &repo,
        &["-c", "commit.gpgsign=false", "commit", "-m", "project role"],
    );
    let launched = fixture.launch().await;
    let root = std::path::PathBuf::from(launched["worktree"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(root.join(".agmen/roles/project.xml")).unwrap(),
        "<role/>"
    );
    fixture.daemon.core.managed.shutdown().await;
}

#[tokio::test]
async fn stop_and_shutdown_post_goodbye_and_mark_offline() {
    for shutdown in [false, true] {
        let fixture = Fixture::new().await;
        fixture.launch().await;
        if shutdown {
            fixture.daemon.core.managed.shutdown().await;
        } else {
            fixture
                .daemon
                .core
                .managed
                .stop(json!({"identity":"agent001"}))
                .await
                .unwrap();
        }
        let listed = fixture.daemon.core.managed.list(json!({})).await.unwrap();
        assert_eq!(listed["agents"][0]["state"], "offline");
        assert!(
            fixture
                .posts
                .lock()
                .unwrap()
                .iter()
                .any(|post| matches!(post.payload, agmen_proto::Payload::Bye {}))
        );
    }
}

#[tokio::test]
async fn mention_runs_one_batch_then_resume_and_stops_at_budget() {
    let fixture = Fixture::new().await;
    fixture.launch().await;
    for seq in 1..=3 {
        for worker in fixture.daemon.core.managed.workers.lock().await.values() {
            worker.lock().await.last_run = None;
        }
        let message=serde_json::from_value(json!({"v":1,"id":format!("01JBX6Q4T7W3C2M8Y9K5P{seq:05}"),"verb":"FYI","data":{},"body":"do the work","sent_at":"2026-10-25T16:04:11Z","stream":"feed","seq":seq,"from":"owner001","from_handle":"owner","from_kind":"human","mentioned":["agent001"],"hub_at":"2026-10-25T16:04:12Z"})).unwrap();
        fixture.daemon.core.identities.enqueue_test(message).await;
        fixture.daemon.core.managed.tick().await;
        for _ in 0..100 {
            let worker = fixture
                .daemon
                .core
                .managed
                .workers
                .lock()
                .await
                .get(&"agent001".parse().unwrap())
                .unwrap()
                .clone();
            let worker = worker.lock().await;
            if fixture.runner.runs.lock().unwrap().len() >= seq.min(2) as usize
                && worker.task.as_ref().is_none_or(|task| task.is_finished())
            {
                break;
            }
            drop(worker);
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        fixture.daemon.core.managed.tick().await;
    }
    {
        let runs = fixture.runner.runs.lock().unwrap();
        assert_eq!(runs.len(), 2);
        assert!(!runs[0].resume);
        assert!(runs[1].resume);
        assert_eq!(runs[0].session_id, runs[1].session_id);
        assert!(runs[0].prompt.contains("do the work"));
    }
    fixture.daemon.core.managed.shutdown().await;
}

#[test]
fn managed_api_rejects_invalid_names_and_budget() {
    for raw in [
        json!({"project":"project1","name":"../escape"}),
        json!({"project":"project1","name":"ok","budget_usd":-1}),
        json!({"project":"project1","name":"ok","model":"--dangerously-skip-permissions"}),
        json!({"project":"project1","name":"ok","model":"son net"}),
    ] {
        assert!(Managed::validate_launch(raw).is_err());
    }
}
