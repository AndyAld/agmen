//! Recovery and heartbeat isolation against a scripted local daemon.

#![cfg(test)]
#![cfg(unix)]

#[path = "support/local.rs"]
mod local_support;
mod support;

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use agmen_cli::McpOptions;
use agmen_client::{Outbox, OutboxState, STATE_DB};
use agmen_local::{ErrorCode, Request, Response, frame};
use agmen_proto::ClientEnvelope;
use local_support::Local;
use serde_json::{Value, json};
use support::mcp::{Mcp, text_of};
use support::{Worktree, assert_exit, stderr};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{Notify, mpsc};
use tokio::task::{JoinHandle, JoinSet};

const WAIT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy)]
enum Behavior {
    RefuseWatch {
        code: ErrorCode,
        reconnect: bool,
    },
    QueueHello,
    StallHeartbeat,
    /// The first watch ends when released, the second try is cut at once
    /// (agmend is still coming up), and the third watch stays open.
    DropWatch,
}

struct Fake {
    release: Arc<Notify>,
    posts: mpsc::UnboundedReceiver<ClientEnvelope>,
    task: JoinHandle<()>,
}

impl Fake {
    fn start(socket: &Path, worker: &Worktree, behavior: Behavior) -> Self {
        // The fixture's real daemon stays alive on its unlinked socket.
        std::fs::remove_file(socket).unwrap();
        let listener = UnixListener::bind(socket).unwrap();
        std::fs::set_permissions(socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        let identity = worker.identity();
        let me = json!({"kind": "self", "identity_id": identity.identity_id,
            "handle": identity.handle, "owner_id": identity.owner_id});
        let release = Arc::new(Notify::new());
        let released = release.clone();
        let (posted, posts) = mpsc::unbounded_channel();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            while let Ok((mut socket, _)) = listener.accept().await {
                let (me, release, posted, attempts) = (
                    me.clone(),
                    released.clone(),
                    posted.clone(),
                    attempts.clone(),
                );
                connections.spawn(async move {
                    while let Ok(request) = frame::read_async::<Request>(&mut socket).await {
                        let id = request.id;
                        let answer = match request.method.as_str() {
                            "hello" => Response::ok(id, json!({"api": 1})),
                            "page" => Response::ok(id, json!({"messages": [], "next_after": null, "head": 0, "pruned_through": 0})),
                            "state" => Response::ok(id, json!({"value": {"feed_seq": 0}})),
                            "watch" => {
                                if let Behavior::DropWatch = behavior {
                                    let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                    if n == 1 {
                                        return;
                                    }
                                    frame::write_async(&mut socket, &Response::ok(id, me)).await.unwrap();
                                    if n == 0 {
                                        release.notified().await;
                                    } else {
                                        let _closed = frame::read_async::<Request>(&mut socket).await;
                                    }
                                    return;
                                }
                                if let Behavior::RefuseWatch { code, reconnect: true } = behavior
                                    && attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0
                                {
                                    let _sent = frame::write_async(&mut socket, &Response::err(id, code, "identity revoked by the owner")).await;
                                    return;
                                }
                                frame::write_async(&mut socket, &Response::ok(id, me)).await.unwrap();
                                if let Behavior::RefuseWatch { code, reconnect } = behavior {
                                    release.notified().await;
                                    if !reconnect {
                                        let _sent = frame::write_async(&mut socket, &Response::err(id, code, "identity revoked by the owner")).await;
                                    }
                                } else {
                                    let _closed = frame::read_async::<Request>(&mut socket).await;
                                }
                                return;
                            }
                            "post" => {
                                let envelope = serde_json::from_value(request.params["envelope"].clone()).unwrap();
                                posted.send(envelope).unwrap();
                                match behavior {
                                    Behavior::QueueHello if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 => {
                                        frame::write_async(&mut socket, &Response::err(id, ErrorCode::Hub, "local.db is busy; try again")).await.unwrap();
                                        continue;
                                    }
                                    Behavior::QueueHello | Behavior::StallHeartbeat => release.notified().await,
                                    Behavior::RefuseWatch { .. } | Behavior::DropWatch => {}
                                }
                                Response::ok(id, json!({"stream": "feed", "seq": 1}))
                            }
                            method => panic!("unexpected method {method}"),
                        };
                        if frame::write_async(&mut socket, &answer).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self {
            release,
            posts,
            task,
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn options(local: &Local, worker: &Worktree) -> McpOptions {
    McpOptions::new(worker.path(), local.home.tokens(), local.home.state_base()).with_managed(true)
}

#[tokio::test]
async fn local_watch_terminal_refusal_exits_with_identity_error() {
    for (code, reconnect) in [
        (ErrorCode::Unauthorized, false),
        (ErrorCode::Forbidden, false),
        (ErrorCode::Unauthorized, true),
        (ErrorCode::Forbidden, true),
    ] {
        let local = Local::new().await;
        let worker = local.member("worker").await;
        let fake = Fake::start(
            &local.home.state_base().join("agmend.sock"),
            &worker,
            Behavior::RefuseWatch { code, reconnect },
        );
        let mut child =
            tokio::process::Command::from(local.home.agmen(&worker.path(), &["watch", "--json"]))
                .env("AGMEN_LOG", "off")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        tokio::time::timeout(WAIT, async {
            while let Some(line) = lines.next_line().await.unwrap() {
                if serde_json::from_str::<Value>(&line).unwrap()["connected"] == true {
                    break;
                }
            }
        })
        .await
        .unwrap();
        fake.release.notify_one();
        let output = tokio::time::timeout(WAIT, child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_exit(&output, 3);
        let error = stderr(&output);
        assert!(error.contains("identity revoked by the owner"), "{error}");
        assert!(
            error.contains("run agmen leave, then agmen join"),
            "{error}"
        );
        local.stop().await;
    }
}

#[tokio::test]
async fn local_mcp_terminal_refusal_keeps_server_and_reports_reason() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    let fake = Fake::start(
        &local.home.state_base().join("agmend.sock"),
        &worker,
        Behavior::RefuseWatch {
            code: ErrorCode::Forbidden,
            reconnect: false,
        },
    );
    let mcp = Mcp::start(options(&local, &worker)).await;
    assert!(
        mcp.text("state", json!({"key": "head"}))
            .await
            .contains("feed_seq")
    );
    fake.release.notify_one();
    tokio::time::timeout(WAIT, async {
        loop {
            let answer = mcp.call("state", json!({"key": "head"})).await;
            if answer.is_error == Some(true) {
                assert!(
                    text_of(&answer).contains("identity revoked by the owner"),
                    "{}",
                    text_of(&answer)
                );
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!mcp.tools().await.is_empty());
    let again = mcp.call("state", json!({"key": "head"})).await;
    assert!(text_of(&again).contains("identity revoked by the owner"));
    mcp.stop().await;
    local.stop().await;
}

#[tokio::test]
async fn local_watch_queues_failed_hello_in_existing_outbox() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    let mut fake = Fake::start(
        &local.home.state_base().join("agmend.sock"),
        &worker,
        Behavior::QueueHello,
    );
    let mut child =
        tokio::process::Command::from(local.home.agmen(&worker.path(), &["watch", "--json"]))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let hello = tokio::time::timeout(WAIT, fake.posts.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            let line = lines
                .next_line()
                .await
                .unwrap()
                .expect("watch starts despite failed HELLO");
            if serde_json::from_str::<Value>(&line).unwrap()["connected"] == true {
                break;
            }
        }
    })
    .await
    .unwrap();
    let path = local
        .home
        .state_base()
        .join(worker.identity().identity_id.as_str())
        .join(STATE_DB);
    let outbox = tokio::task::spawn_blocking(move || Outbox::open(path).unwrap())
        .await
        .unwrap();
    let (db, id) = (outbox.clone(), hello.id.clone());
    assert!(matches!(
        tokio::task::spawn_blocking(move || db.state(&id).unwrap())
            .await
            .unwrap(),
        Some(OutboxState::Pending { .. })
    ));
    fake.release.notify_one();
    let retried = tokio::time::timeout(WAIT, fake.posts.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retried, hello);
    tokio::time::timeout(WAIT, async {
        loop {
            let (db, id) = (outbox.clone(), hello.id.clone());
            if matches!(
                tokio::task::spawn_blocking(move || db.state(&id).unwrap())
                    .await
                    .unwrap(),
                Some(OutboxState::Recorded { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(child.try_wait().unwrap().is_none());
    child.start_kill().unwrap();
    child.wait().await.unwrap();
    local.stop().await;
}

#[tokio::test]
async fn local_mcp_tool_answers_while_heartbeat_post_is_stalled() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    assert_exit(&local.run(&worker, &["found", "heartbeat claim"]).await, 0);
    assert_exit(&local.run(&worker, &["claim", "B-1"]).await, 0);
    let mut fake = Fake::start(
        &local.home.state_base().join("agmend.sock"),
        &worker,
        Behavior::StallHeartbeat,
    );
    let mcp = Mcp::start(options(&local, &worker).with_heartbeat(Duration::from_secs(1))).await;
    let post = tokio::time::timeout(WAIT, fake.posts.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        post.payload,
        agmen_proto::Payload::Heartbeat { .. }
    ));
    let answer = tokio::time::timeout(
        Duration::from_secs(2),
        mcp.text("state", json!({"key": "head"})),
    )
    .await
    .expect("a heartbeat must not block tool calls");
    assert!(answer.contains("feed_seq"), "{answer}");
    mcp.stop().await;
    local.stop().await;
}

/// Reads `agmen watch`'s stderr lines until `times` of them have `needle`.
async fn stderr_until(
    lines: &mut mpsc::UnboundedReceiver<String>,
    seen: &mut Vec<String>,
    needle: &str,
    times: usize,
) {
    tokio::time::timeout(WAIT, async {
        while seen.iter().filter(|line| line.contains(needle)).count() < times {
            seen.push(lines.recv().await.expect("watch's stderr stays open"));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {needle:?} x{times} on stderr: {seen:?}"));
}

#[tokio::test]
async fn local_watch_notes_each_outage_once_posts_one_hello_and_stops_on_ctrl_c() {
    let local = Local::new().await;
    let worker = local.member("worker").await;
    let mut fake = Fake::start(
        &local.home.state_base().join("agmend.sock"),
        &worker,
        Behavior::DropWatch,
    );
    let mut child =
        tokio::process::Command::from(local.home.agmen(&worker.path(), &["watch", "--json"]))
            // Off, so a line on stderr is the watch's own notice.
            .env("AGMEN_LOG", "off")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut err = BufReader::new(child.stderr.take().unwrap()).lines();
    let (notes, mut stderr_lines) = mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        while let Some(line) = err.next_line().await.unwrap() {
            if notes.send(line).is_err() {
                return;
            }
        }
    });
    tokio::time::timeout(WAIT, async {
        while let Some(line) = out.next_line().await.unwrap() {
            if serde_json::from_str::<Value>(&line).unwrap()["connected"] == true {
                return;
            }
        }
        panic!("watch ended before it connected");
    })
    .await
    .unwrap();
    let hello = tokio::time::timeout(WAIT, fake.posts.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(hello.payload, agmen_proto::Payload::Hello { .. }));
    // The connection drops, one try finds agmend not answering, the next gets in.
    fake.release.notify_one();
    let mut seen = Vec::new();
    stderr_until(&mut stderr_lines, &mut seen, "reconnected to agmend", 1).await;
    let count = |seen: &[String], needle: &str| seen.iter().filter(|l| l.contains(needle)).count();
    assert_eq!(count(&seen, "waiting for agmend"), 1, "{seen:?}");
    assert_eq!(count(&seen, "reconnected to agmend"), 1, "{seen:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        fake.posts.try_recv().is_err(),
        "a reconnect must not post another HELLO"
    );
    // agmend goes away; the watch says so once more and waits.
    drop(fake);
    stderr_until(&mut stderr_lines, &mut seen, "waiting for agmend", 2).await;
    while let Ok(line) = stderr_lines.try_recv() {
        seen.push(line);
    }
    assert_eq!(count(&seen, "waiting for agmend"), 2, "{seen:?}");
    // Ctrl-C ends the wait at once.
    let pid = child.id().unwrap().to_string();
    let signalled = tokio::process::Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .await
        .unwrap();
    assert!(signalled.success());
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("Ctrl-C must end a watch that is waiting for agmend")
        .unwrap();
    assert_eq!(status.code(), Some(0), "{seen:?}");
    reader.abort();
    local.stop().await;
}
