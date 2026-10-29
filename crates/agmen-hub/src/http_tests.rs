use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::get;
use sqlx::postgres::PgPoolOptions;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use url::Url;

use super::*;
use crate::auth::Github;
use crate::broker_tokens::BrokerTokens;
use crate::config::{DEFAULT_CLAIM_LEASE, EmqxConfig, GithubConfig, HubMode, HubSecrets};
use crate::emqx::EmqxAdmin;
use crate::events::Slots;
use crate::ingest::IngestState;
use crate::presence::PresenceState;
use crate::self_config;

const WAIT: Duration = Duration::from_secs(5);
/// The EMQX API key name the test hub presents; not a secret.
const EMQX_KEY_NAME: &str = "agmen-hub";
const UPGRADE_HEAD: &[u8] = b"GET /mqtt HTTP/1.1\r\nHost: hub.example\r\nUpgrade: WebSocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: mqtt\r\n\r\n";
const SWITCH: &[u8] = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\nSec-WebSocket-Protocol: mqtt\r\n\r\n";

struct RunningHub {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for RunningHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn test_hub(mqtt_ws_upstream: Option<Url>) -> Hub {
    let dir = tempfile::tempdir().unwrap();
    let loaded = self_config::load(
        &HubSecrets::Generated {
            data_dir: dir.path().to_owned(),
        },
        "agmen-hub",
    )
    .unwrap();
    let tokens = BrokerTokens::new(loaded.signing, vec![]).unwrap();
    let http = upstream_client().unwrap();
    let emqx = EmqxAdmin::new(
        &EmqxConfig {
            api_url: Url::parse("http://127.0.0.1:1/api/v5").unwrap(),
            api_key: EMQX_KEY_NAME.to_owned(),
        },
        loaded.emqx_api_secret,
        http.clone(),
    )
    .unwrap();
    let github = Github::new(
        &GithubConfig {
            client_id: "Iv1.test".to_owned(),
            url: Url::parse("http://127.0.0.1:1").unwrap(),
            api_url: Url::parse("http://127.0.0.1:1").unwrap(),
        },
        http,
    )
    .unwrap();
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://agmen@127.0.0.1:1/agmen")
        .unwrap();
    Hub {
        inner: Arc::new(Inner {
            mode: HubMode::SelfHost,
            broker_up: AtomicBool::new(false),
            pool,
            tokens,
            emqx,
            github,
            paddle: None,
            public_url: Url::parse("https://hub.example").unwrap(),
            public_mqtt_url: Url::parse("wss://hub.example/mqtt").unwrap(),
            mqtt_ws_upstream,
            ingest: IngestState::default(),
            presence: PresenceState::default(),
            events: Slots::default(),
            claim_lease: DEFAULT_CLAIM_LEASE,
            retention_days: None,
        }),
    }
}

async fn start_hub(header_read_timeout: Duration, mqtt_ws_upstream: Option<Url>) -> RunningHub {
    let hub = test_hub(mqtt_ws_upstream);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = hub.router();
    let task = tokio::spawn(async move {
        serve(
            listener,
            router,
            header_read_timeout,
            std::future::pending(),
        )
        .await;
    });
    RunningHub { addr, task }
}

async fn read_head(stream: &mut TcpStream) -> Vec<u8> {
    timeout(WAIT, async {
        let mut head = Vec::new();
        for _ in 0..8192 {
            head.push(stream.read_u8().await.unwrap());
            if head.ends_with(b"\r\n\r\n") {
                return head;
            }
        }
        panic!("response head exceeded 8 KiB");
    })
    .await
    .unwrap()
}

/// A stand-in broker: accepts one connection, answers the WebSocket upgrade
/// with a 101, greets, then echoes whatever it receives.
async fn echo_upstream() -> (Url, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = timeout(WAIT, listener.accept()).await.unwrap().unwrap();
        let head = read_head(&mut stream).await;
        let head_text = String::from_utf8_lossy(&head).to_ascii_lowercase();
        assert!(
            head_text.starts_with("get /mqtt http/1.1\r\n"),
            "{head_text}"
        );
        stream.write_all(SWITCH).await.unwrap();
        stream.write_all(b"broker-ready").await.unwrap();
        let (mut reader, mut writer) = stream.split();
        let _echo = tokio::io::copy(&mut reader, &mut writer).await;
    });
    (Url::parse(&format!("ws://{addr}/mqtt")).unwrap(), task)
}

