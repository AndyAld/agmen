//! Spike S1: EMQX JWT identity and ACL behavior against the dev stack.
//!
//! Run with `cargo nextest run -p s1-emqx --profile it` after
//! `deploy/dev/init.sh` and `docker compose -f deploy/dev/compose.yaml up -d --wait`,
//! with `deploy/dev/.secrets/dev.env` exported. Results go into ADR-0002.

// Test helpers fail the test on any setup error; clippy's in-tests allowances
// cover only #[test] functions, not the helpers they call.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::HashSet;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rumqttc::Transport;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{ConnectReturnCode, Packet, PubAckReason, SubscribeReasonCode};
use rumqttc::v5::{AsyncClient, ConnectionError, Event, MqttOptions};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, timeout};

// ---------------------------------------------------------------------------
// Environment and tokens

struct ItEnv {
    mqtt_host: String,
    mqtt_port: u16,
    ws_url: String,
    wss_url: String,
    dev_ca: Vec<u8>,
    api: String,
    api_key: String,
    api_secret: String,
    jwt_pem: Vec<u8>,
    kid: String,
    compose_file: String,
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is unset: run deploy/dev/init.sh and export deploy/dev/.secrets/dev.env")
    })
}

fn it_env() -> ItEnv {
    let mqtt = var("AGMEN_IT_MQTT");
    let hostport = mqtt.trim_start_matches("mqtt://");
    let (host, port) = hostport
        .split_once(':')
        .expect("AGMEN_IT_MQTT is mqtt://host:port");
    ItEnv {
        mqtt_host: host.to_owned(),
        mqtt_port: port.parse().expect("port"),
        ws_url: var("AGMEN_IT_MQTT_WS"),
        wss_url: var("AGMEN_IT_MQTT_WSS"),
        dev_ca: std::fs::read(var("AGMEN_IT_DEV_CA")).expect("read AGMEN_IT_DEV_CA"),
        api: var("AGMEN_IT_EMQX_API"),
        api_key: var("AGMEN_IT_EMQX_API_KEY"),
        api_secret: var("AGMEN_IT_EMQX_API_SECRET"),
        jwt_pem: std::fs::read(var("AGMEN_IT_JWT_KEY")).expect("read AGMEN_IT_JWT_KEY"),
        kid: var("AGMEN_IT_JWT_KID"),
        compose_file: var("AGMEN_IT_COMPOSE_FILE"),
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// A fresh id in the protocol alphabet `[a-z0-9]{8,32}`.
fn fresh_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    format!("{prefix}{:x}", nanos % (1 << 60))
}

/// Tenant and project for one test, so tests never see each other's traffic.
struct Scope {
    t: String,
    p: String,
}

impl Scope {
    fn new() -> Self {
        Scope {
            t: fresh_id("t"),
            p: fresh_id("p"),
        }
    }
    fn topic(&self, rest: &str) -> String {
        format!("agmen/v1/{}/{}/{rest}", self.t, self.p)
    }
}

/// The ACL an identity gets: its own topics in full with `eq`, then deny-all.
fn identity_acl(s: &Scope, id: &str) -> Value {
    json!([
        { "permission": "allow", "action": "publish",   "topic": format!("eq {}", s.topic(&format!("up/{id}"))), "qos": [1] },
        { "permission": "allow", "action": "publish",   "topic": format!("eq {}", s.topic(&format!("presence/{id}"))), "retain": true },
        { "permission": "allow", "action": "subscribe", "topic": format!("eq {}", s.topic("feed")) },
        { "permission": "allow", "action": "subscribe", "topic": format!("eq {}", s.topic(&format!("inbox/{id}"))) },
        { "permission": "allow", "action": "subscribe", "topic": format!("eq {}", s.topic(&format!("ack/{id}"))) },
        { "permission": "allow", "action": "subscribe", "topic": s.topic("state/#") },
        { "permission": "allow", "action": "subscribe", "topic": s.topic("presence/+") },
        { "permission": "deny",  "action": "all",       "topic": "#" }
    ])
}

/// The hub's ACL: read every `up/`, publish everywhere else under `agmen/v1/`.
fn hub_acl() -> Value {
    json!([
        { "permission": "allow", "action": "subscribe", "topic": "agmen/v1/+/+/up/+" },
        { "permission": "deny",  "action": "publish",   "topic": "agmen/v1/+/+/up/#" },
        { "permission": "allow", "action": "publish",   "topic": "agmen/v1/#" },
        { "permission": "deny",  "action": "all",       "topic": "#" }
    ])
}

fn sign(pem: &[u8], kid: &str, claims: &Value) -> String {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(kid.to_owned());
    let key = EncodingKey::from_ec_pem(pem).expect("EC key");
    jsonwebtoken::encode(&header, claims, &key).expect("sign")
}

fn token(env: &ItEnv, sub: &str, clientid: &str, acl: Option<Value>, ttl_s: u64) -> String {
    let mut claims =
        json!({ "sub": sub, "clientid": clientid, "iat": now(), "exp": now() + ttl_s });
    if let Some(acl) = acl {
        claims["acl"] = acl;
    }
    sign(&env.jwt_pem, &env.kid, &claims)
}

// ---------------------------------------------------------------------------
// MQTT connections

struct Conn {
    client: AsyncClient,
    rx: mpsc::UnboundedReceiver<Result<Event, String>>,
    session_present: bool,
}

struct ConnOpts {
    clean_start: bool,
    session_expiry: Option<u32>,
    websocket: bool,
    /// WSS through the HAProxy TLS front, as on the cluster.
    wss: bool,
}

impl Default for ConnOpts {
    fn default() -> Self {
        ConnOpts {
            clean_start: true,
            session_expiry: None,
            websocket: false,
            wss: false,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // fields are read through Debug in assertion messages
enum ConnectFailure {
    Refused(ConnectReturnCode),
    Other(String),
}

async fn connect(
    env: &ItEnv,
    clientid: &str,
    username: &str,
    password: &str,
    o: ConnOpts,
) -> Result<Conn, ConnectFailure> {
    let mut opts = if o.wss {
        let port = env
            .wss_url
            .rsplit(':')
            .next()
            .and_then(|p| p.split('/').next())
            .and_then(|p| p.parse().ok())
            .expect("port in AGMEN_IT_MQTT_WSS");
        let mut m = MqttOptions::new(clientid, env.wss_url.clone(), port);
        m.set_transport(Transport::Wss(rumqttc::TlsConfiguration::Simple {
            ca: env.dev_ca.clone(),
            alpn: None,
            client_auth: None,
        }));
        m
    } else if o.websocket {
        let mut m = MqttOptions::new(clientid, env.ws_url.clone(), 8083);
        m.set_transport(Transport::Ws);
        m
    } else {
        MqttOptions::new(clientid, env.mqtt_host.clone(), env.mqtt_port)
    };
    opts.set_credentials(username, password);
    opts.set_keep_alive(Duration::from_secs(10));
    opts.set_clean_start(o.clean_start);
    opts.set_session_expiry_interval(o.session_expiry);
    opts.set_max_packet_size(Some(256 * 1024));

    let (client, mut eventloop) = AsyncClient::new(opts, 256);
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(ev) => {
                    if tx.send(Ok(ev)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let msg = match &e {
                        ConnectionError::ConnectionRefused(code) => format!("refused:{code:?}"),
                        other => other.to_string(),
                    };
                    let _ = tx.send(Err(msg));
                    break;
                }
            }
        }
    });

    let first = timeout(Duration::from_secs(10), async {
        while let Some(ev) = rx.recv().await {
            match ev {
                Ok(Event::Incoming(Packet::ConnAck(ack))) => return Ok(ack.session_present),
                Ok(_) => continue,
                Err(e) => return Err(e),
            }
        }
        Err("event loop ended".to_owned())
    })
    .await;

    match first {
        Ok(Ok(session_present)) => Ok(Conn {
            client,
            rx,
            session_present,
        }),
        Ok(Err(e)) => Err(refusal(&e)),
        Err(_) => Err(ConnectFailure::Other("no CONNACK within 10 s".to_owned())),
    }
}

fn refusal(e: &str) -> ConnectFailure {
    // ConnectionRefused carries the code; match on its Debug name.
    for code in [
        ConnectReturnCode::BadUserNamePassword,
        ConnectReturnCode::NotAuthorized,
        ConnectReturnCode::Banned,
        ConnectReturnCode::ClientIdentifierNotValid,
    ] {
        if e == format!("refused:{code:?}") {
            return ConnectFailure::Refused(code);
        }
    }
    ConnectFailure::Other(e.to_owned())
}

impl Conn {
    /// Waits for the first event `f` maps to `Some`, up to `within`.
    async fn wait_for<T>(
        &mut self,
        within: Duration,
        mut f: impl FnMut(&Event) -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match timeout(left, self.rx.recv()).await {
                Ok(Some(Ok(ev))) => {
                    if let Some(t) = f(&ev) {
                        return Some(t);
                    }
                }
                Ok(Some(Err(_))) | Ok(None) | Err(_) => return None,
            }
        }
    }

    /// Waits until the connection drops. Returns the reason, or None if it stayed up.
    async fn wait_disconnect(&mut self, within: Duration) -> Option<String> {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match timeout(left, self.rx.recv()).await {
                Ok(Some(Ok(Event::Incoming(Packet::Disconnect(d))))) => {
                    return Some(format!("DISCONNECT {:?}", d.reason_code));
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(e))) => return Some(e),
                Ok(None) => return Some("event loop ended".to_owned()),
                Err(_) => return None,
            }
        }
    }

    async fn subscribe(&mut self, filter: &str) -> SubscribeReasonCode {
        self.client
            .subscribe(filter, QoS::AtLeastOnce)
            .await
            .expect("queue subscribe");
        self.wait_for(Duration::from_secs(5), |ev| match ev {
            Event::Incoming(Packet::SubAck(ack)) => ack.return_codes.first().copied(),
            _ => None,
        })
        .await
        .expect("SUBACK within 5 s")
    }

    async fn publish(&mut self, topic: &str, payload: Vec<u8>) -> Option<PubAckReason> {
        self.client
            .publish(topic, QoS::AtLeastOnce, false, payload)
            .await
            .expect("queue publish");
        self.wait_for(Duration::from_secs(5), |ev| match ev {
            Event::Incoming(Packet::PubAck(ack)) => Some(ack.reason),
            _ => None,
        })
        .await
    }

    /// Collects publish payloads arriving within `within`.
    async fn collect(&mut self, within: Duration) -> Vec<(String, Vec<u8>)> {
        let deadline = Instant::now() + within;
        let mut got = Vec::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match timeout(left, self.rx.recv()).await {
                Ok(Some(Ok(Event::Incoming(Packet::Publish(p))))) => {
                    got.push((
                        String::from_utf8_lossy(&p.topic).into_owned(),
                        p.payload.to_vec(),
                    ));
                }
                Ok(Some(Ok(_))) => continue,
                _ => break,
            }
        }
        got
    }
}

