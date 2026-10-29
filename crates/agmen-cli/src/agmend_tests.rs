//! Each test speaks to a fake agmend on a temp Unix socket. None of them
//! connects to the user's real agmend socket.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agmen_local::frame::{FrameError, read_async, write_async};
use agmen_local::{API, ErrorCode, MAX_FRAME, Request, Response};
use agmen_proto::IdentityId;
use serde_json::json;
use tokio::io::AsyncWriteExt as _;
use tokio::net::UnixListener;

use super::{Agmend, AgmendError, LocalToken};

const NOT_RUNNING: &str = "agmend is not running: open the agmen app or run agmend service install";

fn hello_response(id: u64) -> Response {
    Response::ok(
        id,
        json!({
            "api": API,
            "version": "0.1.0",
            "build": "dev",
            "exe": "/app/agmend",
        }),
    )
}

/// `None` from the reply leaves the fake holding the connection and not answering.
struct Fake {
    _dir: tempfile::TempDir,
    path: PathBuf,
    requests: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fake {
    async fn start<F>(reply: F) -> Self
    where
        F: Fn(usize, &Request) -> Option<Response> + Send + 'static,
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agmend.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            // One hello plus a few calls. A test that sends more is wrong.
            for n in 0..8 {
                let request: Request = match read_async(&mut stream).await {
                    Ok(request) => request,
                    Err(_) => return,
                };
                recorded.lock().unwrap().push(request.clone());
                match reply(n, &request) {
                    Some(response) => {
                        if write_async(&mut stream, &response).await.is_err() {
                            return;
                        }
                    }
                    None => std::future::pending::<()>().await,
                }
            }
        });
        Self {
            _dir: dir,
            path,
            requests,
            task,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn hello_is_sent_first_and_kept() {
    let fake = Fake::start(|_, request| Some(hello_response(request.id))).await;
    let client = Agmend::connect(fake.path()).await.unwrap();
    let hello = client.hello();
    assert_eq!(hello.api, API);
    assert_eq!(hello.version.as_deref(), Some("0.1.0"));
    assert_eq!(hello.build.as_deref(), Some("dev"));
    assert_eq!(hello.exe.as_deref(), Some(Path::new("/app/agmend")));
    let requests = fake.requests();
    assert_eq!(requests.len(), 1, "hello is the only frame connect sends");
    assert_eq!(requests[0].v, API);
    assert_eq!(requests[0].id, 1);
    assert_eq!(requests[0].method, "hello");
    assert_eq!(requests[0].params, json!({ "api": API }));
    assert!(requests[0].as_.is_none());
}

#[tokio::test]
async fn version_mismatch_is_reported() {
    let fake = Fake::start(|_, request| {
        Some(Response::err(
            request.id,
            ErrorCode::Version,
            format!("agmend speaks local API 9; this client asked for {API}"),
        ))
    })
    .await;
    let err = Agmend::connect(fake.path()).await.unwrap_err();
    assert!(
        matches!(err, AgmendError::Version { daemon: 9, client } if client == API),
        "{err:?}"
    );

    let wrong_api = Fake::start(|_, request| {
        Some(Response::ok(
            request.id,
            json!({ "api": 4, "version": "0.1.0", "build": "dev" }),
        ))
    })
    .await;
    let err = Agmend::connect(wrong_api.path()).await.unwrap_err();
    assert!(
        matches!(err, AgmendError::Version { daemon: 4, client } if client == API),
        "{err:?}"
    );
}

/// agmend answers `ready` with the Version code when a store's schema is one
/// it does not support (`crates/agmend/src/api.rs`, `ready`). That is not a
/// local API mismatch, so after hello the code is a plain refusal and
/// agmend's own message reaches the person.
#[tokio::test]
async fn a_version_code_after_hello_is_a_refusal_with_agmends_message() {
    let schema = "daemon database schema 9 is unsupported (expected 2)";
    let fake = Fake::start(move |n, request| {
        if n == 0 {
            Some(hello_response(request.id))
        } else {
            Some(Response::err(request.id, ErrorCode::Version, schema))
        }
    })
    .await;
    let mut client = Agmend::connect(fake.path()).await.unwrap();
    let err = client.call("ready", json!({})).await.unwrap_err();
    match &err {
        AgmendError::Refused { code, message } => {
            assert_eq!(*code, ErrorCode::Version);
            assert_eq!(message, schema);
        }
        other => panic!("{other:?}"),
    }
    assert!(err.to_string().contains(schema), "{err}");
    let again = client.call("ready", json!({})).await.unwrap_err();
    assert!(
        matches!(again, AgmendError::Refused { .. }),
        "a refusal leaves the connection open: {again:?}"
    );
}

#[tokio::test]
async fn missing_socket_is_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.sock");
    let err = Agmend::connect(&missing).await.unwrap_err();
    assert!(matches!(err, AgmendError::NotRunning), "{err:?}");
    assert_eq!(err.to_string(), NOT_RUNNING);

    let refused = dir.path().join("refused.sock");
    {
        let _listener = UnixListener::bind(&refused).unwrap();
    }
    let err = Agmend::connect(&refused).await.unwrap_err();
    assert!(matches!(err, AgmendError::NotRunning), "{err:?}");
    assert_eq!(err.to_string(), NOT_RUNNING);
}

