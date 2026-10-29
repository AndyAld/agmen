//! What the end-to-end tests add to agmen-cli's test support (`support`):
//! the two "machines" are two temp homes with a worktree each, and here
//! they run `agmen watch --json` and `agmen mcp` as child processes, move
//! a machine to another broker address (Toxiproxy or the WSS listener),
//! read streams through the hub's replay API, and ask EMQX's REST API about
//! client sessions. The Toxiproxy link and the EMQX API are the shared dev
//! stack's (deploy/dev).

pub mod contract;
// Each test binary uses a different subset of these helpers, so `expect`
// cannot cover both crates.
#[allow(
    dead_code,
    reason = "each test binary uses only part of the daemon harness"
)]
pub mod daemon;
#[allow(
    dead_code,
    reason = "each test binary uses only part of the fake claude harness"
)]
pub mod fake_claude;

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agmen_client::{ClientEvent, HubClient, TokenKey};
use agmen_proto::{StampedEnvelope, Stream};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, BufReader, Lines};
use tokio::process::{Child, ChildStdout};
use url::Url;

use crate::support::mcp::text_of;
use crate::support::{self, Ctx, Ingest, Member, WAIT, env};

/// The variables that make an `agmen` process trust the dev stack's
/// self-signed CA (deploy/dev/init.sh): rustls-native-certs, which
/// agmen-client's TLS loads its roots through, reads `SSL_CERT_FILE` in
/// place of the system store.
pub fn trust_dev_ca() -> Vec<(String, String)> {
    vec![("SSL_CERT_FILE".to_owned(), env("AGMEN_IT_WSS_CA"))]
}

/// Points `member`'s identity file at the broker `url`, as if the hub had
/// named it when the worktree joined: one machine reaches the broker
/// through Toxiproxy or the WSS listener while the other connects
/// directly. The file stays where `agmen join` wrote it, so it still
/// passes the trust test.
pub fn use_broker(member: &mut Member, url: &str) {
    member.identity.broker_url = url.parse().unwrap();
    let file = member.wt.path().join(".agmen/identity.json");
    std::fs::write(file, serde_json::to_vec(&member.identity).unwrap()).unwrap();
}

/// Runs `agmen <args>` in `member`'s worktree with `vars` set as well.
pub async fn run_with(
    member: &Member,
    args: &[&str],
    vars: &[(String, String)],
) -> std::process::Output {
    let mut cmd = member.home.agmen(&member.wt.path(), args);
    cmd.envs(vars.iter().map(|(name, value)| (name, value)));
    tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap()
}

/// `member`'s hub client, with the identity token `agmen join` stored.
pub fn hub_client(member: &Member) -> HubClient {
    let base = &member.identity.hub_url;
    let token = member
        .home
        .tokens()
        .get(&TokenKey::new(base, &member.identity.identity_id).unwrap())
        .unwrap()
        .expect("join stored the identity token");
    HubClient::new(
        base.clone(),
        member.identity.project.clone(),
        member.identity.identity_id.clone(),
        token,
    )
    .unwrap()
}

/// `stream` from the start, as `member` reads it through the hub's replay
/// API (one page of up to 500, more than any test here writes).
pub async fn read(member: &Member, stream: &Stream) -> Vec<StampedEnvelope> {
    hub_client(member).page(stream, 0).await.unwrap().messages
}

/// A running `agmen watch --json` and the lines it printed so far.
pub struct Watch {
    child: Child,
    lines: Lines<BufReader<ChildStdout>>,
    /// Events read while waiting for the first connection, not yet taken.
    early: VecDeque<ClientEvent>,
    /// Every line read, as printed.
    pub printed: Vec<String>,
}