#[test]
fn public_link_keeps_a_path_prefix() {
    let root = Url::parse("https://api.example.com").unwrap();
    assert_eq!(
        public_link(&root, "v1/auth/device").as_str(),
        "https://api.example.com/v1/auth/device"
    );
    let prefixed = Url::parse("https://example.com/hub/").unwrap();
    assert_eq!(
        public_link(&prefixed, "v1/jwks").as_str(),
        "https://example.com/hub/v1/jwks"
    );
}

#[tokio::test]
async fn a_client_that_drips_its_head_is_dropped_after_the_timeout() {
    let head_timeout = Duration::from_millis(800);
    let margin = Duration::from_millis(500);
    let server = start_hub(head_timeout, None).await;

    let started = Instant::now();
    let mut stream = TcpStream::connect(server.addr).await.unwrap();
    let (mut reader, mut writer) = stream.split();
    let mut sent = 0_usize;

    let closed = timeout(head_timeout + margin, async {
        let mut buf = [0_u8; 64];
        let drip = async {
            for &byte in b"GET /livez HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n" {
                if writer.write_all(&[byte]).await.is_err() {
                    return;
                }
                sent += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        };
        tokio::select! {
            read = reader.read(&mut buf) => read,
            () = drip => panic!("finished dripping the whole request head without being dropped"),
        }
    })
    .await
    .expect("hub did not drop the dripping connection within the timeout plus margin");

    let elapsed = started.elapsed();
    match closed {
        Ok(0) => {}
        Err(err) if err.kind() == ErrorKind::ConnectionReset => {}
        other => panic!("expected EOF or connection reset, got {other:?}"),
    }
    // Two on time (0 ms and 500 ms); a loaded machine may delay the second.
    assert!(
        (1..=2).contains(&sent),
        "expected one or two bytes sent before the 800 ms drop, sent {sent}"
    );
    assert!(
        elapsed >= head_timeout,
        "dropped before {head_timeout:?} elapsed ({elapsed:?})"
    );
    assert!(
        elapsed < head_timeout + margin,
        "dropped too late ({elapsed:?})"
    );
}

#[tokio::test]
async fn an_idle_client_that_sends_no_bytes_is_dropped_after_the_timeout() {
    let head_timeout = Duration::from_millis(300);
    let margin = Duration::from_millis(400);
    let server = start_hub(head_timeout, None).await;

    let started = Instant::now();
    let mut stream = TcpStream::connect(server.addr).await.unwrap();
    let mut buf = [0_u8; 16];
    let read = timeout(head_timeout + margin, stream.read(&mut buf))
        .await
        .expect("hub did not drop the silent connection within the timeout plus margin")
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(read, 0);
    assert!(
        elapsed >= head_timeout,
        "dropped before {head_timeout:?} elapsed ({elapsed:?})"
    );
}

#[tokio::test]
async fn a_normal_request_and_the_mqtt_upgrade_still_work() {
    let (upstream_url, upstream) = echo_upstream().await;

    let server = start_hub(HEADER_READ_TIMEOUT, Some(upstream_url)).await;

    let client = reqwest::Client::builder()
        .timeout(WAIT)
        .no_proxy()
        .build()
        .unwrap();
    let livez = client
        .get(format!("http://{}/livez", server.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(livez.status(), StatusCode::OK);
    assert_eq!(livez.headers()[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(
        livez.headers()[header::CONTENT_SECURITY_POLICY],
        "frame-ancestors 'none'"
    );
    assert_eq!(livez.text().await.unwrap(), "ok");

    let jwks: serde_json::Value = client
        .get(format!("http://{}/v1/jwks", server.addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(jwks["keys"].as_array().unwrap().len(), 1);

    let mut stream = TcpStream::connect(server.addr).await.unwrap();
    stream.write_all(UPGRADE_HEAD).await.unwrap();
    let response_head = String::from_utf8(read_head(&mut stream).await).unwrap();
    assert!(
        response_head.starts_with("HTTP/1.1 101 "),
        "{response_head}"
    );
    assert!(
        response_head
            .to_ascii_lowercase()
            .contains("sec-websocket-protocol: mqtt\r\n"),
        "{response_head}"
    );

    timeout(WAIT, async {
        let mut greeting = [0_u8; 12];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(&greeting, b"broker-ready");
        let payload = b"mqtt-frame-bytes";
        stream.write_all(payload).await.unwrap();
        let mut echoed = [0_u8; 16];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, payload);
    })
    .await
    .unwrap();
    drop(stream);
    timeout(WAIT, upstream).await.unwrap().unwrap();
}

#[tokio::test]
async fn an_upgraded_connection_outlives_the_header_timeout() {
    let head_timeout = Duration::from_millis(300);
    let (upstream_url, upstream) = echo_upstream().await;
    let server = start_hub(head_timeout, Some(upstream_url)).await;

    let mut stream = TcpStream::connect(server.addr).await.unwrap();
    stream.write_all(UPGRADE_HEAD).await.unwrap();
    let response_head = String::from_utf8(read_head(&mut stream).await).unwrap();
    assert!(
        response_head.starts_with("HTTP/1.1 101 "),
        "{response_head}"
    );

    tokio::time::sleep(head_timeout * 3).await;
    timeout(WAIT, async {
        let mut greeting = [0_u8; 12];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(&greeting, b"broker-ready");
        let payload = b"after-the-timeout";
        stream.write_all(payload).await.unwrap();
        let mut echoed = [0_u8; 17];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, payload);
    })
    .await
    .unwrap();
    drop(stream);
    timeout(WAIT, upstream).await.unwrap().unwrap();
}

#[tokio::test]
async fn a_slow_response_is_not_cut_by_the_header_timeout() {
    let head_timeout = Duration::from_millis(300);
    let router = Router::new().route(
        "/slow",
        get(move || async move {
            tokio::time::sleep(head_timeout * 3).await;
            "finished"
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        serve(listener, router, head_timeout, std::future::pending()).await;
    });
    let server = RunningHub { addr, task };

    let client = reqwest::Client::builder()
        .timeout(WAIT)
        .no_proxy()
        .build()
        .unwrap();
    for _ in 0..2 {
        let response = client
            .get(format!("http://{}/slow", server.addr))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "finished");
    }
}

#[tokio::test]
async fn graceful_shutdown_finishes_in_flight_requests() {
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let entered = Arc::new(std::sync::Mutex::new(Some(entered_tx)));
    let release = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let router = Router::new().route(
        "/slow",
        get({
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            move || async move {
                if let Some(tx) = entered.lock().unwrap().take() {
                    tx.send(()).unwrap();
                }
                let rx = release.lock().await.take().unwrap();
                rx.await.unwrap();
                "finished"
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut server = tokio::spawn(async move {
        serve(listener, router, HEADER_READ_TIMEOUT, async move {
            shutdown_rx.await.unwrap();
        })
        .await;
    });

    let client = reqwest::Client::builder()
        .timeout(WAIT)
        .no_proxy()
        .build()
        .unwrap();
    let response_task =
        tokio::spawn(async move { client.get(format!("http://{addr}/slow")).send().await });

    timeout(WAIT, entered_rx).await.unwrap().unwrap();
    shutdown_tx.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !server.is_finished(),
        "server exited before in-flight request finished"
    );

    release_tx.send(()).unwrap();
    let response = timeout(WAIT, response_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "finished");
    timeout(WAIT, &mut server).await.unwrap().unwrap();
}

#[test]
fn hub_mode_messages_name_the_paddle_settings() {
    let messages = [
        (
            GroupError::ManyTenants(2).to_string(),
            "a self-host hub has one group, but the database has 2 tenants; set AGMEN_HUB_MODE=hosted",
        ),
        (
            ADOPTED_GROUP_MESSAGE.to_owned(),
            "adopted the database's only tenant as this self-host hub's group; set AGMEN_HUB_MODE=hosted to keep tenants separate",
        ),
        (
            crate::BootstrapError::OtherGroup {
                group: "agmen".to_owned(),
            }
            .to_string(),
            "this self-host hub has one group, \"agmen\"; use --tenant \"agmen\" or set AGMEN_HUB_MODE=hosted",
        ),
    ];
    let mut missing = Vec::new();
    for (message, existing) in messages {
        assert!(message.starts_with(existing), "{message}");
        for setting in [
            "AGMEN_PADDLE_API_KEY",
            "AGMEN_PADDLE_WEBHOOK_SECRET",
            "AGMEN_PADDLE_PRICE_TEAM_BASE",
            "AGMEN_PADDLE_PRICE_TEAM_SEAT",
            "AGMEN_PADDLE_PRICE_BUSINESS_10",
            "AGMEN_PADDLE_PRICE_BUSINESS_25",
            "AGMEN_PADDLE_PRICE_BUSINESS_50",
        ] {
            if !message.contains(setting) {
                missing.push((existing, setting));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "messages omit Paddle settings: {missing:?}"
    );
}