#[tokio::test]
async fn call_round_trips() {
    let fake = Fake::start(|n, request| {
        if n == 0 {
            Some(hello_response(request.id))
        } else {
            Some(Response::ok(request.id, json!(request.method)))
        }
    })
    .await;
    let mut client = Agmend::connect(fake.path()).await.unwrap();
    let who = client.call("whoami", json!({})).await.unwrap();
    let ready = client.call("ready", json!({ "wait": true })).await.unwrap();
    assert_eq!(who, json!("whoami"));
    assert_eq!(ready, json!("ready"));
    let requests = fake.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].id, 1);
    assert_eq!(requests[0].method, "hello");
    assert_eq!(requests[1].id, 2);
    assert_eq!(requests[1].method, "whoami");
    assert!(requests[1].as_.is_none());
    assert!(requests[1].params.get("local_token").is_none());
    assert_eq!(requests[2].id, 3);
    assert_eq!(requests[2].method, "ready");
    assert_eq!(requests[2].params, json!({ "wait": true }));
}

#[tokio::test]
async fn call_as_sends_identity_and_token() {
    let raw = "0123456789abcdef".repeat(4);
    let token: LocalToken = raw.parse().unwrap();
    let id: IdentityId = "idlisten".parse().unwrap();
    let fake = Fake::start(|n, request| {
        if n == 0 {
            Some(hello_response(request.id))
        } else {
            Some(Response::ok(request.id, json!("sent")))
        }
    })
    .await;
    let mut client = Agmend::connect(fake.path()).await.unwrap();
    let sent = client
        .call_as(&id, &token, "post", json!({ "body": "hi" }))
        .await
        .unwrap();
    assert_eq!(sent, json!("sent"));
    let bad = client
        .call_as(&id, &token, "post", json!(1))
        .await
        .unwrap_err();
    assert!(matches!(bad, AgmendError::BadParams), "{bad:?}");
    let after = client.call("whoami", json!({})).await.unwrap();
    assert_eq!(after, json!("sent"));
    let requests = fake.requests();
    assert_eq!(requests.len(), 3, "BadParams must not send a frame");
    assert_eq!(requests[1].id, 2);
    assert_eq!(requests[1].method, "post");
    assert_eq!(requests[1].as_.as_ref(), Some(&id));
    assert_eq!(requests[1].params["body"], json!("hi"));
    assert_eq!(requests[1].params["local_token"], json!(raw));
    assert_eq!(requests[2].id, 3);
    assert_eq!(requests[2].method, "whoami");
}

