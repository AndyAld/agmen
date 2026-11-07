//! Managed workers use the local record and credentials without a hub login.

#![cfg(test)]
#![cfg(feature = "test-hooks")]

mod support;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agmen_client::{IdentityFile, LocalCredential, read_local_credential};
use agmen_local::{ErrorCode, Request, Response};
use agmen_proto::{ClientEnvelope, Payload, Timestamp};
use agmend::Agmend;
use serde_json::{Value, json};
use support::{Client, Home, error, result};

const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
const POLLS: usize = 500;
const FAKE_CLAUDE: &str = r#"#!/bin/sh
printf '%s' "$2" >prompt-seen
i=0
while [ ! -f release ] && [ "$i" -lt 500 ]; do
	/bin/sleep 0.02
	i=$((i + 1))
done
printf '%s\n' '{"total_cost_usd":0,"is_error":false}'
"#;

struct Fixture {
    home: Home,
    client: Client,
    project: Value,
    repo: PathBuf,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let home = Home {
            dir: tempfile::Builder::new().prefix("lag").tempdir().unwrap(),
        };
        let mut f = Self::start(home).await;
        f.project = result(
            f.client
                .call("local_project_create", json!({"name":"workers"}))
                .await,
        )["project"]
            .clone();
        let identity: IdentityFile = serde_json::from_value(result(
            f.client
                .call("local_mint", json!({"project":f.project,"name":"checkout"}))
                .await,
        ))
        .unwrap();
        tokio::fs::create_dir_all(f.repo.join(".agmen"))
            .await
            .unwrap();
        tokio::fs::write(
            f.repo.join(".agmen/identity.json"),
            serde_json::to_vec(&identity).unwrap(),
        )
        .await
        .unwrap();
        git(&f.repo, &["init", "-b", "main"]).await;
        tokio::fs::write(
            f.repo.join(".gitignore"),
            ".agmen/\n.worktrees/\nprompt-seen\nrelease\n",
        )
        .await
        .unwrap();
        git(&f.repo, &["add", ".gitignore"]).await;
        git(
            &f.repo,
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
        )
        .await;
        git(&f.repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]).await;
        f
    }

    async fn start(home: Home) -> Self {
        let fake = home.dir.path().join("fake-claude");
        tokio::fs::write(&fake, FAKE_CLAUDE).await.unwrap();
        tokio::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700))
            .await
            .unwrap();
        let config = home
            .config()
            .test_programs(fake.clone(), fake, "/usr/bin:/bin".into());
        let daemon = Agmend::bind(config).unwrap();
        Self::serve(home, daemon).await
    }

    async fn serve(home: Home, daemon: Agmend) -> Self {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(daemon.serve_until(async {
            let _stopped = stopped.await;
        }));
        let mut client = Client::connect(&home.socket()).await;
        result(client.call("hello", json!({"api":1})).await);
        let repo = home.dir.path().join("repo");
        Self {
            home,
            client,
            project: Value::Null,
            repo,
            stop,
            task,
        }
    }

    async fn launch(&mut self, name: &str, role: Option<&str>) -> Value {
        result(
            self.client
                .call(
                    "agent_launch",
                    json!({"project":self.project,"root":self.repo,"name":name,"role":role}),
                )
                .await,
        )
    }

    async fn credential(&self, worker: &Value) -> LocalCredential {
        let tokens = self.home.tokens();
        let id = worker["identity"].as_str().unwrap().parse().unwrap();
        tokio::task::spawn_blocking(move || read_local_credential(&tokens, &id).unwrap().unwrap())
            .await
            .unwrap()
    }

    async fn call_as(
        &mut self,
        worker: &Value,
        method: &str,
        mut raw: Value,
        credential: &LocalCredential,
    ) -> Response {
        raw["local_token"] = json!(credential.local_token);
        let mut request = Request::new(900, method, raw);
        request.as_ = Some(worker["identity"].as_str().unwrap().parse().unwrap());
        tokio::time::timeout(WAIT, async {
            self.client.send(&request).await;
            self.client.read().await
        })
        .await
        .unwrap()
    }

    async fn dm(&mut self, worker: &Value, body: &str) {
        let sent = dm(worker, body);
        result(
            self.client
                .call("post", json!({"project":self.project,"envelope":sent}))
                .await,
        );
    }

    async fn stop(self) -> Home {
        self.stop.send(()).unwrap();
        tokio::time::timeout(WAIT, self.task)
            .await
            .unwrap()
            .unwrap();
        self.home
    }
}

