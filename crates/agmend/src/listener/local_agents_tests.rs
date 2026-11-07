//! Listener sessions and delivery on the local origin.

#![cfg(test)]

use super::super::Listeners;
use super::{FAKE, complete_batch_lines, git};

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::Agmend;
use agmen_client::{IdentityFile, read_local_credential};
use agmen_local::{Request, Response};
use agmen_proto::{ClientEnvelope, IdentityId, Payload, ProjectId, Timestamp};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);
const POLLS: usize = 250;
const THREAD_CAP: usize = 3;
const THREAD_CAPACITY: usize = 256;
const POST_INTERVAL: Duration = Duration::from_millis(110);

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn socket(&self) -> PathBuf {
        self.dir.path().join("agmend.sock")
    }

    fn tokens(&self) -> agmen_client::TokenStore {
        agmen_client::TokenStore::Dir(self.dir.path().join("tokens"))
    }

    fn config(&self) -> crate::Config {
        crate::Config::new(
            self.socket(),
            self.dir.path().join("login.json"),
            self.tokens(),
        )
        .personal_roles(self.dir.path().join("roles"))
    }

    async fn signed_in(hub_url: &str) -> Self {
        let home = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        let login = agmen_client::Login {
            hub_url: hub_url.parse().unwrap(),
            human_id: "owner001".parse().unwrap(),
            handle: "owner".into(),
        };
        std::fs::write(
            home.dir.path().join("login.json"),
            serde_json::to_vec(&login).unwrap(),
        )
        .unwrap();
        agmen_client::store_tokens(
            &home.tokens(),
            &login,
            &agmen_client::HumanTokens::new("access".into(), "refresh".into(), 3600),
        )
        .await
        .unwrap();
        home
    }
}

struct Client {
    stream: tokio::net::UnixStream,
}

impl Client {
    async fn connect(socket: &Path) -> Self {
        Self {
            stream: tokio::net::UnixStream::connect(socket).await.unwrap(),
        }
    }

    async fn send(&mut self, request: &Request) {
        agmen_local::frame::write_async(&mut self.stream, request)
            .await
            .unwrap();
    }

    async fn read(&mut self) -> Response {
        agmen_local::frame::read_async(&mut self.stream)
            .await
            .unwrap()
    }

    async fn call(&mut self, method: &str, raw: Value) -> Response {
        tokio::time::timeout(WAIT, async {
            self.send(&Request::new(1, method, raw)).await;
            self.read().await
        })
        .await
        .unwrap()
    }
}

fn result(response: Response) -> Value {
    match response.outcome {
        agmen_local::Outcome::Ok { result } => result,
        agmen_local::Outcome::Err { error } => panic!("{error:?}"),
    }
}

fn error(response: Response) -> agmen_local::ApiError {
    match response.outcome {
        agmen_local::Outcome::Err { error } => error,
        agmen_local::Outcome::Ok { .. } => panic!("expected a refusal"),
    }
}