async fn connect_identity(env: &ItEnv, s: &Scope, id: &str, suffix: &str, o: ConnOpts) -> Conn {
    let cid = format!("{id}-{suffix}");
    let t = token(env, id, &cid, Some(identity_acl(s, id)), 600);
    connect(env, &cid, id, &t, o)
        .await
        .expect("identity connects")
}

async fn connect_hub(env: &ItEnv, name: &str) -> Conn {
    let id = fresh_id(name);
    let t = token(env, &id, &id, Some(hub_acl()), 600);
    connect(env, &id, &id, &t, ConnOpts::default())
        .await
        .expect("hub connects")
}

fn emqx_api(env: &ItEnv) -> (reqwest::Client, String) {
    (reqwest::Client::new(), env.api.clone())
}

// ---------------------------------------------------------------------------
// Tests

#[tokio::test]
async fn it_publish_to_own_up_allowed() {
    let env = it_env();
    let s = Scope::new();
    let mut hub = connect_hub(&env, "hub").await;
    assert_eq!(
        hub.subscribe(&s.topic("up/+")).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );

    let a = fresh_id("ida");
    let mut ca = connect_identity(&env, &s, &a, "mcp", ConnOpts::default()).await;
    let reason = ca
        .publish(&s.topic(&format!("up/{a}")), b"hello".to_vec())
        .await;
    assert_eq!(reason, Some(PubAckReason::Success));

    let got = hub.collect(Duration::from_secs(2)).await;
    assert_eq!(got, vec![(s.topic(&format!("up/{a}")), b"hello".to_vec())]);
}

