//! Local watch delivery through the CLI reader and a real in-process daemon.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agmen_client::{ClientEvent, IdentityFile, TokenStore, read_local_credential};
use agmen_local::{Outcome, Request, Response, frame};
use agmen_proto::{Payload, StampedEnvelope, Stream};
use serde_json::{Value, json};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::older_tests::NEWER_LOCAL_DB_SAID;
use super::{LocalLink, WatchState};
use crate::agmend::Agmend;
use crate::output::explain;

const WAIT: Duration = Duration::from_secs(10);

struct Serving {
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Serving {
    fn new(config: agmend::Config) -> Self {
        let daemon = agmend::Agmend::bind(config).unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(daemon.serve_until(async {
            // Dropping the fixture during a failed assertion also stops it.
            let _closed = stopped.await;
        }));
        Self {
            stop: Some(stop),
            task,
        }
    }

    async fn close(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(WAIT, &mut self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    serving: Option<Serving>,
    identity: IdentityFile,
    link: LocalLink,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("lw").tempdir().unwrap();
        let config = config(dir.path());
        let serving = Serving::new(config);
        let mut daemon = Agmend::connect(&dir.path().join("agmend.sock"))
            .await
            .unwrap();
        let project = daemon
            .call("local_project_create", json!({"name":"watch"}))
            .await
            .unwrap();
        let identity: IdentityFile = serde_json::from_value(
            daemon
                .call(
                    "local_mint",
                    json!({
                        "project":project["project"], "name":"worker"
                    }),
                )
                .await
                .unwrap(),
        )
        .unwrap();
        let path = dir.path().to_owned();
        let who = identity.identity_id.clone();
        let token = tokio::task::spawn_blocking(move || {
            read_local_credential(&tokens(&path), &who)
                .unwrap()
                .unwrap()
                .local_token
        })
        .await
        .unwrap();
        let link = LocalLink::at(
            dir.path().join("agmend.sock"),
            &identity,
            token.parse().unwrap(),
        );
        Self {
            dir,
            serving: Some(serving),
            identity,
            link,
        }
    }

    async fn post(&self, stream: Stream, body: &str) -> (Stream, u64) {
        let mut envelope = crate::joined::envelope(Payload::Fyi {}, body.into()).unwrap();
        if let Stream::Inbox(id) = &stream {
            envelope.payload = Payload::Dm {};
            envelope.to.push(id.clone());
        }
        let mut daemon = Agmend::connect(&self.dir.path().join("agmend.sock"))
            .await
            .unwrap();
        daemon
            .call(
                "post",
                json!({
                    "project":self.identity.project, "envelope":envelope
                }),
            )
            .await
            .unwrap();
        // A DM's ACK names the sender's inbox copy. The watch receives the
        // recipient's copy, whose independent seq comes from its own page.
        let page = daemon
            .call_as(
                &self.identity.identity_id,
                &self.link.token,
                "page",
                json!({
                    "stream":stream, "limit":1
                }),
            )
            .await
            .unwrap();
        (stream, page["head"].as_u64().unwrap())
    }
}

fn tokens(dir: &Path) -> TokenStore {
    TokenStore::Dir(dir.join("tokens"))
}

fn config(dir: &Path) -> agmend::Config {
    agmend::Config::new(dir.join("agmend.sock"), dir.join("login.json"), tokens(dir))
        .personal_roles(dir.join("roles"))
}

async fn message(rx: &mut mpsc::Receiver<ClientEvent>, stage: &str) -> StampedEnvelope {
    tokio::time::timeout(WAIT, async {
        for _ in 0..32 {
            match rx.recv().await.expect("local watch remains open") {
                ClientEvent::Message { message } => return *message,
                _ => continue,
            }
        }
        panic!("no message within the event bound");
    })
    .await
    .unwrap_or_else(|_| panic!("local watch did not deliver {stage} within {WAIT:?}"))
}

async fn quiet(rx: &mut mpsc::Receiver<ClientEvent>) {
    let extra =
        tokio::time::timeout(Duration::from_millis(200), message(rx, "duplicate probe")).await;
    assert!(extra.is_err(), "watch delivered a duplicate: {extra:?}");
}

async fn received(rx: &mut mpsc::Receiver<ClientEvent>, count: usize) -> Vec<(Stream, u64)> {
    let mut rows = Vec::new();
    for _ in 0..count {
        let row = message(rx, "gap message").await;
        rows.push((row.stream, row.seq.0));
    }
    rows.sort();
    rows
}

#[tokio::test]
async fn local_watch_after_agmend_reopens_loses_no_message() {
    let mut f = Fixture::new().await;
    let mut rx = f
        .link
        .watch(WatchState::Delivery(&f.dir.path().join("watcher")))
        .await
        .unwrap();
    let inbox = Stream::Inbox(f.identity.identity_id.clone());
    f.post(Stream::Feed, "before feed").await;
    let feed = message(&mut rx, "feed before reopen").await;
    f.post(inbox.clone(), "before inbox").await;
    let dm = message(&mut rx, "inbox before reopen").await;
    f.serving.take().unwrap().close().await;
    let path = f.dir.path().join("local.db");
    let project = f.identity.project.clone();
    let expected = tokio::task::spawn_blocking(move || {
        let mut conn = rusqlite::Connection::open(path).unwrap();
        let tx = conn.transaction().unwrap();
        let mut expected = Vec::new();
        for (template, count) in [(feed, 505), (dm, 3)] {
            for offset in 1..=count {
                let mut row = template.clone();
                row.seq.0 += offset;
                row.client = crate::joined::envelope(template.client.payload.clone(), format!("gap {offset}")).unwrap();
                row.client.to = template.client.to.clone();
                tx.execute("INSERT INTO messages(project_id,stream,seq,id,from_id,payload_hash,envelope,hub_at,ack_row)
                    VALUES (?1,?2,?3,?4,?5,X'00',?6,0,1)", rusqlite::params![
                    project.as_str(), row.stream.to_string(), i64::try_from(row.seq.0).unwrap(), row.client.id.as_str(), row.from.as_str(), serde_json::to_string(&row).unwrap()
                ]).unwrap();
                expected.push((row.stream.clone(), row.seq.0));
            }
            tx.execute("UPDATE stream_heads SET seq=?1 WHERE stream=?2", rusqlite::params![
                i64::try_from(template.seq.0 + count).unwrap(), format!("{project}/{}", template.stream)
            ]).unwrap();
        }
        tx.commit().unwrap();
        expected.sort();
        expected
    }).await.unwrap();
    f.serving = Some(Serving::new(config(f.dir.path())));
    assert_eq!(received(&mut rx, expected.len()).await, expected);
    let tail = f.post(inbox, "after reopen").await;
    assert_eq!(received(&mut rx, 1).await, vec![tail]);
    quiet(&mut rx).await;
    drop(rx);
    f.serving.take().unwrap().close().await;
}

struct Proxy(JoinHandle<()>);

impl Drop for Proxy {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn proxy(
    path: &Path,
    target: PathBuf,
) -> (
    Proxy,
    oneshot::Sender<()>,
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
) {
    use std::os::unix::fs::PermissionsExt as _;
    let listener = UnixListener::bind(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (cut, mut cutting) = oneshot::channel();
    let (closed, closing) = oneshot::channel();
    let (resume, resuming) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut daemon = UnixStream::connect(&target).await.unwrap();
        tokio::select! {
            _ = &mut cutting => {},
            result = tokio::io::copy_bidirectional(&mut client, &mut daemon) => { result.unwrap(); },
        }
        drop((client, daemon));
        closed.send(()).unwrap();
        resuming.await.unwrap();
        for _ in 0..5 {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut daemon = UnixStream::connect(&target).await.unwrap();
            let _disconnected = tokio::io::copy_bidirectional(&mut client, &mut daemon).await;
        }
    });
    (Proxy(task), cut, closing, resume)
}

#[tokio::test]
async fn local_watch_forced_reconnect_loses_no_message() {
    let mut f = Fixture::new().await;
    let path = f.dir.path().join("proxy.sock");
    let (_proxy, cut, closing, resume) = proxy(&path, f.link.socket.clone());
    f.link.socket = path;
    let mut rx = f
        .link
        .watch(WatchState::Delivery(&f.dir.path().join("watcher")))
        .await
        .unwrap();
    let inbox = Stream::Inbox(f.identity.identity_id.clone());
    f.post(Stream::Feed, "before feed").await;
    message(&mut rx, "feed before socket drop").await;
    f.post(inbox.clone(), "before inbox").await;
    message(&mut rx, "inbox before socket drop").await;
    cut.send(()).unwrap();
    tokio::time::timeout(WAIT, closing).await.unwrap().unwrap();
    let mut expected = Vec::new();
    for stream in [Stream::Feed, inbox] {
        for _ in 0..3 {
            expected.push(f.post(stream.clone(), "during reconnect").await);
        }
    }
    expected.sort();
    resume.send(()).unwrap();
    assert_eq!(received(&mut rx, expected.len()).await, expected);
    quiet(&mut rx).await;
    drop(rx);
    f.serving.take().unwrap().close().await;
}

/// Answers `hello` and `watch` on the next connection, like agmend, and reports
/// the `after` that the watch asked for.
async fn scripted_watch(
    listener: &UnixListener,
    me: &Value,
    afters: &mpsc::UnboundedSender<Value>,
) -> (UnixStream, u64) {
    let (mut socket, _) = listener.accept().await.unwrap();
    let hello: Request = frame::read_async(&mut socket).await.unwrap();
    frame::write_async(&mut socket, &Response::ok(hello.id, json!({"api":1})))
        .await
        .unwrap();
    let watch: Request = frame::read_async(&mut socket).await.unwrap();
    afters.send(watch.params["after"].clone()).unwrap();
    frame::write_async(&mut socket, &Response::ok(watch.id, me.clone()))
        .await
        .unwrap();
    (socket, watch.id)
}

#[tokio::test]
async fn local_watch_asks_again_for_a_message_cut_off_in_a_dying_socket() {
    use tokio::io::AsyncWriteExt as _;
    let mut f = Fixture::new().await;
    for body in ["one", "two", "three"] {
        f.post(Stream::Feed, body).await;
    }
    let rows = f.link.page(&Stream::Feed, 0, 10).await.unwrap().messages;
    let (cut, kept) = rows.split_last().unwrap();
    let path = f.dir.path().join("scripted.sock");
    let listener = UnixListener::bind(&path).unwrap();
    // The CLI refuses an agmend socket that is not private to the user.
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let (afters, mut asked) = mpsc::unbounded_channel();
    let me = json!({
        "kind":"self", "identity_id":f.identity.identity_id,
        "handle":f.identity.handle, "owner_id":f.identity.owner_id
    });
    let (rows_for_agmend, last) = (kept.to_vec(), cut.clone());
    let agmend = tokio::spawn(async move {
        let (mut socket, id) = scripted_watch(&listener, &me, &afters).await;
        for row in rows_for_agmend {
            let event = ClientEvent::Message {
                message: Box::new(row),
            };
            let event = serde_json::to_value(event).unwrap();
            frame::write_async(&mut socket, &Response::ok(id, event))
                .await
                .unwrap();
        }
        // The last message goes into a socket that dies before the frame ends.
        let event = ClientEvent::Message {
            message: Box::new(last.clone()),
        };
        let body =
            serde_json::to_vec(&Response::ok(id, serde_json::to_value(event).unwrap())).unwrap();
        socket
            .write_all(&u32::try_from(body.len()).unwrap().to_be_bytes())
            .await
            .unwrap();
        socket.write_all(&body[..body.len() / 2]).await.unwrap();
        drop(socket);
        let (mut socket, id) = scripted_watch(&listener, &me, &afters).await;
        let event = ClientEvent::Message {
            message: Box::new(last),
        };
        let event = serde_json::to_value(event).unwrap();
        frame::write_async(&mut socket, &Response::ok(id, event))
            .await
            .unwrap();
        // Hold the watch open until the test drops it.
        let _end = frame::read_async::<Request>(&mut socket).await;
    });
    f.link.socket = path;
    let mut rx = f
        .link
        .watch(WatchState::Delivery(&f.dir.path().join("watcher")))
        .await
        .unwrap();
    let inbox = Stream::Inbox(f.identity.identity_id.clone());
    let first = tokio::time::timeout(WAIT, asked.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, json!({"feed":0, inbox.to_string():0}));
    let seen: Vec<_> = kept
        .iter()
        .map(|row| (row.stream.clone(), row.seq.0))
        .collect();
    assert_eq!(received(&mut rx, kept.len()).await, seen);
    // The cut-off frame did not count: the watch asks again from the last whole one.
    let again = tokio::time::timeout(WAIT, asked.recv())
        .await
        .unwrap()
        .unwrap();
    let from = kept.last().map_or(0, |row| row.seq.0);
    assert_eq!(again, json!({"feed":from, inbox.to_string():0}));
    assert_eq!(
        received(&mut rx, 1).await,
        vec![(cut.stream.clone(), cut.seq.0)]
    );
    quiet(&mut rx).await;
    drop(rx);
    agmend.abort();
    f.serving.take().unwrap().close().await;
}

#[tokio::test]
async fn local_watch_without_traffic_gets_keepalive_within_interval() {
    let mut f = Fixture::new().await;
    let mut daemon = Agmend::connect(&f.link.socket).await.unwrap();
    let raw = daemon
        .call_as(
            &f.identity.identity_id,
            &f.link.token,
            "state",
            json!({
                "project":f.identity.project, "key":"head"
            }),
        )
        .await
        .unwrap();
    assert_eq!(raw["value"]["feed_seq"], 0);
    let mut socket = UnixStream::connect(&f.link.socket).await.unwrap();
    frame::write_async(&mut socket, &Request::new(1, "hello", json!({"api":1})))
        .await
        .unwrap();
    let _: Response = frame::read_async(&mut socket).await.unwrap();
    // Request credentials through call_as, then use a separate watch's raw frames.
    let path = f.dir.path().to_owned();
    let who = f.identity.identity_id.clone();
    let credential = tokio::task::spawn_blocking(move || {
        read_local_credential(&tokens(&path), &who)
            .unwrap()
            .unwrap()
    })
    .await
    .unwrap();
    let mut request = Request::new(
        2,
        "watch",
        json!({
            "project":f.identity.project, "local_token":credential.local_token
        }),
    );
    request.as_ = Some(f.identity.identity_id.clone());
    frame::write_async(&mut socket, &request).await.unwrap();
    let first: Response = frame::read_async(&mut socket).await.unwrap();
    assert!(matches!(first.outcome, Outcome::Ok { .. }));
    let heartbeat = tokio::time::timeout(Duration::from_secs(31), async {
        for _ in 0..32 {
            let response: Response = frame::read_async(&mut socket).await.unwrap();
            assert_eq!(response.id, 2);
            let Outcome::Ok { result } = response.outcome else {
                panic!("watch refused");
            };
            if result.get("keepalive") == Some(&Value::Bool(true)) {
                assert_eq!(result["kind"], "connection");
                assert_eq!(result["connected"], true);
                return;
            }
        }
        panic!("keepalive missing from watch frames");
    })
    .await;
    drop(socket);
    f.serving.take().unwrap().close().await;
    heartbeat.expect("idle identity watch sends a keepalive within 30 seconds");
}

#[tokio::test]
async fn local_watch_resume_rejects_another_identity_inbox() {
    let mut f = Fixture::new().await;
    let daemon = Agmend::connect(&f.link.socket).await.unwrap();
    let result = daemon
        .watch_as(
            &f.identity.identity_id,
            &f.link.token,
            json!({
                "project":f.identity.project, "after":{"inbox:other001":0}
            }),
        )
        .await;
    assert!(
        matches!(
            result,
            Err(crate::agmend::AgmendError::Refused {
                code: agmen_local::ErrorCode::Forbidden,
                ..
            })
        ),
        "{result:?}"
    );
    f.serving.take().unwrap().close().await;
}

#[tokio::test]
async fn local_watch_resume_refuses_hostile_lists_at_once() {
    let mut f = Fixture::new().await;
    let crowd: serde_json::Map<String, Value> = (0..5_000)
        .map(|n| (format!("inbox:o{n:07}"), json!(0)))
        .collect();
    // Foreign inboxes, and seqs the store cannot hold, are refused before any page is read.
    let cases = [
        (Value::Object(crowd), agmen_local::ErrorCode::Forbidden),
        (
            json!({"feed": 9_223_372_036_854_775_808_u64}),
            agmen_local::ErrorCode::BadRequest,
        ),
        (json!({"feed": -1}), agmen_local::ErrorCode::BadRequest),
        (json!({"feed": 1.5}), agmen_local::ErrorCode::BadRequest),
        (json!({"nothing": 1}), agmen_local::ErrorCode::BadRequest),
    ];
    for (after, wanted) in cases {
        let daemon = Agmend::connect(&f.link.socket).await.unwrap();
        let params = json!({"project":f.identity.project, "after":after});
        let result = tokio::time::timeout(
            WAIT,
            daemon.watch_as(&f.identity.identity_id, &f.link.token, params),
        )
        .await
        .expect("a hostile resume list is refused promptly");
        assert!(
            matches!(&result, Err(crate::agmend::AgmendError::Refused { code, .. }) if *code == wanted),
            "wanted {wanted:?}: {result:?}"
        );
    }
    f.serving.take().unwrap().close().await;
}

#[tokio::test]
async fn local_watch_resume_replays_only_after_each_stream_seq() {
    let mut f = Fixture::new().await;
    let inbox = Stream::Inbox(f.identity.identity_id.clone());
    f.post(Stream::Feed, "old feed").await;
    f.post(inbox.clone(), "old inbox").await;
    let mut expected = vec![
        f.post(Stream::Feed, "new feed").await,
        f.post(inbox.clone(), "new inbox").await,
    ];
    expected.sort();
    let after = std::collections::BTreeMap::from([(Stream::Feed, 1), (inbox, 1)]);
    let daemon = Agmend::connect(&f.link.socket).await.unwrap();
    let mut watch = daemon
        .watch_as(
            &f.identity.identity_id,
            &f.link.token,
            json!({
                "project":f.identity.project, "after":after
            }),
        )
        .await
        .unwrap();
    let mut received = Vec::new();
    for _ in 0..2 {
        let event = tokio::time::timeout(WAIT, watch.recv())
            .await
            .unwrap()
            .unwrap();
        let ClientEvent::Message { message } = event else {
            panic!("expected replayed message");
        };
        received.push((message.stream.clone(), message.seq.0));
    }
    received.sort();
    assert_eq!(received, expected);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), watch.recv())
            .await
            .is_err()
    );
    drop(watch);
    f.serving.take().unwrap().close().await;
}

