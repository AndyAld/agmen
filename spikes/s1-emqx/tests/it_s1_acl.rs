//! Spike S1: EMQX JWT identity and ACLs, run against the dev broker in
//! `deploy/dev`. The results and the config they prove are recorded in
//! `docs/adr/0002-broker-emqx.md`.
//!
//! Every test mints its own identity ids, client ids and shared
//! subscription group, so tests can run in parallel without seeing each
//! other's sessions. `it_persistent_session_survives_restart` restarts
//! EMQX and runs only under `--profile it-restart` (`.config/nextest.toml`).

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

use std::collections::{HashSet, VecDeque};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_tungstenite::tungstenite::client::IntoClientRequest;
use async_tungstenite::tungstenite::http::HeaderValue;
use async_tungstenite::tungstenite::{Error as WsError, Message};
use bytes::BytesMut;
use futures_util::{Stream, StreamExt};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rumqttc::Transport;
use rumqttc::v5::mqttbytes::v5::{
    ConnAck, Connect, ConnectReturnCode, DisconnectReasonCode, Login, Packet, PubAckReason,
    Publish, SubscribeReasonCode,
};
use rumqttc::v5::mqttbytes::{Error as MqttBytesError, QoS};
use rumqttc::v5::{AsyncClient, ConnectionError, Event, MqttOptions, StateError};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout, timeout_at};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName};
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

const TENANT: &str = "t1";
const PROJECT: &str = "p1";
/// The hub's MQTT username and `sub`. The `-` keeps it out of the
/// identity id alphabet (`[a-z0-9]{8,32}`), so no identity can collide.
const HUB_USER: &str = "agmen-hub";
/// Key id of the dev signing key; `deploy/dev/init.sh` writes `<kid>.pem`.
const DEV_KID: &str = "agmen-dev-1";
/// A key that is not in the dev JWKS.
const UNTRUSTED_KEY: &str = "untrusted";
/// Large enough for the 60 KiB publish; EMQX itself caps packets at 96 KB.
const CLIENT_MAX_PACKET: u32 = 128 * 1024;
const ACK_WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- env

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is unset: run deploy/dev/init.sh, start the dev stack, then \
             `set -a; . deploy/dev/.secrets/dev.env; set +a`"
        )
    })
}

/// Host and port from `AGMEN_IT_MQTT` (`mqtt://host:port`).
fn mqtt_addr() -> (String, u16) {
    let url = env("AGMEN_IT_MQTT");
    let rest = url
        .strip_prefix("mqtt://")
        .unwrap_or_else(|| panic!("AGMEN_IT_MQTT must start with mqtt://, got {url}"));
    let (host, port) = rest
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("AGMEN_IT_MQTT must name a port, got {url}"));
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

fn up_topic(id: &str) -> String {
    format!("agmen/v1/{TENANT}/{PROJECT}/up/{id}")
}

// ---------------------------------------------------------------- tokens

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The ACL claim the hub mints for one identity (ADR-0002). Topics are
/// written out in full with `eq`, so an id can never widen a rule.
fn identity_acl(tenant: &str, project: &str, id: &str) -> Value {
    let base = format!("agmen/v1/{tenant}/{project}");
    json!([
        { "permission": "allow", "action": "publish",   "topic": format!("eq {base}/up/{id}"), "qos": [1] },
        { "permission": "allow", "action": "publish",   "topic": format!("eq {base}/presence/{id}"), "retain": true },
        { "permission": "allow", "action": "subscribe", "topic": format!("eq {base}/feed") },
        { "permission": "allow", "action": "subscribe", "topic": format!("eq {base}/inbox/{id}") },
        { "permission": "allow", "action": "subscribe", "topic": format!("eq {base}/ack/{id}") },
        { "permission": "allow", "action": "subscribe", "topic": format!("{base}/state/#") },
        { "permission": "allow", "action": "subscribe", "topic": format!("{base}/presence/+") },
        { "permission": "deny",  "action": "all",       "topic": "#" }
    ])
}

fn identity_claims(id: &str, exp: u64) -> Value {
    json!({ "sub": id, "username": id, "exp": exp, "acl": identity_acl(TENANT, PROJECT, id) })
}