#[tokio::test]
async fn it_publish_to_other_up_refused() {
    let env = it_env();
    let s = Scope::new();
    let mut hub = connect_hub(&env, "hub").await;
    hub.subscribe(&s.topic("up/+")).await;

    let a = fresh_id("ida");
    let b = fresh_id("idb");
    let mut ca = connect_identity(&env, &s, &a, "mcp", ConnOpts::default()).await;
    let reason = ca
        .publish(&s.topic(&format!("up/{b}")), b"spoof".to_vec())
        .await;
    assert_eq!(reason, Some(PubAckReason::NotAuthorized));
    assert!(
        hub.collect(Duration::from_secs(2)).await.is_empty(),
        "hub must receive nothing"
    );
}

#[tokio::test]
async fn it_subscribe_other_tenant_refused() {
    let env = it_env();
    let s = Scope::new();
    let other = Scope::new();
    let a = fresh_id("ida");
    let mut ca = connect_identity(&env, &s, &a, "watch", ConnOpts::default()).await;
    assert_eq!(
        ca.subscribe(&other.topic("feed")).await,
        SubscribeReasonCode::NotAuthorized
    );
}

#[tokio::test]
async fn it_wildcard_subscriptions_cannot_widen_acl() {
    let env = it_env();
    let s = Scope::new();
    let a = fresh_id("ida");
    let mut ca = connect_identity(&env, &s, &a, "watch", ConnOpts::default()).await;

    let refused = [
        "#".to_owned(),
        "agmen/#".to_owned(),
        s.topic("#"),
        s.topic("+"),
        s.topic("up/+"),
        s.topic(&format!("up/{a}")),
        s.topic("inbox/+"),
        s.topic("ack/+"),
        format!("agmen/v1/{}/+/feed", s.t),
        format!("agmen/v1/+/{}/feed", s.p),
        format!("$share/g/{}", s.topic("inbox/+")),
    ];
    for f in &refused {
        assert_eq!(
            ca.subscribe(f).await,
            SubscribeReasonCode::NotAuthorized,
            "filter {f}"
        );
    }
    let allowed = [
        s.topic("feed"),
        s.topic(&format!("inbox/{a}")),
        s.topic(&format!("ack/{a}")),
        s.topic("state/head"),
        s.topic("state/#"),
        s.topic("presence/+"),
    ];
    for f in &allowed {
        assert_eq!(
            ca.subscribe(f).await,
            SubscribeReasonCode::Success(QoS::AtLeastOnce),
            "filter {f}"
        );
    }
}

