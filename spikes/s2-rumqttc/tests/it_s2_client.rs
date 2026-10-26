//! Spike S2: the MQTT 5 client settings agmen-client uses, run against the
//! dev EMQX in `deploy/dev`. The results, per candidate crate, are in
//! `docs/adr/0003-mqtt-client.md`.
//!
//! Every test mints its own identity ids, client ids and shared
//! subscription group, so tests can run in parallel. Only
//! `it_reconnect_after_toxiproxy_cut` connects through toxiproxy; the
//! `toxiproxy` test group in `.config/nextest.toml` keeps tests like it
//! from running at the same time.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

use std::collections::HashMap;
use std::future::Future;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rumqttc::tokio_rustls::rustls::pki_types::CertificateDer;
use rumqttc::tokio_rustls::rustls::pki_types::pem::PemObject;
use rumqttc::tokio_rustls::rustls::{ClientConfig, RootCertStore};
use rumqttc::{
    AckMode, AsyncClient, ConnAck, ConnectionError, DisconnectReasonCode, Event, EventLoop,
    MqttOptions, Packet, PersistedSession, PubAckReason, Publish, PublishOptions, QoS,
    SessionStore, SessionStoreError, SessionStoreKey, StateError, SubscribeReasonCode,
    TlsConfiguration,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{timeout, timeout_at};

const TENANT: &str = "t1";
const PROJECT: &str = "p1";
/// The hub's MQTT username and `sub` (ADR-0002).
const HUB_USER: &str = "agmen-hub";
/// Key id of the dev signing key; `deploy/dev/init.sh` writes `<kid>.pem`.
const DEV_KID: &str = "agmen-dev-1";
/// Largest packet the client accepts (MQTT 5 Maximum Packet Size), set to
/// EMQX's own cap. rumqttc-next defaults to 10 KiB, below the 64 KiB
/// envelope cap.
const CLIENT_MAX_PACKET: u32 = 96 * 1024;
const ACK_WAIT: Duration = Duration::from_secs(10);
/// MQTT keep-alive for agmen clients, in seconds. EMQX drops a client after
/// 1.5 times this with no packets; HAProxy's tunnel timeout (1 h) is longer.
const KEEP_ALIVE_SECS: u16 = 30;
/// How long the broker keeps a disconnected session (24 h).
const SESSION_EXPIRY_SECS: u32 = 86_400;
/// Pause between reconnect attempts.
const RETRY_DELAY: Duration = Duration::from_secs(1);
/// Failed reconnect attempts in a row before the poller gives up.
const MAX_RETRIES: u32 = 60;

// ---------------------------------------------------------------- env

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is unset: run deploy/dev/init.sh, start the dev stack, then \
             `set -a; . deploy/dev/.secrets/dev.env; set +a`"
        )
    })
}

/// Host and port from an `mqtt://host:port` variable.
fn mqtt_addr(var: &str) -> (String, u16) {
    let url = env(var);
    let rest = url
        .strip_prefix("mqtt://")
        .unwrap_or_else(|| panic!("{var} must start with mqtt://, got {url}"));
    let (host, port) = rest
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("{var} must name a port, got {url}"));
    (host.to_owned(), port.parse().unwrap())
}

/// A fresh identity id in the hub's format: `id` plus 8 of `[a-z0-9]`.
fn new_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    // RandomState is seeded randomly per process and per instance.
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    );
    let mut bits = hasher.finish();
    let mut id = String::from("id");
    for _ in 0..8 {
        id.push(char::from(ALPHABET[usize::try_from(bits % 36).unwrap()]));
        bits /= 36;
    }
    id
}

fn topic(kind: &str, id: &str) -> String {
    format!("agmen/v1/{TENANT}/{PROJECT}/{kind}/{id}")
}

// ---------------------------------------------------------------- tokens

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The identity claims of ADR-0002, trimmed to the topics these tests use.
fn identity_claims(id: &str, exp: u64) -> Value {
    json!({
        "sub": id, "username": id, "exp": exp,
        "acl": [
            { "permission": "allow", "action": "publish",   "topic": format!("eq {}", topic("up", id)), "qos": [1] },
            { "permission": "allow", "action": "subscribe", "topic": format!("eq {}", topic("inbox", id)) },
            { "permission": "deny",  "action": "all",       "topic": "#" }
        ]
    })
}