/// The hub's own claims (ADR-0002): one shared subscription on every
/// `up/`, and publish anywhere under `agmen/v1/` except `up/`.
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

/// Signs `claims` with ES256 using `<key_file>.pem` from `AGMEN_IT_JWT_DIR`.
/// The header always names `DEV_KID`, so a token signed with the untrusted
/// key differs from a good one only in its signature.
fn sign_with(key_file: &str, claims: &Value) -> String {
    let path = PathBuf::from(env("AGMEN_IT_JWT_DIR")).join(format!("{key_file}.pem"));
    let pem = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let key = EncodingKey::from_ec_pem(&pem).unwrap();
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(DEV_KID.to_owned());
    jsonwebtoken::encode(&header, claims, &key).unwrap()
}

fn sign(claims: &Value) -> String {
    sign_with(DEV_KID, claims)
}

fn identity_token(id: &str) -> String {
    sign(&identity_claims(id, now_secs() + 3600))
}

// ---------------------------------------------------------------- clients

fn tcp_options(client_id: &str, username: &str, token: &str) -> MqttOptions {
    let (host, port) = mqtt_addr();
    let mut opts = MqttOptions::new(client_id, host, port);
    opts.set_credentials(username, token)
        .set_keep_alive(Duration::from_secs(30))
        .set_max_packet_size(Some(CLIENT_MAX_PACKET));
    opts
}

/// rumqttc takes host and port from the URL for WebSocket transports, so
/// the port argument is unused.
fn ws_options(url: &str, client_id: &str, username: &str, token: &str) -> MqttOptions {
    let mut opts = MqttOptions::new(client_id, url, 0);
    opts.set_transport(Transport::Ws)
        .set_credentials(username, token)
        .set_keep_alive(Duration::from_secs(30))
        .set_max_packet_size(Some(CLIENT_MAX_PACKET));
    opts
}

/// What the poller task forwards: an incoming packet, or the error that
/// ended the connection (after which the poller stops, so it never
/// reconnects behind a test's back).
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "a test channel item; boxing Packet would only add derefs to every match"
)]
enum Ev {
    Packet(Packet),
    Closed(ConnectionError),
}

/// One connected MQTT 5 client whose event loop runs on its own task.
#[derive(Debug)]
struct Client {
    client: AsyncClient,
    events: mpsc::Receiver<Ev>,
    /// Publishes that arrived while a call waited for an ack.
    pending: VecDeque<Publish>,
    poller: JoinHandle<()>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.poller.abort();
    }
}

/// Connects and waits up to 10 s for CONNACK. A refused CONNACK comes back
/// as `Err(ConnectionError::ConnectionRefused(code))`.
#[expect(
    clippy::result_large_err,
    reason = "returns rumqttc's own error as is, so tests can match the CONNACK code"
)]
async fn connect(opts: MqttOptions) -> Result<(Client, ConnAck), ConnectionError> {
    let (client, mut eventloop) = AsyncClient::new(opts, 64);
    let connack = loop {
        let event = timeout(ACK_WAIT, eventloop.poll()).await??;
        if let Event::Incoming(Packet::ConnAck(ack)) = event {
            break ack;
        }
    };
    let (tx, events) = mpsc::channel(1024);
    let poller = tokio::spawn(async move {
        loop {
            let ev = match eventloop.poll().await {
                Ok(Event::Incoming(packet)) => Ev::Packet(packet),
                Ok(Event::Outgoing(_)) => continue,
                Err(e) => Ev::Closed(e),
            };
            let closed = matches!(ev, Ev::Closed(_));
            if tx.send(ev).await.is_err() || closed {
                return;
            }
        }
    });
    let client = Client {
        client,
        events,
        pending: VecDeque::new(),
        poller,
    };
    Ok((client, connack))
}

async fn connect_ok(opts: MqttOptions) -> Client {
    let id = opts.client_id();
    match connect(opts).await {
        Ok((client, _)) => client,
        Err(e) => panic!("{id} should connect: {e:?}"),
    }
}