#[tokio::test]
async fn it_no_acl_claim_is_denied() {
    let env = it_env();
    let s = Scope::new();
    let a = fresh_id("ida");
    let cid = format!("{a}-mcp");
    let t = token(&env, &a, &cid, None, 600);
    let mut ca = connect(&env, &cid, &a, &t, ConnOpts::default())
        .await
        .expect("signature is valid");
    assert_eq!(
        ca.publish(&s.topic(&format!("up/{a}")), b"x".to_vec())
            .await,
        Some(PubAckReason::NotAuthorized)
    );
    assert_eq!(
        ca.subscribe(&s.topic("feed")).await,
        SubscribeReasonCode::NotAuthorized
    );
}

#[tokio::test]
async fn it_bad_signature_refused() {
    let env = it_env();
    let s = Scope::new();
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let pem_path = dir.join("s1-other-key.pem");
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "openssl ecparam -name prime256v1 -genkey -noout | openssl pkcs8 -topk8 -nocrypt -out {}",
            pem_path.display()
        ))
        .status()
        .expect("run openssl");
    assert!(status.success());
    let other_pem = std::fs::read(&pem_path).expect("read other key");

    let a = fresh_id("ida");
    let cid = format!("{a}-mcp");
    let claims = json!({ "sub": a, "clientid": cid, "iat": now(), "exp": now() + 600, "acl": identity_acl(&s, &a) });

    // Same kid, wrong key.
    let forged = sign(&other_pem, &env.kid, &claims);
    let r = connect(&env, &cid, &a, &forged, ConnOpts::default()).await;
    assert!(
        matches!(r, Err(ConnectFailure::Refused(_))),
        "forged token: {:?}",
        r.err()
    );

    // Unknown kid.
    let unknown = sign(&other_pem, "nope-1", &claims);
    let r = connect(&env, &cid, &a, &unknown, ConnOpts::default()).await;
    assert!(
        matches!(r, Err(ConnectFailure::Refused(_))),
        "unknown kid: {:?}",
        r.err()
    );

    // Expired.
    let mut old = claims.clone();
    old["iat"] = json!(now() - 1200);
    old["exp"] = json!(now() - 600);
    let expired = sign(&env.jwt_pem, &env.kid, &old);
    let r = connect(&env, &cid, &a, &expired, ConnOpts::default()).await;
    assert!(
        matches!(r, Err(ConnectFailure::Refused(_))),
        "expired token: {:?}",
        r.err()
    );
}