/// The hub claims of ADR-0002.
fn hub_claims(exp: u64) -> Value {
    json!({
        "sub": HUB_USER, "username": HUB_USER, "exp": exp,
        "acl": [
            { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/+/+/up/+" },
            { "permission": "deny",  "action": "publish",   "topic": "agmen/v1/+/+/up/#" },
            { "permission": "allow", "action": "publish",   "topic": "agmen/v1/#" },
            { "permission": "deny",  "action": "all",       "topic": "#" }
        ]
    })
}

/// Signs `claims` with ES256 using the dev key from `AGMEN_IT_JWT_DIR`.
fn sign(claims: &Value) -> String {
    let path = PathBuf::from(env("AGMEN_IT_JWT_DIR")).join(format!("{DEV_KID}.pem"));
    let pem = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let key = EncodingKey::from_ec_pem(&pem).unwrap();
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(DEV_KID.to_owned());
    jsonwebtoken::encode(&header, claims, &key).unwrap()
}

fn identity_token(id: &str) -> String {
    sign(&identity_claims(id, now_secs() + 3600))
}

// ---------------------------------------------------------------- options

/// Where a client connects.
#[derive(Debug, Clone, Copy)]
enum Endpoint {
    /// Plain MQTT on `AGMEN_IT_MQTT`.
    Direct,
    /// Plain MQTT through toxiproxy on `AGMEN_IT_MQTT_PROXIED`.
    Proxied,
    /// MQTT over WebSocket over TLS through HAProxy on `AGMEN_IT_MQTT_WSS`.
    Wss,
}

fn base_options(endpoint: Endpoint, client_id: &str) -> MqttOptions {
    match endpoint {
        Endpoint::Direct => MqttOptions::new(client_id, mqtt_addr("AGMEN_IT_MQTT")),
        Endpoint::Proxied => MqttOptions::new(client_id, mqtt_addr("AGMEN_IT_MQTT_PROXIED")),
        Endpoint::Wss => MqttOptions::websocket_with_tls_config(
            client_id,
            env("AGMEN_IT_MQTT_WSS"),
            TlsConfiguration::Rustls(dev_tls()),
        )
        .unwrap(),
    }
}

/// Makes aws-lc-rs the process-level rustls provider (ADR-0003). The
/// workspace builds rustls with both the `ring` and `aws-lc-rs` features, so
/// `ClientConfig::builder()` panics until a provider is installed.
fn install_crypto_provider() {
    // Err means a provider is already installed. Only this function installs
    // one in this test binary, so it is aws-lc-rs from an earlier call.
    #[expect(
        clippy::let_underscore_must_use,
        reason = "an earlier call already installed aws-lc-rs"
    )]
    let _ = rumqttc::tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// TLS that trusts only the dev CA in `AGMEN_IT_WSS_CA`.
fn dev_tls() -> Arc<ClientConfig> {
    install_crypto_provider();
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(&std::fs::read(env("AGMEN_IT_WSS_CA")).unwrap()) {
        roots.add(cert.unwrap()).unwrap();
    }
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// The client's own record of its MQTT session, which rumqttc-next loads
/// when a new event loop (a new process) resumes a session the broker kept.
/// agmen-client keeps it in a file; these tests keep the same encoded bytes
/// in memory and share one store between the connects of one client id.
#[derive(Debug, Default)]
struct MemoryStore(Mutex<HashMap<SessionStoreKey, Vec<u8>>>);

/// rumqttc-next names this type in `SessionStore` but does not export it.
type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SessionStoreError>> + Send + 'a>>;

impl SessionStore for MemoryStore {
    fn load<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, Option<PersistedSession>> {
        let bytes = self.0.lock().unwrap().get(key).cloned();
        Box::pin(async move {
            Ok(match bytes {
                Some(bytes) => Some(PersistedSession::decode(&bytes)?),
                None => None,
            })
        })
    }

    fn save<'a>(
        &'a self,
        key: &'a SessionStoreKey,
        session: &'a PersistedSession,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let bytes = session.encode()?;
            self.0.lock().unwrap().insert(key.clone(), bytes);
            Ok(())
        })
    }

    fn clear<'a>(&'a self, key: &'a SessionStoreKey) -> StoreFuture<'a, ()> {
        self.0.lock().unwrap().remove(key);
        Box::pin(async { Ok(()) })
    }
}