struct Fixture {
    home: Home,
    root: PathBuf,
    client: Client,
    project: Value,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let home = Home {
            dir: tempfile::Builder::new().prefix("ll").tempdir().unwrap(),
        };
        Self::with_home(home).await
    }

    async fn with_home(home: Home) -> Self {
        let root = home.dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        std::fs::write(root.join("readme"), "listener fixture\n").unwrap();
        git(&root, &["add", "readme"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "fixture",
            ],
        );
        let (mut client, stop, task) = Self::serve(&home).await;
        let project = result(
            client
                .call("local_project_create", json!({"name":"listeners"}))
                .await,
        );
        Self {
            home,
            root,
            client,
            project,
            stop,
            task,
        }
    }

    async fn serve(
        home: &Home,
    ) -> (
        Client,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        Self::serve_daemon(home, Self::daemon(home)).await
    }

    /// A bound daemon whose listeners run the fake program. It is not serving yet.
    fn daemon(home: &Home) -> Agmend {
        let program = home.dir.path().join("fake-listener");
        std::fs::write(&program, format!("#!/bin/sh\nset -eu\n{FAKE}")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut daemon = Agmend::bind(home.config()).unwrap();
        let core = Arc::get_mut(&mut daemon.core).unwrap();
        let mut listeners = Listeners::new(
            core.origins.clone(),
            home.dir.path().to_owned(),
            home.dir.path().join("settings.toml"),
        );
        listeners.program = program;
        listeners.helper = Some("/fake/agmen".into());
        listeners.environment = Some(vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("AGMEN_MANAGED".into(), "1".into()),
        ]);
        listeners.tick = POLL;
        core.listeners = Arc::new(listeners);
        daemon
    }

    async fn serve_daemon(
        home: &Home,
        daemon: Agmend,
    ) -> (
        Client,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(daemon.serve_until(async {
            stopped.await.unwrap();
        }));
        let mut client = Client::connect(&home.socket()).await;
        result(client.call("hello", json!({"api":1})).await);
        (client, stop, task)
    }

    async fn start(&mut self) -> Value {
        result(
            self.client
                .call(
                    "listener_start",
                    json!({"project":self.project["project"],"root":self.root}),
                )
                .await,
        )
    }

    async fn call_as(&mut self, id: &IdentityId, method: &str, mut raw: Value) -> Response {
        raw["local_token"] = json!(
            read_local_credential(&self.home.tokens(), id)
                .unwrap()
                .unwrap()
                .local_token
        );
        let mut request = Request::new(900, method, raw);
        request.as_ = Some(id.clone());
        tokio::time::timeout(WAIT, async {
            self.client.send(&request).await;
            self.client.read().await
        })
        .await
        .unwrap()
    }

    async fn post(&mut self, id: &IdentityId, sent: &ClientEnvelope) -> Value {
        result(self.call_as(id, "post", json!({"envelope":sent})).await)
    }

    async fn stop(self) {
        self.stop.send(()).unwrap();
        tokio::time::timeout(WAIT, self.task)
            .await
            .unwrap()
            .unwrap();
    }

    async fn batches(&self) -> Vec<Value> {
        complete_batch_lines(
            &tokio::fs::read_to_string(self.root.join("batches"))
                .await
                .unwrap_or_default(),
        )
    }

    async fn wait_batches(&self) -> Vec<Value> {
        tokio::time::timeout(WAIT, async {
            for _ in 0..POLLS {
                let batches = self.batches().await;
                if !batches.is_empty() {
                    return batches;
                }
                tokio::time::sleep(POLL).await;
            }
            panic!("local listener did not receive a batch");
        })
        .await
        .unwrap()
    }
}

fn envelope(body: &str) -> ClientEnvelope {
    let now = Timestamp::now();
    ClientEnvelope {
        v: 1,
        id: agmen_core::new_message_id(now).unwrap(),
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: body.try_into().unwrap(),
        valid_until: None,
        payload: Payload::Fyi {},
        sent_at: now,
    }
}

