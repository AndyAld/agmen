//! Each test speaks to a fake agmend on a temp Unix socket. None of them
//! connects to the user's real agmend socket.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agmen_local::frame::{read_async, write_async};
use agmen_local::{API, ErrorCode, Request, Response};
use agmen_proto::IdentityId;
use serde_json::json;
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