#[tokio::test]
async fn it_username_must_match_sub() {
    let env = it_env();
    let s = Scope::new();
    let a = fresh_id("ida");
    let b = fresh_id("idb");
    let cid = format!("{b}-mcp");
    // A's token presented with B's username and client id.
    let t = token(&env, &a, &cid, Some(identity_acl(&s, &a)), 600);
    let r = connect(&env, &cid, &b, &t, ConnOpts::default()).await;
    assert!(
        matches!(r, Err(ConnectFailure::Refused(_))),
        "{:?}",
        r.err()
    );
}

#[tokio::test]
async fn it_token_bound_to_clientid() {
    let env = it_env();
    let s = Scope::new();
    let a = fresh_id("ida");
    let t = token(
        &env,
        &a,
        &format!("{a}-mcp"),
        Some(identity_acl(&s, &a)),
        600,
    );
    let r = connect(&env, &format!("{a}-watch"), &a, &t, ConnOpts::default()).await;
    assert!(
        matches!(r, Err(ConnectFailure::Refused(_))),
        "{:?}",
        r.err()
    );
}

#[tokio::test]
async fn it_expiry_disconnects() {
    let env = it_env();
    let s = Scope::new();
    let a = fresh_id("ida");
    let cid = format!("{a}-mcp");
    let t = token(&env, &a, &cid, Some(identity_acl(&s, &a)), 5);
    let mut ca = connect(&env, &cid, &a, &t, ConnOpts::default())
        .await
        .expect("connects");
    let started = Instant::now();
    let why = ca.wait_disconnect(Duration::from_secs(15)).await;
    let after = started.elapsed();
    assert!(why.is_some(), "still connected 15 s after a 5 s token");
    eprintln!("S1 expiry: disconnected after {after:?}: {why:?}");
}

#[tokio::test]
async fn it_rest_kick_disconnects() {
    let env = it_env();
    let s = Scope::new();
    let a = fresh_id("ida");
    let mut ca = connect_identity(&env, &s, &a, "watch", ConnOpts::default()).await;

    let (http, api) = emqx_api(&env);
    let resp = http
        .delete(format!("{api}/clients/{a}-watch"))
        .basic_auth(&env.api_key, Some(&env.api_secret))
        .send()
        .await
        .expect("REST call");
    assert_eq!(
        resp.status().as_u16(),
        204,
        "kick: {}",
        resp.text().await.unwrap_or_default()
    );
    let why = ca.wait_disconnect(Duration::from_secs(2)).await;
    assert!(why.is_some(), "client still connected 2 s after the kick");
    eprintln!("S1 kick: {why:?}");
}

