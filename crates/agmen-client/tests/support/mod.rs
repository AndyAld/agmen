//! Harness for agmen-client's integration tests: a hub in this process on
//! a database of its own (ruling R26), connected to the dev EMQX, with a
//! fresh project, and identities made through the hub's `test-hooks`.
//!
//! Ids, projects and client ids are random per test, so tests run in
//! parallel and never touch each other's broker sessions. The token store
//! is never involved: tests hand the identity's hub token to `HubClient`
//! directly, so nothing reaches the user's keychain.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the helpers"
)]

use std::future::Future;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agmen_client::{
    Broker, ClientEvent, ClientKind, Events, HubClient, OutboxState, Session, SessionConfig,
};
use agmen_hub::{Config, EmqxConfig, GithubConfig, Hub, Identity, Messaging, MqttConfig, Secret};
use agmen_proto::{
    ClientEnvelope, IdentityId, MessageId, Seq, StampedEnvelope, Stream, Timestamp, Topics,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use url::Url;

/// How long a test waits for one event.
pub const EVENT_WAIT: Duration = Duration::from_secs(20);

pub fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is unset: run deploy/dev/init.sh, start the dev stack, then \
             `set -a; . deploy/dev/.secrets/dev.env; set +a`"
        )
    })
}

/// `count` random characters from `alphabet`.
fn random(alphabet: &[u8], count: usize) -> String {
    let mut out = String::with_capacity(count);
    while out.len() < count {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        );
        let mut bits = hasher.finish();
        for _ in 0..8 {
            if out.len() == count {
                break;
            }
            let len = u64::try_from(alphabet.len()).unwrap();
            out.push(char::from(alphabet[usize::try_from(bits % len).unwrap()]));
            bits /= len;
        }
    }
    out
}

/// A fresh id in the hub's format.
pub fn new_id() -> String {
    random(b"abcdefghijklmnopqrstuvwxyz0123456789", 16)
}

/// A fresh ULID-shaped message id.
pub fn new_message_id() -> MessageId {
    let id = format!("0{}", random(b"0123456789ABCDEFGHJKMNPQRSTVWXYZ", 25));
    id.parse().unwrap()
}

/// An FYI with a fresh id.
pub fn fyi(body: &str) -> ClientEnvelope {
    let raw = serde_json::json!({
        "v": 1, "id": new_message_id(), "verb": "FYI", "data": {},
        "body": body, "sent_at": Timestamp::now(),
    });
    ClientEnvelope::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap()
}

/// A DM to `to` with a fresh id.
pub fn dm(to: &IdentityId, body: &str) -> ClientEnvelope {
    let raw = serde_json::json!({
        "v": 1, "id": new_message_id(), "verb": "DM", "data": {}, "to": [to],
        "body": body, "sent_at": Timestamp::now(),
    });
    ClientEnvelope::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap()
}

/// A database of its own on the `AGMEN_IT_PG` server.
struct TestDb {
    name: String,
    url: String,
    admin: PgPool,
}

impl TestDb {
    async fn create() -> Self {
        let base = env("AGMEN_IT_PG");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&base)
            .await
            .expect("connect to AGMEN_IT_PG");
        let name = format!("agmen_it_client_{}", new_id());
        // The name is a fixed prefix plus [a-z0-9], safe to splice into DDL.
        sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&admin)
            .await
            .expect("create test database");
        let mut url = Url::parse(&base).unwrap();
        url.set_path(&name);
        Self {
            name,
            url: url.into(),
            admin,
        }
    }

    async fn drop_db(self) {
        sqlx::query(AssertSqlSafe(format!(
            "DROP DATABASE \"{}\" WITH (FORCE)",
            self.name
        )))
        .execute(&self.admin)
        .await
        .expect("drop test database");
        self.admin.close().await;
    }
}

/// Host and port from an `mqtt://host:port` variable.
fn mqtt_addr(var: &str) -> (String, u16) {
    let url = Url::parse(&env(var)).unwrap();
    (url.host_str().unwrap().to_owned(), url.port().unwrap())
}

/// Everything a test gets from [`run`].
#[derive(Debug)]
pub struct Ctx {
    pub hub: Hub,
    /// The hub's base URL.
    pub base: Url,
    pub topics: Topics,
    /// The dev EMQX, directly.
    pub broker: Broker,
    /// The dev EMQX through toxiproxy.
    pub proxied: Broker,
    /// Holds the identities' state folders.
    pub state: TempDir,
    _messaging: Messaging,
    server: JoinHandle<()>,
}