/// The connection settings agmen-client uses for `<id>-mcp` and
/// `<id>-watch` (ADR-0003).
fn agmen_options(
    endpoint: Endpoint,
    client_id: &str,
    username: &str,
    token: &str,
    store: &Arc<MemoryStore>,
) -> MqttOptions {
    let mut opts = base_options(endpoint, client_id);
    opts.set_credentials(username, token.to_owned())
        .set_session_store_arc(store.clone())
        .set_clean_start(false)
        .set_session_expiry_interval(Some(SESSION_EXPIRY_SECS))
        .set_ack_mode(AckMode::Manual)
        .set_keep_alive(KEEP_ALIVE_SECS)
        .set_max_packet_size(Some(CLIENT_MAX_PACKET));
    opts
}

/// The hub side of each test: a clean session with automatic acks.
fn hub_options(client_id: &str, token: &str) -> MqttOptions {
    let mut opts = base_options(Endpoint::Direct, client_id);
    opts.set_credentials(HUB_USER, token.to_owned())
        .set_keep_alive(KEEP_ALIVE_SECS)
        .set_max_packet_size(Some(CLIENT_MAX_PACKET));
    opts
}

// ---------------------------------------------------------------- clients

/// What the poller task forwards.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "a test channel item; boxing Packet would only add derefs to every match"
)]
enum Ev {
    Packet(Packet),
    Error(ConnectionError),
}

/// Runs on the options before each reconnect attempt, for example to set a
/// fresh token.
type BeforeReconnect = Box<dyn FnMut(&mut MqttOptions) + Send>;

/// How the poller treats a connection error.
enum Reconnect {
    /// Report it and stop, so the test sees exactly one connection.
    Never,
    /// Report it, wait `RETRY_DELAY` and poll again, which reconnects; give
    /// up after `MAX_RETRIES` failed attempts in a row.
    Retry(Option<BeforeReconnect>),
}

/// One MQTT 5 client whose event loop runs on its own task.
#[derive(Debug)]
struct Client {
    client: AsyncClient,
    events: mpsc::Receiver<Ev>,
    poller: JoinHandle<()>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.poller.abort();
    }
}

/// Connects, waits up to 10 s for CONNACK, then hands the event loop to a
/// poller task.
async fn connect_with(opts: MqttOptions, reconnect: Reconnect) -> (Client, ConnAck) {
    let id = opts.client_id();
    let (client, mut eventloop) = AsyncClient::builder(opts).capacity(64).try_build().unwrap();
    let connack = loop {
        match timeout(ACK_WAIT, eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::ConnAck(ack)))) => break ack,
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("{id} should connect: {e:?}"),
            Err(_) => panic!("{id}: no CONNACK within 10 s"),
        }
    };
    let (tx, events) = mpsc::channel(1024);
    let poller = tokio::spawn(poll_events(eventloop, reconnect, tx));
    let client = Client {
        client,
        events,
        poller,
    };
    (client, connack)
}

async fn connect(opts: MqttOptions) -> (Client, ConnAck) {
    connect_with(opts, Reconnect::Never).await
}

/// Connects with a poller that keeps the connection up.
async fn connect_resilient(
    opts: MqttOptions,
    before_reconnect: Option<BeforeReconnect>,
) -> (Client, ConnAck) {
    connect_with(opts, Reconnect::Retry(before_reconnect)).await
}