async fn git(repo: &Path, args: &[&str]) {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn dm(worker: &Value, body: &str) -> ClientEnvelope {
    let now = Timestamp::now();
    ClientEnvelope {
        v: 1,
        id: agmen_core::new_message_id(now).unwrap(),
        reply_to: None,
        to: vec![worker["identity"].as_str().unwrap().parse().unwrap()],
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: body.try_into().unwrap(),
        valid_until: None,
        payload: Payload::Dm {},
        sent_at: now,
    }
}

async fn identity(worker: &Value) -> IdentityFile {
    serde_json::from_slice(
        &tokio::fs::read(
            Path::new(worker["worktree"].as_str().unwrap()).join(".agmen/identity.json"),
        )
        .await
        .unwrap(),
    )
    .unwrap()
}

async fn prompt(worker: &Value) -> String {
    let path = Path::new(worker["worktree"].as_str().unwrap()).join("prompt-seen");
    for _ in 0..POLLS {
        if let Ok(text) = tokio::fs::read_to_string(&path).await {
            return text;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("local worker received no batch");
}

#[tokio::test]
async fn launch_in_a_local_project_mints_locally() {
    let mut f = Fixture::new().await;
    let worker = f.launch("worker", Some("reviewer")).await;
    let who = identity(&worker).await;
    assert!(who.is_local());
    assert_eq!(who.role.unwrap().as_str(), "reviewer");
    assert_eq!(who.broker_url, agmen_client::local_broker());
    let dir = f
        .home
        .dir
        .path()
        .join("managed")
        .join(who.identity_id.as_str());
    let config: Value =
        serde_json::from_slice(&tokio::fs::read(dir.join("mcp.json")).await.unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"]["agmen"]["env"]["AGMEN_IDENTITY"],
        worker["identity"]
    );
    let credential = f.credential(&worker).await;
    assert!(
        config["mcpServers"]["agmen"]["env"]["AGMEN_LOCAL_TOKEN"]
            .as_str()
            .is_some_and(|token| token == credential.local_token)
    );
    assert_eq!(
        tokio::fs::metadata(dir.join("mcp.json"))
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let snapshot = result(
        f.client
            .call("console_snapshot", json!({"project":f.project}))
            .await,
    );
    assert!(
        snapshot["roster"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["identity_id"] == worker["identity"])
    );
    let listed = result(
        f.client
            .call("agent_list", json!({"project":f.project}))
            .await,
    );
    assert_eq!(listed["agents"][0]["identity"], worker["identity"]);
    f.stop().await;
}

#[tokio::test]
async fn owner_dm_reaches_a_local_worker_batch() {
    let mut f = Fixture::new().await;
    let worker = f.launch("worker", None).await;
    f.dm(&worker, "local owner instruction").await;
    assert!(prompt(&worker).await.contains("local owner instruction"));
    f.stop().await;
}

#[tokio::test]
async fn stop_revokes_a_local_worker() {
    let mut f = Fixture::new().await;
    let worker = f.launch("worker", None).await;
    let credential = f.credential(&worker).await;
    result(
        f.client
            .call("agent_stop", json!({"identity":worker["identity"]}))
            .await,
    );
    let snapshot = result(
        f.client
            .call("console_snapshot", json!({"project":f.project}))
            .await,
    );
    assert!(
        !snapshot["roster"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["identity_id"] == worker["identity"])
    );
    assert_eq!(
        error(
            f.call_as(&worker, "state", json!({"key":"head"}), &credential)
                .await
        )
        .code,
        ErrorCode::Unauthorized
    );
    f.stop().await;
}

#[tokio::test]
async fn child_launch_in_a_local_project_mints_locally() {
    let mut f = Fixture::new().await;
    let parent = f.launch("parent", Some("engineer")).await;
    let credential = f.credential(&parent).await;
    f.dm(&parent, "launch a reviewer").await;
    prompt(&parent).await;
    let child = result(
        f.call_as(
            &parent,
            "child_launch",
            json!({"name":"child","role":"reviewer"}),
            &credential,
        )
        .await,
    );
    assert!(identity(&child).await.is_local());
    assert_eq!(child["parent"], parent["identity"]);
    let path = f.home.dir.path().join("local.db");
    let id = child["identity"].as_str().unwrap().to_owned();
    let delegated: String = tokio::task::spawn_blocking(move || {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT delegated_by FROM identities WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(delegated, parent["identity"].as_str().unwrap());
    f.stop().await;
}

#[tokio::test]
async fn local_pending_after_agmend_reopens_returns_recorded_rows() {
    let mut f = Fixture::new().await;
    let who: IdentityFile = serde_json::from_value(result(
        f.client
            .call("local_mint", json!({"project":f.project,"name":"reader"}))
            .await,
    ))
    .unwrap();
    let worker = json!({"identity":who.identity_id});
    let credential = f.credential(&worker).await;
    let project = f.project.clone();
    let home = f.stop().await;
    let daemon = Agmend::bind(home.config()).unwrap();
    let local = daemon.test_local_origin();
    let sent = dm(&worker, "recorded before serving");
    local.post_as_person(&who.project, &sent).await.unwrap();
    let mut reopened = Fixture::serve(home, daemon).await;
    reopened.project = project;
    let pending = result(
        reopened
            .call_as(&worker, "pending", json!({}), &credential)
            .await,
    );
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["id"], json!(sent.id));
    assert!(
        result(
            reopened
                .call_as(&worker, "pending", json!({}), &credential)
                .await
        )
        .as_array()
        .unwrap()
        .is_empty()
    );
    reopened.stop().await;
}

#[tokio::test]
async fn local_worker_start_after_reopen_preserves_credential() {
    let mut f = Fixture::new().await;
    let worker = f.launch("worker", None).await;
    let credential = f.credential(&worker).await;
    let project = f.project.clone();
    let home = f.stop().await;
    let mut reopened = Fixture::start(home).await;
    reopened.project = project;
    result(
        reopened
            .client
            .call("agent_start", json!({"identity":worker["identity"]}))
            .await,
    );
    assert!(reopened.credential(&worker).await.local_token == credential.local_token);
    reopened.dm(&worker, "owner after reopen").await;
    assert!(prompt(&worker).await.contains("owner after reopen"));
    reopened.stop().await;
}

#[tokio::test]
async fn remove_revokes_a_local_worker() {
    let mut f = Fixture::new().await;
    let worker = f.launch("worker", None).await;
    let credential = f.credential(&worker).await;
    result(
        f.client
            .call("agent_remove", json!({"identity":worker["identity"]}))
            .await,
    );
    assert!(
        !Path::new(worker["worktree"].as_str().unwrap())
            .join(".agmen/identity.json")
            .exists()
    );
    assert_eq!(
        error(
            f.call_as(&worker, "state", json!({"key":"head"}), &credential)
                .await
        )
        .code,
        ErrorCode::Unauthorized
    );
    let listed = result(
        f.client
            .call("agent_list", json!({"project":f.project}))
            .await,
    );
    assert!(listed["agents"].as_array().unwrap().is_empty());
    f.stop().await;
}

#[tokio::test]
async fn local_worker_invalid_credential_refuses_start() {
    let mut f = Fixture::new().await;
    let worker = f.launch("worker", None).await;
    let project = f.project.clone();
    let home = f.stop().await;
    let tokens = home.tokens();
    let id = worker["identity"].as_str().unwrap().parse().unwrap();
    tokio::task::spawn_blocking(move || {
        let namespace = agmen_client::LOCAL_TOKEN_NAMESPACE.parse().unwrap();
        let key = agmen_client::TokenKey::new(&namespace, &id).unwrap();
        tokens
            .set(
                &key,
                r#"{"origin":"local","owner_id":"wrongowner","local_token":"invalid"}"#,
            )
            .unwrap();
    })
    .await
    .unwrap();
    let mut reopened = Fixture::start(home).await;
    reopened.project = project;
    let refusal = error(
        reopened
            .client
            .call("agent_start", json!({"identity":worker["identity"]}))
            .await,
    );
    assert_eq!(refusal.code, ErrorCode::Unauthorized);
    assert_eq!(refusal.message, "invalid local identity credential");
    reopened.stop().await;
}

/// Every file under `root`, depth first.
async fn files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            if entry.file_type().await.unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                found.push(entry.path());
            }
        }
    }
    found
}

#[tokio::test]
async fn local_worker_token_stays_out_of_the_worktree_and_answers() {
    let mut f = Fixture::new().await;
    let worker = f.launch("worker", None).await;
    let token = f.credential(&worker).await.local_token;
    let listed = result(
        f.client
            .call("agent_list", json!({"project":f.project}))
            .await,
    );
    assert!(!worker.to_string().contains(&token));
    assert!(!listed.to_string().contains(&token));
    let tree = files(Path::new(worker["worktree"].as_str().unwrap())).await;
    assert!(
        tree.iter()
            .any(|path| path.ends_with(".agmen/identity.json"))
    );
    for path in tree {
        let bytes = tokio::fs::read(&path).await.unwrap();
        assert!(
            !bytes
                .windows(token.len())
                .any(|window| window == token.as_bytes()),
            "{}",
            path.display()
        );
    }
    f.stop().await;
}