/// Asserts that EMQX refuses the CONNECT with 0x86 (bad username or
/// password) or 0x87 (not authorized).
async fn assert_refused(opts: MqttOptions) {
    let id = format!("{:?}", opts.client_id());
    match connect(opts).await {
        Err(ConnectionError::ConnectionRefused(
            ConnectReturnCode::BadUserNamePassword | ConnectReturnCode::NotAuthorized,
        )) => {}
        Ok(_) => panic!("client id {id} was accepted"),
        Err(e) => panic!("client id {id}: expected CONNACK 0x86 or 0x87, got {e:?}"),
    }
}

impl Client {
    async fn next(&mut self, within: Duration) -> Option<Ev> {
        timeout(within, self.events.recv()).await.ok().flatten()
    }

    /// Next non-publish packet; publishes in between go to `pending`.
    async fn next_control(&mut self) -> Packet {
        loop {
            match self.next(ACK_WAIT).await {
                Some(Ev::Packet(Packet::Publish(p))) => self.pending.push_back(p),
                Some(Ev::Packet(packet)) => return packet,
                Some(Ev::Closed(e)) => panic!("connection closed while waiting for an ack: {e:?}"),
                None => panic!("no ack within 10 s"),
            }
        }
    }

    /// Publishes at QoS 1 and returns the PUBACK reason.
    async fn publish(&mut self, topic: &str, payload: Vec<u8>, retain: bool) -> PubAckReason {
        self.client
            .publish(topic, QoS::AtLeastOnce, retain, payload)
            .await
            .unwrap();
        loop {
            if let Packet::PubAck(ack) = self.next_control().await {
                return ack.reason;
            }
        }
    }

    /// Subscribes at QoS 1 and returns the SUBACK reason.
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

    /// The next publish on `topic` within `within`. Publishes on other
    /// topics (other tests' traffic through a hub subscription) are dropped.
    async fn recv_on(&mut self, topic: &str, within: Duration) -> Option<Publish> {
        if let Some(i) = self
            .pending
            .iter()
            .position(|p| p.topic == topic.as_bytes())
        {
            return self.pending.remove(i);
        }
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match timeout_at(deadline, self.events.recv()).await {
                Ok(Some(Ev::Packet(Packet::Publish(p)))) if p.topic == topic.as_bytes() => {
                    return Some(p);
                }
                Ok(Some(Ev::Packet(_))) => {}
                Ok(Some(Ev::Closed(e))) => {
                    panic!("connection closed while waiting on {topic}: {e:?}")
                }
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// Waits for the broker to end the connection. Returns the DISCONNECT
    /// reason if EMQX sent one (rumqttc reports it as an error), `None` if
    /// it only closed the socket.
    async fn closed_within(
        &mut self,
        within: Duration,
    ) -> Result<Option<DisconnectReasonCode>, &'static str> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match timeout_at(deadline, self.events.recv()).await {
                Ok(Some(Ev::Closed(ConnectionError::MqttState(
                    StateError::ServerDisconnect { reason_code, .. },
                )))) => return Ok(Some(reason_code)),
                Ok(Some(Ev::Packet(_))) => {}
                Ok(Some(Ev::Closed(_)) | None) => return Ok(None),
                Err(_) => return Err("still connected"),
            }
        }
    }
}