#[tokio::test]
async fn refusals_carry_code_and_message() {
    let fake = Fake::start(|n, request| {
        if n == 0 {
            Some(hello_response(request.id))
        } else {
            Some(Response::err(
                request.id,
                ErrorCode::Forbidden,
                "the listener may not post here",
            ))
        }
    })
    .await;
    let mut client = Agmend::connect(fake.path()).await.unwrap();
    let err = client.call("post", json!({})).await.unwrap_err();
    match err {
        AgmendError::Refused { code, message } => {
            assert_eq!(code, ErrorCode::Forbidden);
            assert_eq!(message, "the listener may not post here");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn slow_answer_times_out() {
    let fake = Fake::start(|n, request| {
        if n == 0 {
            Some(hello_response(request.id))
        } else {
            None
        }
    })
    .await;
    let mut client = Agmend::connect_with_call_timeout(fake.path(), Duration::from_millis(200))
        .await
        .unwrap();
    let started = Instant::now();
    let err = client.call("ready", json!({})).await.unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        matches!(err, AgmendError::Timeout { ref method } if method == "ready"),
        "{err:?}"
    );
    assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}

#[tokio::test]
async fn mismatched_response_id_is_a_protocol_error() {
    let fake = Fake::start(|n, request| {
        if n == 0 {
            Some(hello_response(request.id))
        } else {
            Some(Response::ok(99, json!({})))
        }
    })
    .await;
    let mut client = Agmend::connect(fake.path()).await.unwrap();
    let err = client.call("ready", json!({})).await.unwrap_err();
    match err {
        AgmendError::Protocol(message) => {
            assert!(message.contains("99"), "{message}");
            assert!(message.contains('2'), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

/// A hostile or broken agmend declares a body over the frame cap. The client
/// refuses it from the header alone and does not reuse the stream.
#[tokio::test]
async fn an_oversized_answer_is_refused_and_closes_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agmend.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let hello: Request = read_async(&mut stream).await.unwrap();
        write_async(&mut stream, &hello_response(hello.id))
            .await
            .unwrap();
        let _call: Request = read_async(&mut stream).await.unwrap();
        // The header names a body one byte over the cap. No body follows and
        // the connection stays open, so a client that waited for the body
        // would sit until its call timeout instead of failing at once.
        stream
            .write_all(&(MAX_FRAME + 1).to_be_bytes())
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    let mut client = Agmend::connect(&path).await.unwrap();
    let err = client.call("ready", json!({})).await.unwrap_err();
    assert!(
        matches!(err, AgmendError::Frame(FrameError::TooLarge { len }) if len == MAX_FRAME + 1),
        "{err:?}"
    );
    let after = client.call("ready", json!({})).await.unwrap_err();
    assert!(
        matches!(after, AgmendError::Protocol(ref text) if text.contains("closed")),
        "{after:?}"
    );
    server.abort();
}

/// The 2 second connect budget covers hello, not only the socket connect.
/// Without that, a socket that accepts and stays silent would hold the
/// caller for the 30 second call budget.
#[tokio::test]
async fn an_unanswered_hello_times_out_at_the_connect_budget() {
    let fake = Fake::start(|_, _| None).await;
    let started = Instant::now();
    let err = Agmend::connect(fake.path()).await.unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        matches!(err, AgmendError::Timeout { ref method } if method == "hello"),
        "{err:?}"
    );
    assert!(elapsed >= Duration::from_millis(1800), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

/// A late answer to a call that timed out, or an answer out of step, must
/// never be read as the answer to the next call.
#[tokio::test]
async fn a_timeout_or_a_mismatched_id_closes_the_connection() {
    let silent = Fake::start(|n, request| (n == 0).then(|| hello_response(request.id))).await;
    let mut client = Agmend::connect_with_call_timeout(silent.path(), Duration::from_millis(100))
        .await
        .unwrap();
    let err = client.call("ready", json!({})).await.unwrap_err();
    assert!(matches!(err, AgmendError::Timeout { .. }), "{err:?}");
    let after = client.call("ready", json!({})).await.unwrap_err();
    assert!(
        matches!(after, AgmendError::Protocol(ref text) if text.contains("closed")),
        "{after:?}"
    );
    assert_eq!(
        silent.requests().len(),
        2,
        "hello and the call that timed out"
    );

    let stray = Fake::start(|n, request| {
        Some(if n == 0 {
            hello_response(request.id)
        } else {
            Response::ok(99, json!({}))
        })
    })
    .await;
    let mut client = Agmend::connect(stray.path()).await.unwrap();
    let err = client.call("ready", json!({})).await.unwrap_err();
    assert!(matches!(err, AgmendError::Protocol(_)), "{err:?}");
    let after = client.call("ready", json!({})).await.unwrap_err();
    assert!(
        matches!(after, AgmendError::Protocol(ref text) if text.contains("closed")),
        "{after:?}"
    );
    assert_eq!(stray.requests().len(), 2, "hello and the mismatched call");
}

#[test]
fn local_token_debug_is_redacted() {
    let raw = "0123456789abcdef".repeat(4);
    assert_eq!(raw.len(), 64);
    let token: LocalToken = raw.parse().unwrap();
    let shown = format!("{token:?}");
    assert_eq!(shown, "LocalToken(<redacted>)");
    assert!(!shown.contains(&raw));
    assert!("0123456789ABCDEF".repeat(4).parse::<LocalToken>().is_err());
    assert!("gggggggggggggggg".repeat(4).parse::<LocalToken>().is_err());
    assert!("abc".parse::<LocalToken>().is_err());
}