#[tokio::test]
async fn local_mcp_notifications_leave_delivery_cursors_for_cli_watch() {
    let mut f = Fixture::new().await;
    let state_dir = f.dir.path().join("watcher");
    let path = state_dir.clone();
    let cursors = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&path).unwrap();
        agmen_client::Cursors::open(path.join(agmen_client::STATE_DB)).unwrap()
    })
    .await
    .unwrap();
    // MCP uses this same watch reader to wake tools when messages arrive.
    let mut notifications = f.link.watch(WatchState::Notifications).await.unwrap();
    let expected = f.post(Stream::Feed, "before CLI watch opens").await;
    message(&mut notifications, "MCP notification").await;
    let mut delivered = 0;
    for _ in 0..50 {
        let cursors = cursors.clone();
        delivered = tokio::task::spawn_blocking(move || cursors.last(&Stream::Feed).unwrap())
            .await
            .unwrap();
        if delivered != 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(delivered, 0, "MCP leaves CLI delivery cursors unchanged");
    let cli = LocalLink::at(f.link.socket.clone(), &f.identity, f.link.token.clone());
    let mut messages = cli.watch(WatchState::Delivery(&state_dir)).await.unwrap();
    assert_eq!(received(&mut messages, 1).await, vec![expected]);
    drop((messages, notifications));
    f.serving.take().unwrap().close().await;
}

#[tokio::test]
async fn local_watch_stops_when_agmend_restarts_on_a_newer_local_db() {
    let mut f = Fixture::new().await;
    let mut rx = f
        .link
        .watch(WatchState::Delivery(&f.dir.path().join("watcher")))
        .await
        .unwrap();
    f.serving.take().unwrap().close().await;
    // This agmend, started on a local.db that a newer agmend wrote.
    let path = f.dir.path().join("local.db");
    tokio::task::spawn_blocking(move || {
        rusqlite::Connection::open(path)
            .unwrap()
            .pragma_update(None, "user_version", 2)
            .unwrap();
    })
    .await
    .unwrap();
    f.serving = Some(Serving::new(config(f.dir.path())));
    // The watch the stop cut tries again, meets the refusal and ends, where
    // answering busy had it wait for a local.db that never opens.
    tokio::time::timeout(WAIT, async { while rx.recv().await.is_some() {} })
        .await
        .expect("a newer local.db must end the watch, not be waited out");
    let error = f.link.check_watch().expect_err("the refusal is kept");
    assert_eq!(explain(&error), (NEWER_LOCAL_DB_SAID.to_owned(), 1));
    f.serving.take().unwrap().close().await;
}