impl Drop for Ctx {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Runs `body` against a fresh database and an in-process hub with a fresh
/// project, then drops the database, even when `body` panics.
pub async fn run<F, Fut>(body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let db = TestDb::create().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = Url::parse(&format!("http://{addr}")).unwrap();
    let (host, port) = mqtt_addr("AGMEN_IT_MQTT");
    let config = Config {
        database_url: Secret::new(db.url.clone()),
        listen: addr,
        public_url: base.clone(),
        // The client tests never sign in with GitHub; nothing listens here.
        github: GithubConfig {
            client_id: "agmen-client-tests".to_owned(),
            client_secret: Secret::new("unused"),
            url: Url::parse("http://127.0.0.1:9/").unwrap(),
            api_url: Url::parse("http://127.0.0.1:9/").unwrap(),
        },
        emqx: EmqxConfig {
            api_url: Url::parse(&env("AGMEN_IT_EMQX_API")).unwrap(),
            api_key: env("AGMEN_IT_EMQX_API_KEY"),
            api_secret: Secret::new(env("AGMEN_IT_EMQX_API_SECRET")),
        },
        mqtt: MqttConfig {
            host,
            port,
            instance: new_id(),
            share_group: new_id(),
        },
        jwt_signing_key: dev_key(),
        jwt_extra_keys: Vec::new(),
    };
    let hub = Hub::connect(&config).await.expect("start the hub");
    let router = hub.router();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let messaging = hub
        .start_messaging(&config.mqtt)
        .await
        .expect("start messaging");
    let topics = hub.seed_project().await.expect("seed a project");
    let ctx = Ctx {
        hub,
        base,
        topics,
        broker: env("AGMEN_IT_MQTT").parse().unwrap(),
        proxied: env("AGMEN_IT_MQTT_PROXIED").parse().unwrap(),
        state: tempfile::tempdir().unwrap(),
        _messaging: messaging,
        server,
    };
    let outcome = tokio::spawn(body(ctx)).await;
    db.drop_db().await;
    if let Err(err) = outcome {
        std::panic::resume_unwind(err.into_panic());
    }
}

/// The dev signing key, which the dev EMQX trusts through the shared JWKS.
pub fn dev_key() -> PathBuf {
    PathBuf::from(env("AGMEN_IT_JWT_DIR")).join("agmen-dev-1.pem")
}

/// An identity in the test's project and its hub token.
#[derive(Debug, Clone)]
pub struct TestIdentity {
    pub identity: Identity,
    pub token: String,
}

impl Ctx {
    pub async fn identity(&self) -> TestIdentity {
        let (identity, token) = self
            .hub
            .seed_identity(&self.topics)
            .await
            .expect("seed an identity");
        TestIdentity { identity, token }
    }

    pub fn hub_client(&self, who: &TestIdentity) -> HubClient {
        HubClient::new(
            self.base.clone(),
            self.topics.project().clone(),
            who.identity.id.clone(),
            who.token.clone(),
        )
        .unwrap()
    }

    /// The identity's state folder.
    pub fn state_dir(&self, who: &TestIdentity) -> PathBuf {
        self.state.path().join(who.identity.id.as_str())
    }

    /// A session of `kind` for `who` at `broker`, its persistent session
    /// kept for 60 s only, so a test leaves nothing at the broker for long.
    pub fn config(&self, kind: ClientKind, who: &TestIdentity, broker: &Broker) -> SessionConfig {
        SessionConfig::new(
            kind,
            self.topics.clone(),
            self.hub_client(who),
            broker.clone(),
            self.state_dir(who),
        )
        .with_session_expiry(Duration::from_secs(60))
    }
}

/// The next event `pick` accepts, skipping others; panics after
/// [`EVENT_WAIT`].
pub async fn next_event<T>(
    events: &mut Events,
    mut pick: impl FnMut(&ClientEvent) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + EVENT_WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("no matching event in time")
            .expect("the session stopped");
        if let Some(found) = pick(&event) {
            return found;
        }
    }
}

/// Waits for the hub's answer to message `id`, skipping pending states.
pub async fn delivered(events: &mut Events, id: &MessageId) -> OutboxState {
    next_event(events, |event| match event {
        ClientEvent::Delivery { id: got, state } if got == id => match state {
            OutboxState::Pending { .. } => None,
            done => Some(done.clone()),
        },
        _ => None,
    })
    .await
}

/// Waits until message `id` is recorded and returns its feed seq.
pub async fn recorded_on_feed(events: &mut Events, id: &MessageId) -> u64 {
    match delivered(events, id).await {
        OutboxState::Recorded {
            stream: Stream::Feed,
            seq: Seq(seq),
        } => seq,
        other => panic!("{id} was not recorded on the feed: {other:?}"),
    }
}

/// The feed seq of the next message event.
pub async fn next_feed_seq(events: &mut Events) -> u64 {
    next_event(events, |event| match event {
        ClientEvent::Message { message } if message.stream == Stream::Feed => Some(message.seq.0),
        _ => None,
    })
    .await
}

/// Waits for a connection event with `connected`.
pub async fn connection(events: &mut Events, connected: bool) {
    next_event(events, |event| {
        (*event == ClientEvent::Connection { connected }).then_some(())
    })
    .await;
}

/// Turns the shared toxiproxy link to EMQX on or off. Off closes every
/// connection through it.
pub async fn set_proxy_enabled(enabled: bool) {
    let url = format!("{}/proxies/emqx_mqtt", env("AGMEN_IT_TOXIPROXY"));
    let response = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({ "enabled": enabled }))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "toxiproxy {url}: {}",
        response.status()
    );
}

/// Closes `session`, which must take its DISCONNECT and stop well within
/// the 5 s it allows itself.
pub async fn close(session: Session) {
    tokio::time::timeout(Duration::from_secs(3), session.close())
        .await
        .expect("the session closes promptly")
        .expect("the session ran without an error");
}

/// The message events until `done` says enough, skipping other events.
pub async fn messages_until(
    events: &mut Events,
    mut done: impl FnMut(&[StampedEnvelope]) -> bool,
) -> Vec<StampedEnvelope> {
    let mut seen = Vec::new();
    while !done(&seen) {
        let message = next_event(events, |event| match event {
            ClientEvent::Message { message } => Some((**message).clone()),
            _ => None,
        })
        .await;
        seen.push(message);
    }
    seen
}
