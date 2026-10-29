use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;
use url::Url;

use crate::mqtt_proxy::router;

const WAIT: Duration = Duration::from_secs(11);
const UPGRADE_HEAD: &[u8] = b"GET /mqtt HTTP/1.1\r\nHost: public.example\r\nUpgrade: WebSocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: mqtt\r\nSec-WebSocket-Extensions: example\r\n";
const SWITCH: &[u8] = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\nSec-WebSocket-Protocol: mqtt\r\nSec-WebSocket-Extensions: example\r\nSet-Cookie: must-not-escape\r\nX-Upstream: private\r\n\r\n";

#[derive(Debug)]
struct Server {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn proxy(upstream: SocketAddr, cap: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = router(
            Url::parse(&format!("ws://{upstream}/broker/mqtt?test=1")).unwrap(),
            cap,
        )
        .layer(axum::middleware::map_response(crate::http::refuse_framing));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { addr, task }
    }

    async fn upstream(response: Vec<u8>) -> (Self, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (heads, rx) = mpsc::channel(8);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            for _ in 0..8 {
                let (mut stream, _) = timeout(WAIT, listener.accept()).await.unwrap().unwrap();
                let heads = heads.clone();
                let response = response.clone();
                connections.spawn(async move {
                    timeout(Duration::from_secs(15), async move {
                        let head = read_head(&mut stream).await;
                        heads.send(head).await.unwrap();
                        if stream.write_all(&response).await.is_err() {
                            return;
                        }
                        if response.starts_with(b"HTTP/1.1 101") {
                            let (mut reader, mut writer) = stream.split();
                            // A client closing after its head can reset the socket.
                            let _echo = tokio::io::copy(&mut reader, &mut writer).await;
                        }
                    })
                    .await
                    .unwrap();
                });
            }
            timeout(WAIT, async {
                while connections.join_next().await.is_some() {}
            })
            .await
            .unwrap();
        });
        (Self { addr, task }, rx)
    }
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

async fn upgrade(addr: SocketAddr, extra: &[u8]) -> (TcpStream, Vec<u8>) {
    timeout(WAIT, async {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(UPGRADE_HEAD).await.unwrap();
        stream.write_all(extra).await.unwrap();
        stream.write_all(b"\r\n").await.unwrap();
        let head = read_head(&mut stream).await;
        (stream, head)
    })
    .await
    .unwrap()
}

async fn assert_error(mut stream: TcpStream, head: &[u8], status: u16, code: &str) {
    let head = String::from_utf8(head.to_vec()).unwrap();
    assert!(head.starts_with(&format!("HTTP/1.1 {status} ")), "{head}");
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .parse()
        .unwrap();
    let mut body = vec![0; length];
    timeout(WAIT, stream.read_exact(&mut body))
        .await
        .unwrap()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"], code);
}