#[tokio::test]
async fn it_hub_shared_subscription() {
    let env = it_env();
    let s = Scope::new();
    let filter = format!("$share/hub/{}", s.topic("up/+"));
    let mut h1 = connect_hub(&env, "hubx").await;
    let mut h2 = connect_hub(&env, "huby").await;
    assert_eq!(
        h1.subscribe(&filter).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    assert_eq!(
        h2.subscribe(&filter).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );

    let a = fresh_id("ida");
    let mut ca = connect_identity(&env, &s, &a, "mcp", ConnOpts::default()).await;
    for i in 0..100u32 {
        assert_eq!(
            ca.publish(&s.topic(&format!("up/{a}")), i.to_string().into_bytes())
                .await,
            Some(PubAckReason::Success)
        );
    }
    let (g1, g2) = tokio::join!(
        h1.collect(Duration::from_secs(3)),
        h2.collect(Duration::from_secs(3))
    );
    let all: HashSet<Vec<u8>> = g1.iter().chain(g2.iter()).map(|(_, p)| p.clone()).collect();
    eprintln!(
        "S1 shared subscription: hub1 {} hub2 {}",
        g1.len(),
        g2.len()
    );
    assert_eq!(g1.len() + g2.len(), 100, "each message delivered once");
    assert_eq!(all.len(), 100, "no message lost");
    assert!(
        !g1.is_empty() && !g2.is_empty(),
        "both hub clients get a share"
    );
}

#[tokio::test]
async fn it_persistent_session_survives_restart() {
    let env = it_env();
    let s = Scope::new();
    let w = fresh_id("idw");
    let persistent = || ConnOpts {
        clean_start: false,
        session_expiry: Some(86_400),
        ..ConnOpts::default()
    };

    let mut cw = connect_identity(&env, &s, &w, "watch", persistent()).await;
    assert_eq!(
        cw.subscribe(&s.topic("feed")).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    cw.client.disconnect().await.expect("disconnect");
    let _ = cw.wait_disconnect(Duration::from_secs(2)).await;

    let mut hub = connect_hub(&env, "hub").await;
    for i in 1..=3u8 {
        assert_eq!(
            hub.publish(&s.topic("feed"), vec![b'0' + i]).await,
            Some(PubAckReason::Success)
        );
    }
    drop(hub);

    let out = Command::new("docker")
        .args(["compose", "-f", &env.compose_file, "restart", "emqx"])
        .output()
        .expect("docker compose restart");
    assert!(
        out.status.success(),
        "restart: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let restarted = Instant::now();
    wait_broker_up(&env).await;

    // Like a real client, retry until the broker takes logins again.
    let cid = format!("{w}-watch");
    let mut cw = loop {
        let t = token(&env, &w, &cid, Some(identity_acl(&s, &w)), 600);
        match connect(&env, &cid, &w, &t, persistent()).await {
            Ok(c) => break c,
            Err(e) if restarted.elapsed() < Duration::from_secs(30) => {
                eprintln!("S1 restart: retrying after {e:?}");
                sleep(Duration::from_secs(1)).await;
            }
            Err(e) => panic!("no login 30 s after restart: {e:?}"),
        }
    };
    eprintln!(
        "S1 restart: logins accepted {:?} after the restart finished",
        restarted.elapsed()
    );
    let got = cw.collect(Duration::from_secs(3)).await;
    eprintln!(
        "S1 restart: session_present={} messages={}",
        cw.session_present,
        got.len()
    );
    assert!(cw.session_present, "session survives a broker restart");
    let payloads: Vec<Vec<u8>> = got.into_iter().map(|(_, p)| p).collect();
    assert_eq!(payloads, vec![b"1".to_vec(), b"2".to_vec(), b"3".to_vec()]);
}

async fn wait_broker_up(env: &ItEnv) {
    let (http, api) = emqx_api(env);
    for _ in 0..60 {
        let ok = http
            .get(format!("{api}/status"))
            .basic_auth(&env.api_key, Some(&env.api_secret))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if ok {
            // Listeners come up just after the API.
            sleep(Duration::from_secs(2)).await;
            return;
        }
        sleep(Duration::from_secs(1)).await;
    }
    panic!("EMQX did not come back within 60 s");
}

#[tokio::test]
async fn it_packet_over_limit_refused() {
    let env = it_env();
    let s = Scope::new();
    let mut hub = connect_hub(&env, "hub").await;
    hub.subscribe(&s.topic("up/+")).await;

    let a = fresh_id("ida");
    let mut ca = connect_identity(&env, &s, &a, "mcp", ConnOpts::default()).await;
    let up = s.topic(&format!("up/{a}"));
    assert_eq!(
        ca.publish(&up, vec![b'a'; 60 * 1024]).await,
        Some(PubAckReason::Success)
    );
    let got = hub.collect(Duration::from_secs(2)).await;
    assert_eq!(got.len(), 1);

    ca.client
        .publish(&up, QoS::AtLeastOnce, false, vec![b'b'; 100 * 1024])
        .await
        .expect("queue");
    let why = ca.wait_disconnect(Duration::from_secs(5)).await;
    eprintln!("S1 oversize: {why:?}");
    assert!(why.is_some(), "a 100 KiB publish must end the connection");
    assert!(
        hub.collect(Duration::from_secs(2)).await.is_empty(),
        "the oversize message must not arrive"
    );
}

/// Sends CONNECT and an oversize PUBLISH over a raw socket, bypassing the
/// client library's own check against the broker's advertised limit.
#[tokio::test]
async fn it_broker_enforces_packet_limit() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let env = it_env();
    let s = Scope::new();
    let mut hub = connect_hub(&env, "hub").await;
    hub.subscribe(&s.topic("up/+")).await;

    let a = fresh_id("ida");
    let cid = format!("{a}-mcp");
    let t = token(&env, &a, &cid, Some(identity_acl(&s, &a)), 600);

    let mut sock = tokio::net::TcpStream::connect((env.mqtt_host.as_str(), env.mqtt_port))
        .await
        .expect("tcp");
    sock.write_all(&raw_connect(&cid, &a, &t))
        .await
        .expect("send CONNECT");
    let mut connack = [0u8; 64];
    let n = timeout(Duration::from_secs(5), sock.read(&mut connack))
        .await
        .expect("CONNACK in time")
        .expect("read");
    assert!(
        n >= 4 && connack[0] == 0x20 && connack[3] == 0x00,
        "CONNACK success, got {:02x?}",
        &connack[..n]
    );

    sock.write_all(&raw_publish_qos1(
        &s.topic(&format!("up/{a}")),
        &vec![b'z'; 100 * 1024],
    ))
    .await
    .expect("send PUBLISH");
    let mut buf = [0u8; 64];
    let reply = timeout(Duration::from_secs(5), sock.read(&mut buf))
        .await
        .expect("broker answers in time");
    match reply {
        Ok(0) => eprintln!("S1 raw oversize: broker closed the socket"),
        Ok(n) => {
            eprintln!("S1 raw oversize: broker sent {:02x?}", &buf[..n]);
            // DISCONNECT (0xe0) with reason 0x95 Packet too large.
            assert_eq!(buf[0], 0xe0, "expected DISCONNECT");
            assert_eq!(buf.get(2).copied(), Some(0x95), "expected reason 0x95");
        }
        Err(e) => eprintln!("S1 raw oversize: socket error {e}"),
    }
    assert!(
        hub.collect(Duration::from_secs(2)).await.is_empty(),
        "the oversize message must not arrive"
    );
}

fn mqtt_varint(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut b = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}

fn mqtt_str(v: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(v.len() as u16).to_be_bytes());
    out.extend_from_slice(v);
}

fn mqtt_packet(first: u8, body: Vec<u8>) -> Vec<u8> {
    let mut out = vec![first];
    mqtt_varint(body.len(), &mut out);
    out.extend(body);
    out
}

fn raw_connect(cid: &str, user: &str, pass: &str) -> Vec<u8> {
    let mut b = Vec::new();
    mqtt_str(b"MQTT", &mut b);
    b.push(5); // protocol level
    b.push(0b1100_0010); // username, password, clean start
    b.extend_from_slice(&30u16.to_be_bytes()); // keep alive
    b.push(0); // no properties
    mqtt_str(cid.as_bytes(), &mut b);
    mqtt_str(user.as_bytes(), &mut b);
    mqtt_str(pass.as_bytes(), &mut b);
    mqtt_packet(0x10, b)
}

fn raw_publish_qos1(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    mqtt_str(topic.as_bytes(), &mut b);
    b.extend_from_slice(&1u16.to_be_bytes()); // packet id
    b.push(0); // no properties
    b.extend_from_slice(payload);
    mqtt_packet(0x32, b)
}

#[tokio::test]
async fn it_websocket_listener() {
    let env = it_env();
    let s = Scope::new();
    let mut hub = connect_hub(&env, "hub").await;
    hub.subscribe(&s.topic("up/+")).await;

    let a = fresh_id("ida");
    let mut ca = connect_identity(
        &env,
        &s,
        &a,
        "mcp",
        ConnOpts {
            websocket: true,
            ..ConnOpts::default()
        },
    )
    .await;
    assert_eq!(
        ca.publish(&s.topic(&format!("up/{a}")), b"ws".to_vec())
            .await,
        Some(PubAckReason::Success)
    );
    assert_eq!(hub.collect(Duration::from_secs(2)).await.len(), 1);
}

#[tokio::test]
async fn it_wss_behind_haproxy() {
    let env = it_env();
    let s = Scope::new();
    let mut hub = connect_hub(&env, "hub").await;
    hub.subscribe(&s.topic("up/+")).await;

    let a = fresh_id("ida");
    let mut ca = connect_identity(
        &env,
        &s,
        &a,
        "mcp",
        ConnOpts {
            wss: true,
            ..ConnOpts::default()
        },
    )
    .await;
    assert_eq!(
        ca.publish(&s.topic(&format!("up/{a}")), b"wss".to_vec())
            .await,
        Some(PubAckReason::Success)
    );
    assert_eq!(hub.collect(Duration::from_secs(2)).await.len(), 1);

    // ACLs hold through the proxy too.
    let b = fresh_id("idb");
    assert_eq!(
        ca.publish(&s.topic(&format!("up/{b}")), b"spoof".to_vec())
            .await,
        Some(PubAckReason::NotAuthorized)
    );
}
