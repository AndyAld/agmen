//! Managed workers use the local record and credentials without a hub login.

#![cfg(test)]
#![cfg(feature = "test-hooks")]

mod support;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use agmen_client::{IdentityFile, LocalCredential, read_local_credential};
use agmen_local::{ErrorCode, Request, Response};
use agmen_proto::{ClientEnvelope, Payload, Timestamp};
use agmend::Agmend;
use serde_json::{Value, json};
use support::{Client, Home, error, result};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
const POLLS: usize = 500;
const FAKE_CLAUDE: &str = r#"#!/bin/sh
printf '%s' "$2" >prompt-seen.tmp
mv prompt-seen.tmp prompt-seen
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
        Self::new_in(Home {
            dir: tempfile::Builder::new().prefix("lag").tempdir().unwrap(),
        })
        .await
    }

    async fn new_in(home: Home) -> Self {
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
async fn fake_claude_publishes_complete_prompt() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(dir.path().join("release"), "")
        .await
        .unwrap();
    // The shell opens the redirection before calling this function. Hold the
    // first printf there so the reader observes the create-before-write gap.
    let pause = r#"printf() {
	unset -f printf
	echo ready >&2
	IFS= read -r _
	command printf "$@"
}
"#;
    let expected = "complete worker prompt\nwith a second line";
    let mut child = tokio::process::Command::new("/bin/sh")
        .args([
            "-c",
            &format!("{pause}{FAKE_CLAUDE}"),
            "fake-claude",
            "-p",
            expected,
        ])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut ready = String::new();
    tokio::time::timeout(
        WAIT,
        tokio::io::BufReader::new(child.stderr.take().unwrap()).read_line(&mut ready),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ready, "ready\n");
    let before = tokio::fs::read_to_string(dir.path().join("prompt-seen")).await;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"write\n")
        .await
        .unwrap();
    let output = tokio::time::timeout(WAIT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    assert!(
        matches!(&before, Err(error) if error.kind() == std::io::ErrorKind::NotFound),
        "prompt became visible before printf wrote its contents: {before:?}"
    );
    assert_eq!(
        tokio::fs::read_to_string(dir.path().join("prompt-seen"))
            .await
            .unwrap(),
        expected
    );
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

/// A hub that only counts. Any request to it means agmend called a hub.
/// Local to this file because the same helper in `local_identities.rs` is not
/// shared and `tests/support` is not edited here.
struct CountingHub {
    url: String,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl CountingHub {
    async fn new() -> Self {
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let app = axum::Router::new().fallback(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            url,
            requests,
            task,
        }
    }
}

impl Drop for CountingHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A hub worker an earlier signed-in run saved. It loads offline, so it holds
/// no session and makes no hub call of its own.
async fn save_hub_worker(home: &Home, hub_url: &str) -> String {
    const ID: &str = "hubwork1";
    let dir = home.dir.path().join("managed").join(ID);
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let record = json!({
        "identity": {
            "hub_url": hub_url, "broker_url": "mqtt://127.0.0.1:9",
            "tenant": "tenant01", "project": support::PROJECT, "identity_id": ID,
            "owner_id": support::HUMAN, "handle": "andy/hub-worker", "kind": "agent",
        },
        "repo": home.dir.path().join("hub-repo"),
        "worktree": home.dir.path().join("hub-repo/.worktrees/hub-worker"),
        "branch": "agent/hub-worker-1",
        "owner_session_id": "11111111-1111-4111-8111-111111111111",
        "others_session_id": "22222222-2222-4222-8222-222222222222",
        "model": "sonnet",
        "budget_usd": "5",
    });
    tokio::fs::write(dir.join("agent.json"), serde_json::to_vec(&record).unwrap())
        .await
        .unwrap();
    ID.to_owned()
}

/// The identities of every agent `agent_list` shows, sorted. An error answer
/// fails the test.
async fn listed_ids(f: &mut Fixture) -> Vec<String> {
    let answer = result(f.client.call("agent_list", json!({})).await);
    let mut ids: Vec<String> = answer["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["identity"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn agent_list_signed_out_lists_local_workers_and_calls_no_hub() {
    let hub = CountingHub::new().await;
    let home = Home::signed_in(&hub.url).await;
    let hub_worker = save_hub_worker(&home, &hub.url).await;
    let mut f = Fixture::new_in(home).await;
    let local_worker = f.launch("worker", None).await["identity"]
        .as_str()
        .unwrap()
        .to_owned();
    // Signed in, both origins' workers are listed, so the next answer is not
    // short only because the hub worker never loaded.
    let mut both = vec![hub_worker, local_worker.clone()];
    both.sort();
    assert_eq!(listed_ids(&mut f).await, both);
    // `agmen logout` removes the login file.
    tokio::fs::remove_file(f.home.login_file()).await.unwrap();
    assert_eq!(listed_ids(&mut f).await, [local_worker]);
    assert_eq!(
        hub.requests.load(Ordering::SeqCst),
        0,
        "agent_list called a hub while signed out"
    );
    f.stop().await;
}