#[tokio::test]
async fn proxy_passes_bytes_both_ways() {
    let mut response = SWITCH.to_vec();
    response.extend_from_slice(b"broker-ready");
    let (upstream, _heads) = Server::upstream(response).await;
    let proxy = Server::proxy(upstream.addr, 1).await;
    let (mut stream, head) = upgrade(proxy.addr, b"").await;
    let head = String::from_utf8(head).unwrap();
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
    for line in [
        "sec-websocket-protocol: mqtt",
        "sec-websocket-extensions: example",
        "sec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=",
        "x-frame-options: DENY",
        "content-security-policy: frame-ancestors 'none'",
    ] {
        assert!(head.contains(line), "missing {line}: {head}");
    }
    assert!(!head.contains("set-cookie") && !head.contains("x-upstream"));
    timeout(WAIT, async {
        let mut greeting = [0; 12];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(&greeting, b"broker-ready");
        let payload: Vec<_> = (0..70 * 1024).map(|n| (n % 251) as u8).collect();
        stream.write_all(&payload).await.unwrap();
        let mut echoed = vec![0; payload.len()];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(echoed, payload);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn proxy_forwards_only_websocket_headers() {
    let (upstream, mut heads) = Server::upstream(SWITCH.to_vec()).await;
    let proxy = Server::proxy(upstream.addr, 1).await;
    let (_stream, head) = upgrade(proxy.addr, b"Authorization: test-only\r\nCookie: private\r\nOrigin: https://untrusted.example\r\nX-Other: drop\r\nSec-WebSocket-Extensions: second=\xff\r\n").await;
    assert!(head.starts_with(b"HTTP/1.1 101 "));
    let head = timeout(WAIT, heads.recv()).await.unwrap().unwrap();
    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    assert!(
        text.starts_with("get /broker/mqtt?test=1 http/1.1\r\n"),
        "{text}"
    );
    assert!(text.contains(&format!("host: {}\r\n", upstream.addr)));
    let allowed = [
        "host",
        "upgrade",
        "connection",
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-protocol",
        "sec-websocket-extensions",
    ];
    for line in text.lines().skip(1).filter(|line| !line.is_empty()) {
        assert!(allowed.contains(&line.split_once(':').unwrap().0), "{line}");
    }
    for value in [
        b"WebSocket".as_slice(),
        b"keep-alive, Upgrade",
        b"dGhlIHNhbXBsZSBub25jZQ==",
        b"mqtt",
        b"example",
        b"second=\xff",
    ] {
        assert!(head.windows(value.len()).any(|part| part == value));
    }
}

#[tokio::test]
async fn proxy_needs_upgrade() {
    let proxy = Server::proxy("127.0.0.1:1".parse().unwrap(), 1).await;
    for upgrade in [None, Some("h2c")] {
        let client = reqwest::Client::builder().timeout(WAIT).build().unwrap();
        let mut request = client.get(format!("http://{}/mqtt", proxy.addr));
        if let Some(value) = upgrade {
            request = request.header("Upgrade", value);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 426);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"],
            "upgrade_required"
        );
    }
}

#[tokio::test]
async fn proxy_upstream_down_is_502() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let proxy = Server::proxy(addr, 1).await;
    let (stream, head) = upgrade(proxy.addr, b"").await;
    assert_error(stream, &head, 502, "upstream_unavailable").await;
}

#[tokio::test]
async fn proxy_caps_connections() {
    let (upstream, _heads) = Server::upstream(SWITCH.to_vec()).await;
    let proxy = Server::proxy(upstream.addr, 1).await;
    let (first, head) = upgrade(proxy.addr, b"").await;
    assert!(head.starts_with(b"HTTP/1.1 101 "));
    let (second, head) = upgrade(proxy.addr, b"").await;
    assert_error(second, &head, 503, "too_many_connections").await;
    drop(first);
    timeout(WAIT, async {
        for _ in 0..200 {
            let (third, head) = upgrade(proxy.addr, b"").await;
            if head.starts_with(b"HTTP/1.1 101 ") {
                return;
            }
            assert_error(third, &head, 503, "too_many_connections").await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("closed connection did not release its permit");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn proxy_upstream_non_101_is_502() {
    for response in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".as_slice(),
        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n",
        b"invalid HTTP\r\n\r\n",
    ] {
        let (upstream, _heads) = Server::upstream(response.to_vec()).await;
        let proxy = Server::proxy(upstream.addr, 1).await;
        let (stream, head) = upgrade(proxy.addr, b"").await;
        assert_error(stream, &head, 502, "upstream_unavailable").await;
    }
}

#[tokio::test]
async fn proxy_oversized_upstream_head_is_502() {
    let mut response = SWITCH[..SWITCH.len() - 2].to_vec();
    response.extend_from_slice(format!("X-Large: {}\r\n\r\n", "a".repeat(8192)).as_bytes());
    let (upstream, _heads) = Server::upstream(response).await;
    let proxy = Server::proxy(upstream.addr, 1).await;
    let (stream, head) = upgrade(proxy.addr, b"").await;
    assert_error(stream, &head, 502, "upstream_unavailable").await;
}

#[tokio::test]
async fn proxy_oversized_request_head_is_rejected() {
    let (upstream, mut heads) = Server::upstream(SWITCH.to_vec()).await;
    let proxy = Server::proxy(upstream.addr, 1).await;
    let extra = format!("Sec-WebSocket-Extensions: {}\r\n", "a".repeat(8192));
    let (stream, head) = upgrade(proxy.addr, extra.as_bytes()).await;
    assert_error(stream, &head, 431, "headers_too_large").await;
    assert!(heads.try_recv().is_err());
}

#[tokio::test]
async fn proxy_upstream_handshake_times_out() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = Server::proxy(listener.local_addr().unwrap(), 1).await;
    let (stream, head) = upgrade(proxy.addr, b"").await;
    assert_error(stream, &head, 502, "upstream_unavailable").await;
}

#[tokio::test]
async fn proxy_request_head_limit_is_exact() {
    let (upstream, _heads) = Server::upstream(SWITCH.to_vec()).await;
    let proxy = Server::proxy(upstream.addr, 2).await;
    for size in [8192, 8193] {
        let padding = size - UPGRADE_HEAD.len() - b"X-Padding: \r\n\r\n".len();
        let extra = format!("X-Padding: {}\r\n", "a".repeat(padding));
        let (stream, head) = upgrade(proxy.addr, extra.as_bytes()).await;
        if size == 8192 {
            assert!(
                head.starts_with(b"HTTP/1.1 101 "),
                "{}",
                String::from_utf8_lossy(&head)
            );
        } else {
            assert_error(stream, &head, 431, "headers_too_large").await;
        }
    }
}

#[tokio::test]
async fn proxy_response_head_limit_is_exact() {
    for size in [8192, 8193] {
        let mut response = SWITCH[..SWITCH.len() - 2].to_vec();
        response.extend_from_slice(b"Sec-WebSocket-Extensions: second=\xff\r\n");
        let padding = size - response.len() - b"X-Padding: \r\n\r\n".len();
        response
            .extend_from_slice(format!("X-Padding: {}\r\n\r\n", "a".repeat(padding)).as_bytes());
        assert_eq!(response.len(), size);
        let (upstream, _heads) = Server::upstream(response).await;
        let proxy = Server::proxy(upstream.addr, 1).await;
        let (stream, head) = upgrade(proxy.addr, b"").await;
        if size == 8192 {
            assert!(head.starts_with(b"HTTP/1.1 101 "));
            assert!(
                head.windows(b"second=\xff".len())
                    .any(|part| part == b"second=\xff")
            );
        } else {
            assert_error(stream, &head, 502, "upstream_unavailable").await;
        }
    }
}

#[tokio::test]
async fn proxy_frees_the_slot_when_the_upstream_closes_and_the_client_stays_silent() {
    // An upstream that answers 101 and hangs up. A client that then sends
    // nothing and keeps its socket open must not hold the permit forever.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let upstream = tokio::spawn(async move {
        for _ in 0..8 {
            let Ok(Ok((mut stream, _))) = timeout(Duration::from_secs(30), listener.accept()).await
            else {
                return;
            };
            read_head(&mut stream).await;
            stream.write_all(SWITCH).await.unwrap();
            drop(stream);
        }
    });
    let proxy = Server::proxy(addr, 1).await;
    let (_silent, head) = upgrade(proxy.addr, b"").await;
    assert!(head.starts_with(b"HTTP/1.1 101 "));
    timeout(WAIT, async {
        loop {
            let (next, head) = upgrade(proxy.addr, b"").await;
            if head.starts_with(b"HTTP/1.1 101 ") {
                return;
            }
            assert_error(next, &head, 503, "too_many_connections").await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("a silent client kept its slot after the upstream closed");
    upstream.abort();
}