async fn poll_events(mut eventloop: EventLoop, mut reconnect: Reconnect, tx: mpsc::Sender<Ev>) {
    let mut failures = 0_u32;
    loop {
        let ev = match eventloop.poll().await {
            Ok(Event::Incoming(packet)) => {
                if matches!(packet, Packet::ConnAck(_)) {
                    failures = 0;
                }
                Ev::Packet(packet)
            }
            Ok(Event::Outgoing(_) | Event::Auth(_)) => continue,
            Err(e) => Ev::Error(e),
        };
        let failed = matches!(ev, Ev::Error(_));
        if tx.send(ev).await.is_err() {
            return;
        }
        if !failed {
            continue;
        }
        match &mut reconnect {
            Reconnect::Never => return,
            Reconnect::Retry(before) => {
                failures += 1;
                if failures > MAX_RETRIES {
                    return;
                }
                tokio::time::sleep(RETRY_DELAY).await;
                if let Some(before) = before {
                    before(&mut eventloop.options);
                }
            }
        }
    }
}

impl Client {
    /// The next event within `within`, or `None` on timeout or when the
    /// poller has stopped.
    async fn next(&mut self, within: Duration) -> Option<Ev> {
        timeout(within, self.events.recv()).await.ok().flatten()
    }

    /// The next packet, which must not be a publish: the tests that wait
    /// for an ack have nothing queued for the client.
    async fn next_control(&mut self) -> Packet {
        match self.next(ACK_WAIT).await {
            Some(Ev::Packet(Packet::Publish(p))) => panic!("unexpected publish on {:?}", p.topic),
            Some(Ev::Packet(packet)) => packet,
            Some(Ev::Error(e)) => panic!("connection error while waiting for an ack: {e:?}"),
            None => panic!("no ack within 10 s"),
        }
    }

    /// The next connection error within `within`, skipping packets.
    async fn next_error(&mut self, within: Duration) -> Option<ConnectionError> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match timeout_at(deadline, self.events.recv()).await {
                Ok(Some(Ev::Error(e))) => return Some(e),
                Ok(Some(Ev::Packet(_))) => {}
                Ok(None) | Err(_) => return None,
            }
        }
    }

    async fn subscribe(&mut self, filter: &str) -> SubscribeReasonCode {
        self.client
            .subscribe(filter, QoS::AtLeastOnce)
            .await
            .unwrap();
        loop {
            if let Packet::SubAck(ack) = self.next_control().await {
                return ack.return_codes[0];
            }
        }
    }

    async fn publish(&mut self, topic: &str, payload: &[u8]) -> PubAckReason {
        self.client
            .publish(topic, payload.to_vec(), PublishOptions::at_least_once())
            .await
            .unwrap();
        loop {
            if let Packet::PubAck(ack) = self.next_control().await {
                return ack.reason;
            }
        }
    }

    /// The next publish on `topic` within `within`. Other packets, and
    /// publishes on other topics, are skipped; connection errors too, since
    /// a reconnecting client reports each failed attempt.
    async fn recv_on(&mut self, topic: &str, within: Duration) -> Option<Publish> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match timeout_at(deadline, self.events.recv()).await {
                Ok(Some(Ev::Packet(Packet::Publish(p)))) if p.topic == topic.as_bytes() => {
                    return Some(p);
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// Waits for the CONNACK of a reconnect, skipping the errors of the
    /// attempts before it.
    async fn reconnected(&mut self, within: Duration) -> ConnAck {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match timeout_at(deadline, self.events.recv()).await {
                Ok(Some(Ev::Packet(Packet::ConnAck(ack)))) => return ack,
                Ok(Some(_)) => {}
                Ok(None) => panic!("the poller stopped without reconnecting"),
                Err(_) => panic!("no reconnect within {within:?}"),
            }
        }
    }

    /// Sends DISCONNECT and waits for the poller to report the closed
    /// connection.
    async fn disconnect(&mut self) {
        self.client.disconnect().await.unwrap();
        loop {
            match self.next(ACK_WAIT).await {
                Some(Ev::Error(_)) | None => return,
                Some(Ev::Packet(_)) => {}
            }
        }
    }
}