/// A client with the hub's token in its own shared subscription group, so
/// parallel tests each get every `up/` message.
async fn hub_subscriber(group: &str) -> Client {
    let client_id = format!("{HUB_USER}-{}", new_id());
    let token = sign(&hub_claims(now_secs() + 3600));
    let mut hub = connect_ok(tcp_options(&client_id, HUB_USER, &token)).await;
    let filter = format!("$share/{group}/agmen/v1/+/+/up/+");
    assert_eq!(
        hub.subscribe(&filter).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    hub
}

fn share_group() -> String {
    format!("hub{}", new_id())
}

/// A connects as `<A>-mcp` with `opts_for(client_id, username, token)`,
/// publishes to its own `up/`, and a hub subscriber receives it.
async fn own_up_round_trip(opts_for: impl Fn(&str, &str, &str) -> MqttOptions) {
    let mut hub = hub_subscriber(&share_group()).await;
    let a = new_id();
    let mut client = connect_ok(opts_for(&format!("{a}-mcp"), &a, &identity_token(&a))).await;
    assert_eq!(
        client
            .publish(&up_topic(&a), b"hello".to_vec(), false)
            .await,
        PubAckReason::Success
    );
    let got = hub
        .recv_on(&up_topic(&a), Duration::from_secs(5))
        .await
        .expect("hub got nothing");
    assert_eq!(got.payload.as_ref(), b"hello");
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn it_publish_to_own_up_allowed() {
    own_up_round_trip(tcp_options).await;
}

#[tokio::test]
async fn it_publish_to_other_up_refused() {
    let mut hub = hub_subscriber(&share_group()).await;
    let (a, b) = (new_id(), new_id());
    let mut client = connect_ok(tcp_options(&format!("{a}-mcp"), &a, &identity_token(&a))).await;
    assert_eq!(
        client
            .publish(&up_topic(&b), b"spoof".to_vec(), false)
            .await,
        PubAckReason::NotAuthorized
    );
    assert!(
        hub.recv_on(&up_topic(&b), Duration::from_secs(2))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn it_subscribe_other_tenant_refused() {
    let a = new_id();
    let mut client = connect_ok(tcp_options(&format!("{a}-watch"), &a, &identity_token(&a))).await;
    // Control: A's own feed is allowed, so the refusal below is the ACL.
    assert_eq!(
        client
            .subscribe(&format!("agmen/v1/{TENANT}/{PROJECT}/feed"))
            .await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    assert_eq!(
        client.subscribe("agmen/v1/t2/p1/feed").await,
        SubscribeReasonCode::NotAuthorized
    );
}

#[tokio::test]
async fn it_no_acl_claim_is_denied() {
    let a = new_id();
    let token = sign(&json!({ "sub": a, "username": a, "exp": now_secs() + 3600 }));
    let mut client = connect_ok(tcp_options(&format!("{a}-mcp"), &a, &token)).await;
    let base = format!("agmen/v1/{TENANT}/{PROJECT}");
    for topic in [
        up_topic(&a),
        format!("{base}/presence/{a}"),
        format!("{base}/feed"),
        "other".to_owned(),
    ] {
        for retain in [false, true] {
            assert_eq!(
                client.publish(&topic, b"x".to_vec(), retain).await,
                PubAckReason::NotAuthorized,
                "{topic}"
            );
        }
    }
    for filter in [
        format!("{base}/feed"),
        format!("{base}/inbox/{a}"),
        format!("{base}/ack/{a}"),
        format!("{base}/state/#"),
        format!("{base}/presence/+"),
        "#".to_owned(),
    ] {
        assert_eq!(
            client.subscribe(&filter).await,
            SubscribeReasonCode::NotAuthorized,
            "{filter}"
        );
    }
}

#[tokio::test]
async fn it_bad_signature_refused() {
    let a = new_id();
    let token = sign_with(UNTRUSTED_KEY, &identity_claims(&a, now_secs() + 3600));
    assert_refused(tcp_options(&format!("{a}-mcp"), &a, &token)).await;
    // Control: the same claims signed with the dev key connect.
    connect_ok(tcp_options(&format!("{a}-mcp"), &a, &identity_token(&a))).await;
}

#[tokio::test]
async fn it_expiry_disconnects() {
    let a = new_id();
    let exp = now_secs() + 5;
    let mut client = connect_ok(tcp_options(
        &format!("{a}-mcp"),
        &a,
        &sign(&identity_claims(&a, exp)),
    ))
    .await;
    let reason = client
        .closed_within(Duration::from_secs(10))
        .await
        .expect("session outlived exp");
    let now = now_secs();
    assert!(now >= exp, "dropped at {now}, before exp {exp}");
    assert_eq!(reason, Some(DisconnectReasonCode::NotAuthorized));
}

#[tokio::test]
async fn it_rest_kick_disconnects() {
    let a = new_id();
    let client_id = format!("{a}-mcp");
    let mut client = connect_ok(tcp_options(&client_id, &a, &identity_token(&a))).await;
    let http = reqwest::Client::new();
    let url = format!("{}/clients/{client_id}", env("AGMEN_IT_EMQX_API"));
    let (key, secret) = (
        env("AGMEN_IT_EMQX_API_KEY"),
        env("AGMEN_IT_EMQX_API_SECRET"),
    );
    let kicked = http
        .delete(&url)
        .basic_auth(&key, Some(&secret))
        .send()
        .await
        .unwrap();
    assert_eq!(kicked.status(), reqwest::StatusCode::NO_CONTENT);
    let reason = client
        .closed_within(Duration::from_secs(2))
        .await
        .expect("client still connected 2 s after kick");
    assert_eq!(reason, Some(DisconnectReasonCode::AdministrativeAction));
    let gone = http
        .get(&url)
        .basic_auth(&key, Some(&secret))
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), reqwest::StatusCode::NOT_FOUND);
    // Kicking a client that is not connected is a 404, not an error.
    let again = http
        .delete(&url)
        .basic_auth(&key, Some(&secret))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn it_hub_shared_subscription() {
    let group = share_group();
    let mut hubs = [hub_subscriber(&group).await, hub_subscriber(&group).await];
    let a = new_id();
    let mut client = connect_ok(tcp_options(&format!("{a}-mcp"), &a, &identity_token(&a))).await;
    for n in 0..100 {
        assert_eq!(
            client
                .publish(&up_topic(&a), format!("{n}").into_bytes(), false)
                .await,
            PubAckReason::Success
        );
    }
    let mut seen = HashSet::new();
    let mut per_hub = [0_u32; 2];
    for (hub, count) in hubs.iter_mut().zip(per_hub.iter_mut()) {
        while let Some(p) = hub.recv_on(&up_topic(&a), Duration::from_secs(2)).await {
            assert!(seen.insert(p.payload), "duplicate delivery");
            *count += 1;
        }
    }
    assert_eq!(seen.len(), 100, "split {per_hub:?}");
    assert!(
        per_hub.iter().all(|&n| n > 0),
        "one hub got everything: {per_hub:?}"
    );
}

#[tokio::test]
async fn it_packet_over_limit_refused() {
    let mut hub = hub_subscriber(&share_group()).await;
    let a = new_id();
    let token = identity_token(&a);
    let mut client = connect_ok(tcp_options(&format!("{a}-mcp"), &a, &token)).await;
    assert_eq!(
        client
            .publish(&up_topic(&a), vec![b'x'; 60 * 1024], false)
            .await,
        PubAckReason::Success
    );
    let got = hub
        .recv_on(&up_topic(&a), Duration::from_secs(5))
        .await
        .expect("60 KiB never arrived");
    assert_eq!(got.payload.len(), 60 * 1024);

    // rumqttc refuses locally, before sending, once CONNACK has told it the
    // broker's limit.
    client
        .client
        .publish(
            up_topic(&a),
            QoS::AtLeastOnce,
            false,
            vec![b'x'; 100 * 1024],
        )
        .await
        .unwrap();
    match client.next(ACK_WAIT).await {
        Some(Ev::Closed(ConnectionError::MqttState(StateError::Deserialization(
            MqttBytesError::OutgoingPacketTooLarge { .. },
        )))) => {}
        other => panic!("expected a local OutgoingPacketTooLarge, got {other:?}"),
    }

    // A raw client skips that check, so EMQX itself must refuse the packet.
    // EMQX 6.3.2 closes the socket (log: `{shutdown,emsgsize}`) without a
    // DISCONNECT 0x95 and without a PUBACK.
    let reason = raw_publish(&format!("{a}-cli"), &a, &token, &up_topic(&a), 100 * 1024).await;
    assert_eq!(reason, None);
    assert!(
        hub.recv_on(&up_topic(&a), Duration::from_secs(2))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn it_websocket_listener() {
    let url = env("AGMEN_IT_MQTT_WS");
    own_up_round_trip(|client_id, user, token| ws_options(&url, client_id, user, token)).await;
}

/// The WSS-behind-HAProxy check (m0-m1 Task 2 Step 4): TLS ends at the
/// haproxy service, which forwards plain WebSocket to EMQX's 8083. A raw
/// client, because rumqttc's WSS transport needs its TLS features, whose
/// dependencies fail `cargo deny` (ADR-0002).
#[tokio::test]
async fn it_wss_behind_haproxy() {
    let mut hub = hub_subscriber(&share_group()).await;
    let a = new_id();
    let reason = wss_publish(&format!("{a}-mcp"), &a, &identity_token(&a), &up_topic(&a)).await;
    assert_eq!(reason, PubAckReason::Success);
    let got = hub
        .recv_on(&up_topic(&a), Duration::from_secs(5))
        .await
        .expect("hub got nothing");
    assert_eq!(got.payload.as_ref(), b"hello");
}

/// R4: one token binds to `<sub>-mcp`, `<sub>-watch` and `<sub>-cli`, and
/// to no other client id.
#[tokio::test]
async fn it_client_ids_bound_to_identity() {
    let (a, b) = (new_id(), new_id());
    let token = identity_token(&a);
    let mut hub = hub_subscriber(&share_group()).await;
    let suffixes = ["mcp", "watch", "cli"];
    let mut live = Vec::new();
    for suffix in suffixes {
        live.push(connect_ok(tcp_options(&format!("{a}-{suffix}"), &a, &token)).await);
    }
    // All three stay up together: none took over another's session.
    for (client, suffix) in live.iter_mut().zip(suffixes) {
        assert_eq!(
            client.publish(&up_topic(&a), suffix.into(), false).await,
            PubAckReason::Success
        );
        let got = hub.recv_on(&up_topic(&a), Duration::from_secs(5)).await;
        assert_eq!(got.map(|p| p.payload), Some(suffix.into()));
    }
    let wrong_ids = [
        format!("{b}-mcp"),
        a.clone(),
        format!("{a}-"),
        format!("{a}-mcpx"),
        format!("{a}-mcp-watch"),
        format!("{a}-MCP"),
        format!("x{a}-mcp"),
        format!("{HUB_USER}-{}", new_id()),
    ];
    for client_id in &wrong_ids {
        assert_refused(tcp_options(client_id, &a, &token)).await;
    }
    // EMQX drops a CONNECT whose client id ends in a newline before
    // authentication runs, without a CONNACK.
    match connect(tcp_options(&format!("{a}-mcp\n"), &a, &token)).await {
        Err(ConnectionError::MqttState(StateError::ConnectionAborted)) => {}
        Ok(_) => panic!("client id with a newline was accepted"),
        Err(e) => panic!("client id with a newline: expected the socket closed, got {e:?}"),
    }
    // Username and sub must match too: A's token under B's name is refused.
    assert_refused(tcp_options(&format!("{b}-mcp"), &b, &token)).await;
    // An identity token cannot pose as the hub, and the hub's token cannot
    // use an identity's client id.
    assert_refused(tcp_options(
        &format!("{HUB_USER}-{}", new_id()),
        HUB_USER,
        &token,
    ))
    .await;
    let hub_token = sign(&hub_claims(now_secs() + 3600));
    assert_refused(tcp_options(&format!("{a}-mcp"), HUB_USER, &hub_token)).await;
    assert_refused(tcp_options(&format!("{a}-mcp"), &a, &hub_token)).await;
}

/// R3: what the hub's own token may do besides its shared subscription.
#[tokio::test]
async fn it_hub_token_scope() {
    let a = new_id();
    let base = format!("agmen/v1/{TENANT}/{PROJECT}");
    let mut watch = connect_ok(tcp_options(&format!("{a}-watch"), &a, &identity_token(&a))).await;
    assert_eq!(
        watch.subscribe(&format!("{base}/inbox/{a}")).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    let mut hub = hub_subscriber(&share_group()).await;
    let inbox = format!("{base}/inbox/{a}");
    assert_eq!(
        hub.publish(&inbox, b"dm".to_vec(), false).await,
        PubAckReason::Success
    );
    assert!(
        watch
            .recv_on(&inbox, Duration::from_secs(5))
            .await
            .is_some(),
        "inbox message lost"
    );
    assert_eq!(
        hub.publish(&up_topic(&a), b"spoof".to_vec(), false).await,
        PubAckReason::NotAuthorized
    );
    assert_eq!(
        hub.publish("elsewhere/x", b"x".to_vec(), false).await,
        PubAckReason::NotAuthorized
    );
    assert_eq!(
        hub.subscribe(&format!("{base}/inbox/+")).await,
        SubscribeReasonCode::NotAuthorized
    );
}

/// Runs only under `--profile it-restart`: it restarts the dev EMQX.
#[tokio::test]
async fn it_persistent_session_survives_restart() {
    let a = new_id();
    let base = format!("agmen/v1/{TENANT}/{PROJECT}");
    let inbox = format!("{base}/inbox/{a}");
    let token = identity_token(&a);
    let persistent = || {
        let mut opts = tcp_options(&format!("{a}-watch"), &a, &token);
        opts.set_clean_start(false)
            .set_session_expiry_interval(Some(86_400));
        opts
    };
    let mut watch = connect_ok(persistent()).await;
    assert_eq!(
        watch.subscribe(&inbox).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    watch.client.disconnect().await.unwrap();
    assert!(watch.closed_within(Duration::from_secs(5)).await.is_ok());
    drop(watch);

    let mut hub = hub_subscriber(&share_group()).await;
    for n in 0..3 {
        assert_eq!(
            hub.publish(&inbox, format!("queued {n}").into_bytes(), false)
                .await,
            PubAckReason::Success
        );
    }
    drop(hub);

    restart_emqx().await;

    let (mut watch, connack) = reconnect_after_restart(persistent).await;
    assert!(
        connack.session_present,
        "EMQX lost the session in the restart"
    );
    for n in 0..3 {
        let got = watch.recv_on(&inbox, Duration::from_secs(10)).await;
        assert_eq!(
            got.map(|p| p.payload),
            Some(format!("queued {n}").into()),
            "message {n}"
        );
    }
}

// ---------------------------------------------------------------- helpers

/// Sends CONNECT and one QoS 1 PUBLISH of `size` bytes over a raw socket,
/// without rumqttc's client-side size check. Returns the DISCONNECT reason
/// EMQX sends, or `None` if it closes without one.
async fn raw_publish(
    client_id: &str,
    username: &str,
    token: &str,
    topic: &str,
    size: usize,
) -> Option<DisconnectReasonCode> {
    let mut stream = TcpStream::connect(mqtt_addr()).await.unwrap();
    stream
        .write_all(&encode(connect_packet(client_id, username, token)))
        .await
        .unwrap();
    let mut inbuf = BytesMut::new();
    match read_packet(&mut stream, &mut inbuf).await {
        Some(Packet::ConnAck(ack)) => assert_eq!(ack.code, ConnectReturnCode::Success),
        other => panic!("expected CONNACK, got {other:?}"),
    }
    stream
        .write_all(&encode(publish_packet(topic, vec![b'x'; size])))
        .await
        .unwrap();
    match timeout(ACK_WAIT, read_packet(&mut stream, &mut inbuf))
        .await
        .expect("EMQX neither answered nor closed")
    {
        Some(Packet::Disconnect(d)) => Some(d.reason_code),
        None => None,
        Some(other) => panic!("expected DISCONNECT or close, got {other:?}"),
    }
}

fn connect_packet(client_id: &str, username: &str, token: &str) -> Packet {
    let connect = Connect {
        keep_alive: 30,
        client_id: client_id.to_owned(),
        clean_start: true,
        properties: None,
    };
    let login = Login {
        username: username.to_owned(),
        password: token.to_owned(),
    };
    Packet::Connect(connect, None, Some(login))
}

/// A QoS 1 PUBLISH with packet id 1.
fn publish_packet(topic: &str, payload: Vec<u8>) -> Packet {
    let mut publish = Publish::new(topic, QoS::AtLeastOnce, payload, None);
    publish.pkid = 1;
    Packet::Publish(publish)
}

/// Encodes without a size limit, so a packet over the broker's limit is
/// still sent.
fn encode(packet: Packet) -> BytesMut {
    let mut out = BytesMut::new();
    packet.write(&mut out, None).unwrap();
    out
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
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
}

/// Sends CONNECT and one QoS 1 PUBLISH of `hello` to `topic` as MQTT over
/// WebSocket over TLS to `AGMEN_IT_MQTT_WSS`, trusting only the dev CA in
/// `AGMEN_IT_WSS_CA`. Returns the PUBACK reason.
async fn wss_publish(client_id: &str, username: &str, token: &str, topic: &str) -> PubAckReason {
    let url = env("AGMEN_IT_MQTT_WSS");
    let authority = url
        .strip_prefix("wss://")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_else(|| panic!("AGMEN_IT_MQTT_WSS must be wss://host:port/path, got {url}"));
    let (host, port) = authority.rsplit_once(':').unwrap();
    install_crypto_provider();
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(&std::fs::read(env("AGMEN_IT_WSS_CA")).unwrap()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = TcpStream::connect((host, port.parse::<u16>().unwrap()))
        .await
        .unwrap();
    let tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(host.to_owned()).unwrap(), tcp)
        .await
        .unwrap();
    let mut request = url.as_str().into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("mqtt"));
    let (mut ws, _) = async_tungstenite::tokio::client_async(request, tls)
        .await
        .unwrap();
    let mut inbuf = BytesMut::new();
    ws.send(Message::binary(
        encode(connect_packet(client_id, username, token)).freeze(),
    ))
    .await
    .unwrap();
    match ws_read_packet(&mut ws, &mut inbuf).await {
        Some(Packet::ConnAck(ack)) => assert_eq!(ack.code, ConnectReturnCode::Success),
        other => panic!("expected CONNACK over WSS, got {other:?}"),
    }
    ws.send(Message::binary(
        encode(publish_packet(topic, b"hello".to_vec())).freeze(),
    ))
    .await
    .unwrap();
    match ws_read_packet(&mut ws, &mut inbuf).await {
        Some(Packet::PubAck(ack)) => ack.reason,
        other => panic!("expected PUBACK over WSS, got {other:?}"),
    }
}

/// Reads one packet from WebSocket binary frames; `None` when the stream
/// ends first.
async fn ws_read_packet(
    ws: &mut (impl Stream<Item = Result<Message, WsError>> + Unpin),
    buf: &mut BytesMut,
) -> Option<Packet> {
    loop {
        match Packet::read(buf, None) {
            Ok(packet) => return Some(packet),
            Err(MqttBytesError::InsufficientBytes(_)) => {}
            Err(e) => panic!("bad packet from EMQX: {e:?}"),
        }
        match timeout(ACK_WAIT, ws.next())
            .await
            .expect("no answer within 10 s")
        {
            Some(Ok(Message::Binary(data))) => buf.extend_from_slice(&data),
            Some(Ok(_)) => {}
            Some(Err(e)) => panic!("WebSocket error: {e:?}"),
            None => return None,
        }
    }
}

/// Reads one packet; `None` when the socket closes first.
async fn read_packet(stream: &mut TcpStream, buf: &mut BytesMut) -> Option<Packet> {
    loop {
        match Packet::read(buf, None) {
            Ok(packet) => return Some(packet),
            Err(MqttBytesError::InsufficientBytes(_)) => {}
            Err(e) => panic!("bad packet from EMQX: {e:?}"),
        }
        if stream.read_buf(buf).await.unwrap() == 0 {
            return None;
        }
    }
}

async fn restart_emqx() {
    let compose = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/dev/compose.yaml");
    let status = tokio::process::Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(&compose)
        .args(["restart", "emqx"])
        .status()
        .await
        .expect("run docker compose");
    assert!(status.success(), "docker compose restart emqx: {status}");
}

/// Retries the CONNECT once a second for up to 90 s while EMQX starts and
/// loads the JWKS. A refused attempt leaves the stored session alone.
async fn reconnect_after_restart(opts: impl Fn() -> MqttOptions) -> (Client, ConnAck) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match connect(opts()).await {
            Ok(connected) => return connected,
            Err(_) if Instant::now() < deadline => sleep(Duration::from_secs(1)).await,
            Err(e) => panic!("EMQX not accepting connections 90 s after restart: {e:?}"),
        }
    }
}
