//! Harness for agmen-client's integration tests: a hub in this process on
//! a database of its own (ruling R26), connected to the dev EMQX, with a
//! fresh project, and identities made through the hub's `test-hooks`.
//!
//! Ids, projects and client ids are random per test, so tests run in
//! parallel and never touch each other's broker sessions. The token store
//! is never involved: tests hand the identity's hub token to `HubClient`
//! directly, so nothing reaches the user's keychain. [`run`] deletes the
//! retained messages a test leaves for its project (`state/roster`,
//! `state/head`, and each identity's `presence/{id}`), so the dev broker
//! keeps none of them.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the helpers"
)]

use std::future::Future;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agmen_client::{
    Broker, ClientError, ClientEvent, ClientKind, Events, HubClient, IdentityFile, OutboxState,
    Session, SessionConfig, TokenSource,
};
use agmen_hub::{BrokerTokens, Config, Hub, Identity, Messaging, SigningKey};
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
    /// The identities [`Ctx::identity`] made, whose presence [`run`]
    /// clears.
    seeded: Arc<Mutex<Vec<IdentityId>>>,
    _messaging: Messaging,
    server: JoinHandle<()>,
}

impl Drop for Ctx {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Runs `body` against a fresh database and an in-process hub with a fresh
/// project, then deletes the project's retained messages and drops the
/// database, even when `body` panics.
pub async fn run<F, Fut>(body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let db = TestDb::create().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = Url::parse(&format!("http://{addr}")).unwrap();
    let (instance, share_group) = (new_id(), new_id());
    let signing_key = dev_key().to_str().unwrap().to_owned();
    // Built through the hub's own variable parser, so a setting the hub adds
    // later takes its default here instead of breaking this harness.
    let config = Config::from_lookup(|name| {
        Some(match name {
            "AGMEN_DATABASE_URL" => db.url.clone(),
            "AGMEN_LISTEN" => addr.to_string(),
            "AGMEN_PUBLIC_URL" => base.to_string(),
            "AGMEN_GITHUB_CLIENT_ID" => "agmen-client-tests".to_owned(),
            "AGMEN_GITHUB_CLIENT_SECRET" => "unused".to_owned(),
            // The client tests never sign in with GitHub; nothing listens here.
            "AGMEN_GITHUB_URL" | "AGMEN_GITHUB_API_URL" => "http://127.0.0.1:9/".to_owned(),
            "AGMEN_EMQX_API_URL" => env("AGMEN_IT_EMQX_API"),
            "AGMEN_EMQX_API_KEY" => env("AGMEN_IT_EMQX_API_KEY"),
            "AGMEN_EMQX_API_SECRET" => env("AGMEN_IT_EMQX_API_SECRET"),
            "AGMEN_MQTT_URL" => env("AGMEN_IT_MQTT"),
            "AGMEN_HUB_INSTANCE" => instance.clone(),
            "AGMEN_MQTT_SHARE_GROUP" => share_group.clone(),
            "AGMEN_JWT_SIGNING_KEY" => signing_key.clone(),
            _ => return None,
        })
    })
    .expect("build the hub config");
    let hub = Hub::connect(&config).await.expect("start the hub");
    let router = hub.router();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let messaging = hub
        .start_messaging(&config.mqtt)
        .await
        .expect("start messaging");
    let topics = hub.seed_project().await.expect("seed a project");
    let seeded = Arc::new(Mutex::new(Vec::new()));
    let ctx = Ctx {
        hub,
        base,
        topics: topics.clone(),
        broker: env("AGMEN_IT_MQTT").parse().unwrap(),
        proxied: env("AGMEN_IT_MQTT_PROXIED").parse().unwrap(),
        state: tempfile::tempdir().unwrap(),
        seeded: Arc::clone(&seeded),
        _messaging: messaging,
        server,
    };
    // The body owns the hub's messaging, so the hub publishes nothing once
    // it has ended.
    let outcome = tokio::spawn(body(ctx)).await;
    let ids = seeded.lock().unwrap().clone();
    let mut retained = vec![topics.state("roster"), topics.state_head()];
    for id in &ids {
        // A session dropped without DISCONNECT leaves its last will to the
        // broker; wait until it has gone, so the will comes before the
        // deletion.
        for kind in [ClientKind::Watch, ClientKind::Mcp] {
            disconnected(&format!("{id}-{}", kind.suffix())).await;
        }
        retained.push(topics.presence(id));
    }
    clear_retained(&retained).await;
    db.drop_db().await;
    if let Err(err) = outcome {
        std::panic::resume_unwind(err.into_panic());
    }
}

/// The EMQX REST API URL for `segments` under `AGMEN_IT_EMQX_API`.
fn emqx_api(segments: &[&str]) -> Url {
    let mut url = Url::parse(&env("AGMEN_IT_EMQX_API")).unwrap();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend(segments);
    url
}

/// Waits up to 10 s until EMQX lists `client_id` as not connected (or not
/// at all), checking every 100 ms.
pub async fn disconnected(client_id: &str) {
    let http = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let resp = http
            .get(emqx_api(&["clients", client_id]))
            .basic_auth(
                env("AGMEN_IT_EMQX_API_KEY"),
                Some(env("AGMEN_IT_EMQX_API_SECRET")),
            )
            .send()
            .await
            .unwrap();
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return;
        }
        let client: serde_json::Value = resp.json().await.unwrap();
        if client["connected"] == false {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{client_id} is still connected"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Deletes the retained message on each of `topics` through the EMQX REST
/// API. One that is already gone (404) is fine.
pub async fn clear_retained(topics: &[String]) {
    let http = reqwest::Client::new();
    for topic in topics {
        let resp = http
            .delete(emqx_api(&["mqtt", "retainer", "message", topic]))
            .basic_auth(
                env("AGMEN_IT_EMQX_API_KEY"),
                Some(env("AGMEN_IT_EMQX_API_SECRET")),
            )
            .send()
            .await
            .unwrap();
        assert!(
            matches!(resp.status().as_u16(), 200 | 204 | 404),
            "clearing {topic} answered {}",
            resp.status()
        );
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
        self.seeded.lock().unwrap().push(identity.id.clone());
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

    /// The identity file `agmen join` would write for `who`, with the dev
    /// EMQX as its broker.
    pub fn identity_file(&self, who: &TestIdentity) -> IdentityFile {
        IdentityFile {
            tenant: self.topics.tenant().clone(),
            project: self.topics.project().clone(),
            identity_id: who.identity.id.clone(),
            handle: who.identity.handle.clone(),
            owner_id: who.identity.owner.as_str().parse().unwrap(),
            hub_url: self.base.clone(),
            broker_url: self.broker.clone(),
        }
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

/// Mints broker tokens that live 12 s, as the hub would with a short TTL,
/// and counts them. A session swaps such a token every 6 s.
#[derive(Debug, Clone)]
pub struct ShortTokens {
    tokens: Arc<BrokerTokens>,
    identity: Identity,
    minted: Arc<AtomicU32>,
}

impl ShortTokens {
    pub fn new(who: &TestIdentity) -> Self {
        let key = SigningKey::from_pem_file(dev_key()).unwrap();
        Self {
            tokens: Arc::new(BrokerTokens::new(key, Vec::new()).unwrap()),
            identity: who.identity.clone(),
            minted: Arc::default(),
        }
    }

    /// How many tokens it has minted.
    pub fn minted(&self) -> u32 {
        self.minted.load(Ordering::SeqCst)
    }
}

impl TokenSource for ShortTokens {
    async fn token(&self) -> Result<String, ClientError> {
        self.minted.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .tokens
            .mint(&self.identity, Duration::from_secs(12))
            .unwrap())
    }
}

/// Records every payload published on one identity's `presence/{id}`, as
/// another identity of the project, subscribed to it, receives them.
#[derive(Debug)]
pub struct PresenceLog {
    seen: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl PresenceLog {
    /// Subscribes as `reader` (its `-mcp` client id, clean start) and
    /// returns once the subscription is in place.
    pub async fn start(ctx: &Ctx, reader: &TestIdentity, of: &IdentityId) -> Self {
        use rumqttc_next::{AsyncClient, Event, Packet, QoS};
        let url = Url::parse(&env("AGMEN_IT_MQTT")).unwrap();
        let token = ShortTokens::new(reader)
            .tokens
            .mint(&reader.identity, Duration::from_secs(600))
            .unwrap();
        let mut options = rumqttc_next::MqttOptions::new(
            format!("{}-mcp", reader.identity.id),
            (url.host_str().unwrap(), url.port().unwrap()),
        );
        options.set_credentials(reader.identity.id.as_str(), token);
        let (client, mut eventloop) = AsyncClient::builder(options).capacity(16).build();
        let topic = ctx.topics.presence(of);
        client
            .subscribe(topic.clone(), QoS::AtLeastOnce)
            .await
            .unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (subscribed_tx, subscribed_rx) = tokio::sync::oneshot::channel();
        let log = Arc::clone(&seen);
        let task = tokio::spawn(async move {
            // Keeps the client alive as long as the task.
            let _client = client;
            let mut subscribed = Some(subscribed_tx);
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Packet::SubAck(_))) => {
                        if let Some(tx) = subscribed.take() {
                            tx.send(()).unwrap();
                        }
                    }
                    Ok(Event::Incoming(Packet::Publish(publish))) if publish.topic == topic => {
                        let payload = String::from_utf8_lossy(&publish.payload).into_owned();
                        log.lock().unwrap().push(payload);
                    }
                    Ok(_) => {}
                    Err(err) => panic!("the presence observer lost its connection: {err}"),
                }
            }
        });
        tokio::time::timeout(EVENT_WAIT, subscribed_rx)
            .await
            .expect("the presence observer subscribed in time")
            .unwrap();
        Self { seen, task }
    }

    /// The payloads so far, oldest first.
    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for PresenceLog {
    fn drop(&mut self) {
        self.task.abort();
    }
}