/// A clean-session hub client in its own shared subscription group, so
/// parallel tests each get every `up/` message.
async fn hub_subscriber() -> Client {
    let client_id = format!("{HUB_USER}-{}", new_id());
    let token = sign(&hub_claims(now_secs() + 3600));
    let (mut hub, _) = connect(hub_options(&client_id, &token)).await;
    let filter = format!("$share/hub{}/agmen/v1/+/+/up/+", new_id());
    assert_eq!(
        hub.subscribe(&filter).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    hub
}

/// A hub client that only publishes.
async fn hub_publisher() -> Client {
    let client_id = format!("{HUB_USER}-{}", new_id());
    let token = sign(&hub_claims(now_secs() + 3600));
    connect(hub_options(&client_id, &token)).await.0
}

// ---------------------------------------------------------------- tests

/// A second connect with the same client id resumes the session.
#[tokio::test]
async fn it_resume_session_flag() {
    let a = new_id();
    let token = identity_token(&a);
    let store = Arc::default();
    let opts = || agmen_options(Endpoint::Direct, &format!("{a}-mcp"), &a, &token, &store);
    let (mut first, connack) = connect(opts()).await;
    assert!(!connack.session_present, "a fresh client id had a session");
    assert_eq!(
        first.subscribe(&topic("inbox", &a)).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    first.disconnect().await;
    drop(first);

    // A new event loop, as when the MCP server restarts: it resumes the
    // session and can still publish.
    let (mut second, connack) = connect(opts()).await;
    assert!(connack.session_present, "EMQX did not resume the session");
    // Without a subscriber EMQX acks with NoMatchingSubscribers.
    let _hub = hub_subscriber().await;
    assert_eq!(
        second.publish(&topic("up", &a), b"after resume").await,
        PubAckReason::Success
    );
}

/// A QoS 1 publish that was never acked comes back, with the dup flag, on
/// the next connection; once acked it does not.
#[tokio::test]
async fn it_manual_ack_redelivery() {
    let a = new_id();
    let token = identity_token(&a);
    let inbox = topic("inbox", &a);
    let store = Arc::default();
    let opts = || agmen_options(Endpoint::Direct, &format!("{a}-watch"), &a, &token, &store);
    let (mut watch, _) = connect(opts()).await;
    assert_eq!(
        watch.subscribe(&inbox).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    let mut hub = hub_publisher().await;
    assert_eq!(hub.publish(&inbox, b"dm 1").await, PubAckReason::Success);
    let first = watch
        .recv_on(&inbox, ACK_WAIT)
        .await
        .expect("dm 1 never arrived");
    assert!(!first.dup);
    // Drop without acking: the poller task and its socket go away.
    drop(watch);

    let (mut watch, connack) = connect(opts()).await;
    assert!(connack.session_present);
    let again = watch
        .recv_on(&inbox, ACK_WAIT)
        .await
        .expect("dm 1 was not redelivered");
    assert!(again.dup, "redelivery without the dup flag");
    assert_eq!(again.pkid, first.pkid);
    assert_eq!(again.payload, first.payload);
    watch.client.ack(&again).await.unwrap();
    watch.disconnect().await;
    drop(watch);

    let (mut watch, connack) = connect(opts()).await;
    assert!(connack.session_present);
    assert!(
        watch
            .recv_on(&inbox, Duration::from_secs(2))
            .await
            .is_none(),
        "an acked message came back"
    );
}

/// The WSS transport through HAProxy connects and carries a message each
/// way: up to the hub, and back down to the client's inbox.
#[tokio::test]
async fn it_wss_transport() {
    let a = new_id();
    let mut hub = hub_subscriber().await;
    let (mut client, _) = connect(agmen_options(
        Endpoint::Wss,
        &format!("{a}-mcp"),
        &a,
        &identity_token(&a),
        &Arc::default(),
    ))
    .await;
    let inbox = topic("inbox", &a);
    assert_eq!(
        client.subscribe(&inbox).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    assert_eq!(
        client.publish(&topic("up", &a), b"ping").await,
        PubAckReason::Success
    );
    let up = hub.recv_on(&topic("up", &a), ACK_WAIT).await;
    assert_eq!(up.map(|p| p.payload), Some("ping".into()));

    let mut publisher = hub_publisher().await;
    assert_eq!(
        publisher.publish(&inbox, b"pong").await,
        PubAckReason::Success
    );
    let down = client.recv_on(&inbox, ACK_WAIT).await;
    assert_eq!(down.map(|p| p.payload), Some("pong".into()));
}

/// Cuts the proxied link for 10 s; the event loop reconnects on its own,
/// and a publish queued during the cut reaches the hub exactly once.
#[tokio::test]
async fn it_reconnect_after_toxiproxy_cut() {
    // A run that failed mid-cut leaves the proxy disabled; start clean.
    set_proxy_enabled(true).await;
    let a = new_id();
    let up = topic("up", &a);
    let mut hub = hub_subscriber().await;
    let (mut client, _) = connect_resilient(
        agmen_options(
            Endpoint::Proxied,
            &format!("{a}-mcp"),
            &a,
            &identity_token(&a),
            &Arc::default(),
        ),
        None,
    )
    .await;
    assert_eq!(client.publish(&up, b"before").await, PubAckReason::Success);
    assert!(hub.recv_on(&up, ACK_WAIT).await.is_some());

    let cut_at = tokio::time::Instant::now();
    set_proxy_enabled(false).await;
    assert!(
        client.next_error(ACK_WAIT).await.is_some(),
        "the cut was not noticed"
    );
    client
        .client
        .publish(&up, b"during".to_vec(), PublishOptions::at_least_once())
        .await
        .unwrap();
    tokio::time::sleep_until(cut_at + Duration::from_secs(10)).await;
    set_proxy_enabled(true).await;

    let connack = client.reconnected(Duration::from_secs(30)).await;
    assert!(connack.session_present);
    let got = hub.recv_on(&up, Duration::from_secs(10)).await;
    assert_eq!(got.map(|p| p.payload), Some("during".into()));
    assert!(
        hub.recv_on(&up, Duration::from_secs(3)).await.is_none(),
        "the queued publish arrived twice"
    );
}

/// The broker drops the client when its 5 s token expires; the client
/// fetches a new token, reconnects to the same session and still gets its
/// inbox.
#[tokio::test]
async fn it_token_refresh_reconnect() {
    let a = new_id();
    let inbox = topic("inbox", &a);
    let short = sign(&identity_claims(&a, now_secs() + 5));
    let user = a.clone();
    let refresh: BeforeReconnect = Box::new(move |opts: &mut MqttOptions| {
        opts.set_credentials(user.clone(), identity_token(&user));
    });
    let (mut watch, _) = connect_resilient(
        agmen_options(
            Endpoint::Direct,
            &format!("{a}-watch"),
            &a,
            &short,
            &Arc::default(),
        ),
        Some(refresh),
    )
    .await;
    assert_eq!(
        watch.subscribe(&inbox).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    match watch.next_error(Duration::from_secs(15)).await {
        Some(ConnectionError::MqttState(StateError::ServerDisconnect {
            reason_code: DisconnectReasonCode::NotAuthorized,
            ..
        })) => {}
        other => panic!("expected DISCONNECT 0x87 at token expiry, got {other:?}"),
    }
    let connack = watch.reconnected(Duration::from_secs(15)).await;
    assert!(
        connack.session_present,
        "the refreshed connection lost the session"
    );

    let mut hub = hub_publisher().await;
    assert_eq!(hub.publish(&inbox, b"after").await, PubAckReason::Success);
    let got = watch.recv_on(&inbox, ACK_WAIT).await;
    assert_eq!(got.map(|p| p.payload), Some("after".into()));
}

// ---------------------------------------------------------------- toxiproxy

/// Enables or disables the shared `emqx_mqtt` proxy (localhost:21883 to
/// emqx:1883). Disabling it closes every connection through it.
async fn set_proxy_enabled(enabled: bool) {
    let url = format!("{}/proxies/emqx_mqtt", env("AGMEN_IT_TOXIPROXY"));
    let response = reqwest::Client::new()
        .post(&url)
        .header("content-type", "application/json")
        .body(json!({ "enabled": enabled }).to_string())
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "toxiproxy {url}: {}",
        response.status()
    );
}