impl Watch {
    /// Starts `agmen watch --json` in `member`'s worktree with `vars` set as
    /// well, and returns once it has printed its self line and connected.
    /// `sessions` ends its broker session after the test.
    pub async fn start(sessions: &Sessions, member: &Member, vars: &[(String, String)]) -> Self {
        sessions.add(member, "watch").await;
        let mut cmd = member.home.agmen(&member.wt.path(), &["watch", "--json"]);
        cmd.envs(vars.iter().map(|(name, value)| (name, value)));
        let mut child = tokio::process::Command::from(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut watch = Self {
            child,
            lines,
            early: VecDeque::new(),
            printed: Vec::new(),
        };
        match watch.read(WAIT).await {
            Some(ClientEvent::Me(me)) => assert_eq!(me.identity_id, member.identity.identity_id),
            other => panic!("watch printed {other:?} before its self line"),
        }
        loop {
            match watch.read(WAIT).await {
                Some(ClientEvent::Connection { connected: true }) => break,
                Some(event) => watch.early.push_back(event),
                None => panic!("agmen watch did not connect in time"),
            }
        }
        watch
    }

    /// The next line as an event, or `None` when none comes within `within`.
    async fn read(&mut self, within: Duration) -> Option<ClientEvent> {
        let line = tokio::time::timeout(within, self.lines.next_line())
            .await
            .ok()?
            .unwrap()
            .expect("agmen watch stopped");
        let event = serde_json::from_str(&line)
            .unwrap_or_else(|err| panic!("agmen watch printed {line:?}: {err}"));
        self.printed.push(line);
        Some(event)
    }

    /// The next event; panics when none comes within [`WAIT`].
    pub async fn next(&mut self) -> ClientEvent {
        match self.early.pop_front() {
            Some(event) => event,
            None => self
                .read(WAIT)
                .await
                .expect("agmen watch printed nothing in time"),
        }
    }

    /// The next message the watch delivers that `wanted` accepts, skipping
    /// every other event; panics when none comes within [`WAIT`] each.
    pub async fn message(&mut self, wanted: impl Fn(&StampedEnvelope) -> bool) -> StampedEnvelope {
        loop {
            if let ClientEvent::Message { message } = self.next().await
                && wanted(&message)
            {
                return *message;
            }
        }
    }

    /// Every message the watch delivers until it has printed nothing for
    /// `quiet`.
    pub async fn messages_until_quiet(&mut self, quiet: Duration) -> Vec<StampedEnvelope> {
        let mut events: Vec<ClientEvent> = self.early.drain(..).collect();
        while let Some(event) = self.read(quiet).await {
            events.push(event);
        }
        events
            .into_iter()
            .filter_map(|event| match event {
                ClientEvent::Message { message } => Some(*message),
                _ => None,
            })
            .collect()
    }

    /// Kills the watch, so the broker sends its last will, and returns
    /// what it printed.
    pub async fn stop(mut self) -> Vec<String> {
        self.child.kill().await.unwrap();
        self.printed
    }
}

/// `agmen mcp` as a child process with an MCP client on its stdin and
/// stdout, the way an agent harness other than Claude Code runs it.
pub struct StdioMcp {
    client: RunningService<RoleClient, ()>,
    child: Child,
}

impl StdioMcp {
    /// Starts `agmen mcp` in `member`'s worktree and completes the MCP
    /// handshake. `sessions` ends its broker session after the test.
    pub async fn start(sessions: &Sessions, member: &Member) -> Self {
        sessions.add(member, "mcp").await;
        let cmd = member.home.agmen(&member.wt.path(), &["mcp"]);
        let mut child = tokio::process::Command::from(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let io = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
        let client = tokio::time::timeout(WAIT, ().serve(io))
            .await
            .expect("the MCP handshake in time")
            .expect("the MCP handshake");
        Self { client, child }
    }

    /// Calls `tool` with `args` (a JSON object) and returns its text and
    /// whether it is marked as an error.
    pub async fn call(&self, tool: &'static str, args: Value) -> (String, bool) {
        let Value::Object(args) = args else {
            panic!("tool arguments are a JSON object");
        };
        let result = tokio::time::timeout(
            WAIT,
            self.client
                .call_tool(CallToolRequestParams::new(tool).with_arguments(args)),
        )
        .await
        .unwrap_or_else(|_| panic!("{tool} answered in time"))
        .unwrap_or_else(|err| panic!("calling {tool}: {err}"));
        (text_of(&result), result.is_error == Some(true))
    }

    /// Closes the client; `agmen mcp` must then exit on its own, cleanly.
    pub async fn stop(mut self) {
        self.client.cancel().await.unwrap();
        let status = tokio::time::timeout(WAIT, self.child.wait())
            .await
            .expect("agmen mcp exits once its client is gone")
            .unwrap();
        assert!(status.success(), "agmen mcp exited with {status}");
    }
}

/// The persistent broker sessions a test's `agmen watch` and `agmen mcp`
/// processes opened, which [`run`] ends after the test.
#[derive(Debug, Clone, Default)]
pub struct Sessions(Arc<Mutex<BTreeMap<String, Login>>>);

/// How to connect as one client: its id, its identity and a broker token,
/// taken while the test's hub still runs.
#[derive(Debug)]
struct Login {
    identity: String,
    token: String,
}

impl Sessions {
    /// Records `member`'s `<id>-<suffix>` session.
    async fn add(&self, member: &Member, suffix: &str) {
        let hub = hub_client(member);
        let login = Login {
            identity: hub.identity().to_string(),
            token: hub.broker_token().await.unwrap(),
        };
        let client_id = format!("{}-{suffix}", login.identity);
        self.0.lock().unwrap().insert(client_id, login);
    }
}

/// Runs `body` with an in-process hub as agmen-cli's `support::run` does,
/// and once it has ended, even by panicking (which kills its child
/// processes), ends every session in its [`Sessions`]. Without this each
/// `agmen watch` and `agmen mcp` would leave a persistent session on the
/// shared broker for a day.
pub async fn run<F, Fut>(body: F)
where
    F: FnOnce(Ctx, Sessions) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    support::run(Ingest::On, |ctx| async move {
        let sessions = Sessions::default();
        let outcome = tokio::spawn(body(ctx, sessions.clone())).await;
        let opened = std::mem::take(&mut *sessions.0.lock().unwrap());
        for (client_id, login) in &opened {
            forget_session(client_id, login).await;
        }
        if let Err(err) = outcome {
            std::panic::resume_unwind(err.into_panic());
        }
    })
    .await;
}

/// Ends the persistent session `client_id` once its process has stopped:
/// connects as that client with a clean start and no session expiry, which
/// makes the broker discard the session, and disconnects.
async fn forget_session(client_id: &str, login: &Login) {
    let options = crate::mqtt::options(client_id, &login.identity, &login.token);
    let raw = crate::mqtt::connect_ok(options).await;
    raw.disconnect().await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while emqx_client(client_id).await.is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the broker still holds {client_id}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The dev stack's Toxiproxy proxy in front of EMQX's MQTT listener
/// (`AGMEN_IT_MQTT_PROXIED`, deploy/dev/toxiproxy.json). Tests that cut it
/// run one at a time (.config/nextest.toml).
pub const PROXY: &str = "emqx_mqtt";

/// Enables or disables [`PROXY`]. Disabling it closes every connection
/// through it and refuses new ones; nothing else on the stack changes.
pub async fn set_proxy(enabled: bool) {
    let mut url = Url::parse(&env("AGMEN_IT_TOXIPROXY")).unwrap();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend(["proxies", PROXY]);
    let resp = reqwest::Client::new()
        .post(url)
        .json(&serde_json::json!({ "enabled": enabled }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "toxiproxy answered {}",
        resp.status()
    );
}

/// What EMQX's REST API knows about `client_id`'s connection or session,
/// or `None` when it has neither.
pub async fn emqx_client(client_id: &str) -> Option<Value> {
    let mut url = Url::parse(&env("AGMEN_IT_EMQX_API")).unwrap();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend(["clients", client_id]);
    let resp = reqwest::Client::new()
        .get(url)
        .basic_auth(
            env("AGMEN_IT_EMQX_API_KEY"),
            Some(env("AGMEN_IT_EMQX_API_SECRET")),
        )
        .send()
        .await
        .unwrap();
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return None;
    }
    assert!(
        resp.status().is_success(),
        "EMQX answered {}",
        resp.status()
    );
    Some(resp.json().await.unwrap())
}