#[tokio::test]
async fn listener_starts_on_a_local_project() {
    let mut f = Fixture::new().await;
    let started = f.start().await;
    assert_eq!(
        started["handle"],
        format!(
            "{}/listener",
            f.project["person"]["handle"].as_str().unwrap()
        )
    );
    let id: IdentityId = serde_json::from_value(started["identity_id"].clone()).unwrap();
    let config: Value = serde_json::from_slice(
        &std::fs::read(
            f.home
                .dir
                .path()
                .join("listener")
                .join(f.project["project"].as_str().unwrap())
                .join("mcp.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let env = &config["mcpServers"]["agmen"]["env"];
    assert_eq!(env["AGMEN_IDENTITY"], started["identity_id"]);
    assert!(
        env["AGMEN_LOCAL_TOKEN"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    let roster = result(f.call_as(&id, "roster", json!({})).await);
    assert!(
        roster
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["identity_id"] == started["identity_id"] && row["kind"] == "listener")
    );
    tokio::time::sleep(POLL * 3).await;
    let status = result(
        f.client
            .call("listener_status", json!({"project":f.project["project"]}))
            .await,
    );
    assert_eq!(status[0]["state"], "running");
    f.stop().await;
}

#[tokio::test]
async fn listener_accept_from_applies_locally() {
    let mut f = Fixture::new().await;
    let started = f.start().await;
    let stranger: IdentityFile = serde_json::from_value(result(
        f.client
            .call(
                "local_mint",
                json!({"project":f.project["project"],"name":"stranger"}),
            )
            .await,
    ))
    .unwrap();
    let mut denied = envelope("stranger message");
    denied.payload = Payload::Dm {};
    denied
        .to
        .push(serde_json::from_value(started["identity_id"].clone()).unwrap());
    f.post(&stranger.identity_id, &denied).await;
    let mut accepted = envelope("person message");
    accepted.payload = Payload::Dm {};
    accepted.to = denied.to;
    result(
        f.client
            .call(
                "post",
                json!({"project":f.project["project"],"envelope":accepted}),
            )
            .await,
    );
    let batches = f.wait_batches().await;
    let batch = batches.iter().map(Value::to_string).collect::<String>();
    assert!(batch.contains("person message"));
    assert!(!batch.contains("stranger message"));
    f.stop().await;
}

#[tokio::test]
async fn listener_readopts_a_local_listener_after_agmend_stops() {
    let mut f = Fixture::new().await;
    let started = f.start().await;
    f.stop.send(()).unwrap();
    tokio::time::timeout(WAIT, f.task).await.unwrap().unwrap();
    let (client, stop, task) = Fixture::serve(&f.home).await;
    f.client = client;
    f.stop = stop;
    f.task = task;
    let status = tokio::time::timeout(WAIT, async {
        for _ in 0..POLLS {
            let status = result(
                f.client
                    .call("listener_status", json!({"project":f.project["project"]}))
                    .await,
            );
            if status[0]["state"] == "running" {
                return status;
            }
            tokio::time::sleep(POLL).await;
        }
        panic!("local listener was not re-adopted");
    })
    .await
    .unwrap();
    assert_eq!(status[0]["identity_id"], started["identity_id"]);
    result(
        f.client
            .call(
                "post",
                json!({"project":f.project["project"],"envelope":envelope("after adoption")}),
            )
            .await,
    );
    assert!(
        f.wait_batches().await[0]
            .to_string()
            .contains("after adoption")
    );
    f.stop().await;
}

#[tokio::test]
async fn local_listener_pending_after_agmend_reopens_returns_recorded_rows() {
    let mut f = Fixture::new().await;
    f.start().await;
    f.stop.send(()).unwrap();
    tokio::time::timeout(WAIT, f.task).await.unwrap().unwrap();
    // Recorded while agmend is closed, so no later commit wakes a collector for it.
    let daemon = Fixture::daemon(&f.home);
    let project: ProjectId = serde_json::from_value(f.project["project"].clone()).unwrap();
    daemon
        .core
        .origins
        .local()
        .post_as_person(&project, &envelope("recorded while closed"))
        .await
        .unwrap();
    let (client, stop, task) = Fixture::serve_daemon(&f.home, daemon).await;
    f.client = client;
    f.stop = stop;
    f.task = task;
    assert!(
        f.wait_batches().await[0]
            .to_string()
            .contains("recorded while closed")
    );
    f.stop().await;
}

#[tokio::test]
async fn second_local_listener_start_is_already_running() {
    let mut f = Fixture::new().await;
    f.start().await;
    let err = error(
        f.client
            .call(
                "listener_start",
                json!({"project":f.project["project"],"root":f.root}),
            )
            .await,
    );
    assert_eq!(err.message, "a listener already runs in this project");
    f.stop().await;
}

#[tokio::test]
async fn failed_local_listener_registration_frees_the_listener_slot() {
    let mut f = Fixture::new().await;
    let registry = rusqlite::Connection::open(f.home.dir.path().join("agmend.db")).unwrap();
    registry.busy_timeout(WAIT).unwrap();
    for n in 0..agmen_local::safeguards::MAX_MANAGED {
        let id = format!("seed{n:04}");
        let metadata = json!({"hub_url":"https://hub.example/","broker_url":"mqtt://127.0.0.1:9","tenant":"tenant01","project":"project1","identity_id":id,"owner_id":"owner001","handle":format!("owner/{id}"),"kind":"agent"});
        registry
            .execute(
                "INSERT INTO identities VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![
                    "https://hub.example",
                    id,
                    "owner001",
                    "project1",
                    metadata.to_string()
                ],
            )
            .unwrap();
    }
    let refused = error(
        f.client
            .call(
                "listener_start",
                json!({"project":f.project["project"],"root":f.root}),
            )
            .await,
    );
    assert_eq!(refused.code, agmen_local::ErrorCode::Hub);
    registry
        .execute(
            "DELETE FROM identities WHERE origin = 'https://hub.example'",
            [],
        )
        .unwrap();
    // The refused start must not leave a live listener in the record behind.
    f.start().await;
    f.stop().await;
}

#[tokio::test]
async fn local_listener_stop_then_start_keeps_the_identity() {
    let mut f = Fixture::new().await;
    let first = f.start().await;
    result(
        f.client
            .call("listener_stop", json!({"project":f.project["project"]}))
            .await,
    );
    let status = result(
        f.client
            .call("listener_status", json!({"project":f.project["project"]}))
            .await,
    );
    assert_eq!(status[0]["state"], "stopped");
    assert!(
        !f.home
            .dir
            .path()
            .join("listener")
            .join(f.project["project"].as_str().unwrap())
            .join("intent.json")
            .exists()
    );
    // A stopped listener fails the live-listener gate like one that never ran.
    let id: IdentityId = serde_json::from_value(first["identity_id"].clone()).unwrap();
    let refused = error(
        f.call_as(&id, "post", json!({"envelope":envelope("while stopped")}))
            .await,
    );
    assert_eq!(refused.code, agmen_local::ErrorCode::Forbidden);
    assert_eq!(f.start().await["identity_id"], first["identity_id"]);
    f.stop().await;
}

#[tokio::test]
async fn running_local_listener_post_is_accepted() {
    let mut f = Fixture::new().await;
    let id = serde_json::from_value(f.start().await["identity_id"].clone()).unwrap();
    let posted = f.post(&id, &envelope("listener post")).await;
    assert_eq!(posted["stream"], "feed");
    let page = result(f.call_as(&id, "page", json!({"stream":"feed"})).await);
    assert_eq!(page["messages"][0]["from"], json!(id));
    assert_eq!(page["messages"][0]["from_kind"], "listener");
    f.stop().await;
}

#[tokio::test]
async fn local_listener_post_over_the_thread_cap_is_refused() {
    let mut f = Fixture::new().await;
    let id = serde_json::from_value(f.start().await["identity_id"].clone()).unwrap();
    let parent = envelope("root");
    f.post(&id, &parent).await;
    for _ in 1..THREAD_CAP {
        let mut sent = envelope("reply");
        sent.reply_to = Some(parent.id.clone());
        assert!(f.post(&id, &sent).await.get("seq").is_some());
    }
    let mut refused = envelope("capped");
    refused.reply_to = Some(parent.id);
    assert_eq!(f.post(&id, &refused).await, json!({"dropped":"thread_cap"}));
    let page = result(f.call_as(&id, "page", json!({"stream":"feed"})).await);
    assert_eq!(page["messages"].as_array().unwrap().len(), THREAD_CAP);
    f.stop().await;
}

#[tokio::test]
async fn local_listener_post_over_capacity_is_refused() {
    let mut f = Fixture::new().await;
    let id = serde_json::from_value(f.start().await["identity_id"].clone()).unwrap();
    for _ in 0..THREAD_CAPACITY {
        assert!(
            f.post(&id, &envelope("independent thread"))
                .await
                .get("seq")
                .is_some()
        );
        // Stay below the core's ten posts per second to reach the thread cap.
        tokio::time::sleep(POST_INTERVAL).await;
    }
    assert_eq!(
        f.post(&id, &envelope("over capacity")).await,
        json!({"dropped":"capacity"})
    );
    let page = result(f.call_as(&id, "page", json!({"stream":"feed"})).await);
    assert_eq!(page["messages"].as_array().unwrap().len(), THREAD_CAPACITY);
    f.stop().await;
}

#[tokio::test]
async fn local_listener_collector_never_connects_to_the_hub() {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let home = Home::signed_in(&format!("http://{}", socket.local_addr().unwrap())).await;
    let mut f = Fixture::with_home(home).await;
    f.start().await;
    result(
        f.client
            .call(
                "post",
                json!({"project":f.project["project"],"envelope":envelope("local collection")}),
            )
            .await,
    );
    assert!(
        f.wait_batches().await[0]
            .to_string()
            .contains("local collection")
    );
    f.stop().await;
    assert!(tokio::time::timeout(POLL, socket.accept()).await.is_err());
}
