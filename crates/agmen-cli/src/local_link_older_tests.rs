//! The local watch against an agmend from before watch resume.
//!
//! The fake below answers like the agmend of 5d8f266, the last one whose
//! `watch` takes only `{project}`.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agmen_client::{ClientEvent, Cursors, IdentityFile, STATE_DB};
use agmen_local::{ErrorCode, Request, Response, frame};
use agmen_proto::Stream;
use serde_json::{Value, json};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tracing_subscriber::fmt::MakeWriter;

use super::{LocalLink, WatchState};

const WAIT: Duration = Duration::from_secs(10);

/// What the agmend of 5d8f266 answers a `watch` that carries `after`: its
/// params struct is `{project}` with `deny_unknown_fields`, and the console
/// wraps serde's complaint as a `bad_request` reading `bad params: {error}`.
const OLD_REFUSAL: &str = "bad params: unknown field `after`, expected `project`";

/// How a fake agmend answers one `watch` request.
enum Answer {
    /// An error frame, then the connection closes, as agmend does after a
    /// terminal error.
    Refuse(ErrorCode, &'static str),
    /// The `self` answer and then `events`. The connection stays open until
    /// the CLI closes it, unless `drop_after` cuts it first.
    Open {
        events: Vec<ClientEvent>,
        drop_after: bool,
    },
}

/// A daemon that answers `hello`, `page` (every stream at `head`) and `watch`.
/// `script` gets the number of each `watch` request, from 0, and its params.
struct FakeAgmend {
    watches: mpsc::UnboundedReceiver<Value>,
    task: JoinHandle<()>,
}

impl FakeAgmend {
    fn start(
        socket: &Path,
        me: Value,
        head: u64,
        script: impl Fn(usize, &Value) -> Answer + Send + Sync + 'static,
    ) -> Self {
        let listener = UnixListener::bind(socket).unwrap();
        // The CLI refuses an agmend socket that is not private to the user.
        std::fs::set_permissions(socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        let (seen, watches) = mpsc::unbounded_channel();
        let (script, count) = (Arc::new(script), Arc::new(AtomicUsize::new(0)));
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            while let Ok((socket, _)) = listener.accept().await {
                let (me, script, count, seen) =
                    (me.clone(), script.clone(), count.clone(), seen.clone());
                connections.spawn(async move {
                    serve(socket, &me, head, &*script, &count, &seen).await;
                });
            }
        });
        Self { watches, task }
    }

    /// The params of every `watch` request received so far, oldest first.
    fn asked(&mut self) -> Vec<Value> {
        std::iter::from_fn(|| self.watches.try_recv().ok()).collect()
    }
}

impl Drop for FakeAgmend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut socket: UnixStream,
    me: &Value,
    head: u64,
    script: &(dyn Fn(usize, &Value) -> Answer + Send + Sync),
    count: &AtomicUsize,
    seen: &mpsc::UnboundedSender<Value>,
) {
    while let Ok(request) = frame::read_async::<Request>(&mut socket).await {
        let id = request.id;
        let result = match request.method.as_str() {
            "hello" => json!({"api": 1}),
            "page" => {
                json!({"messages": [], "next_after": null, "head": head, "pruned_through": 0})
            }
            "watch" => {
                let n = count.fetch_add(1, Ordering::SeqCst);
                let _queued = seen.send(request.params.clone());
                let (events, drop_after) = match script(n, &request.params) {
                    Answer::Refuse(code, message) => {
                        let _sent =
                            frame::write_async(&mut socket, &Response::err(id, code, message))
                                .await;
                        return;
                    }
                    Answer::Open { events, drop_after } => (events, drop_after),
                };
                let frames = std::iter::once(me.clone())
                    .chain(events.iter().map(|e| serde_json::to_value(e).unwrap()));
                for result in frames {
                    if frame::write_async(&mut socket, &Response::ok(id, result))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                if !drop_after {
                    // Held open until the CLI closes its end.
                    let _end = frame::read_async::<Request>(&mut socket).await;
                }
                return;
            }
            _ => return,
        };
        if frame::write_async(&mut socket, &Response::ok(id, result))
            .await
            .is_err()
        {
            return;
        }
    }
}

fn identity() -> IdentityFile {
    serde_json::from_value(json!({
        "tenant": "tenant01", "project": "project1", "identity_id": "idn7k2q9",
        "handle": "andy/sol", "owner_id": "andyandy",
        "hub_url": agmen_client::LOCAL_HUB_URL,
        "broker_url": agmen_client::LOCAL_BROKER_URL,
    }))
    .unwrap()
}

fn me(identity: &IdentityFile) -> Value {
    json!({
        "kind": "self", "identity_id": identity.identity_id,
        "handle": identity.handle, "owner_id": identity.owner_id,
    })
}

fn link_at(socket: &Path, identity: &IdentityFile) -> LocalLink {
    LocalLink::at(socket.to_owned(), identity, "a".repeat(64).parse().unwrap())
}

fn feed_message(seq: u64) -> ClientEvent {
    serde_json::from_value(json!({
        "kind": "message",
        "message": {
            "v": 1, "id": format!("01JBX6Q4T7W3C2M8Y9K5N0P{seq:03}"), "verb": "FYI", "data": {},
            "body": format!("message {seq}"), "sent_at": "2026-10-25T16:04:11Z",
            "stream": "feed", "seq": seq, "from": "idother1", "from_handle": "x/y",
            "from_kind": "agent", "hub_at": "2026-10-25T16:04:11Z",
        },
    }))
    .unwrap()
}

/// The seqs of the next `count` messages, in the order they arrive.
async fn delivered(rx: &mut mpsc::Receiver<ClientEvent>, count: usize) -> Vec<u64> {
    tokio::time::timeout(WAIT, async {
        let mut seqs = Vec::new();
        while seqs.len() < count {
            if let ClientEvent::Message { message } =
                rx.recv().await.expect("the local watch stays open")
            {
                seqs.push(message.seq.0);
            }
        }
        seqs
    })
    .await
    .unwrap_or_else(|_| panic!("the local watch did not deliver {count} messages in {WAIT:?}"))
}

fn after_of(params: &Value) -> Option<&Value> {
    params.get("after")
}

#[derive(Clone, Default)]
struct LogBytes(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogBytes {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

impl LogBytes {
    /// Captures this thread's log lines until the guard drops.
    fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
        let logs = Self::default();
        let guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(logs.clone())
                .finish(),
        );
        (logs, guard)
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    /// How many lines say that agmend is older.
    fn older_notices(&self) -> usize {
        self.text()
            .lines()
            .filter(|line| line.contains("agmend is older"))
            .count()
    }
}

#[tokio::test]
async fn an_agmend_that_cannot_resume_still_gets_a_watch_and_its_messages() {
    let dir = tempfile::Builder::new().prefix("lo").tempdir().unwrap();
    let socket = dir.path().join("agmend.sock");
    let identity = identity();
    // The saved cursors start at 0, so neither live seq follows them.
    let live = vec![feed_message(2), feed_message(3)];
    let mut older = FakeAgmend::start(&socket, me(&identity), 0, move |_, params| {
        if after_of(params).is_some() {
            Answer::Refuse(ErrorCode::BadRequest, OLD_REFUSAL)
        } else {
            Answer::Open {
                events: live.clone(),
                drop_after: false,
            }
        }
    });
    let state = dir.path().join("watcher");
    // The link owns the watch task, so it lives as long as the test reads.
    let link = link_at(&socket, &identity);
    let mut rx = link
        .watch(WatchState::Delivery(&state))
        .await
        .expect("an agmend that cannot resume still gets a watch");
    let asked = older.asked();
    assert_eq!(asked.len(), 2, "{asked:?}");
    let inbox = Stream::Inbox(identity.identity_id.clone());
    assert_eq!(asked[0]["after"], json!({"feed": 0, inbox.to_string(): 0}));
    assert!(after_of(&asked[1]).is_none(), "{:?}", asked[1]);
    assert_eq!(asked[1]["project"], json!(identity.project));
    assert_eq!(delivered(&mut rx, 2).await, vec![2, 3]);
    // The cursor follows what was delivered, so a later resume starts after it.
    let db = state.join(STATE_DB);
    let cursors = tokio::task::spawn_blocking(move || Cursors::open(db).unwrap())
        .await
        .unwrap();
    let mut saved = 0;
    for _ in 0..100 {
        let cursors = cursors.clone();
        saved = tokio::task::spawn_blocking(move || cursors.last(&Stream::Feed).unwrap())
            .await
            .unwrap();
        if saved == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(saved, 3);
}

#[tokio::test]
async fn only_the_unknown_after_refusal_gets_a_retry_without_after() {
    let dir = tempfile::Builder::new().prefix("lo").tempdir().unwrap();
    let identity = identity();
    let cases = [
        // What a current agmend says to a hub project that sends `after`.
        (
            ErrorCode::BadRequest,
            "watch after is only supported for local projects",
        ),
        // Other bad requests, among them one about another field.
        (
            ErrorCode::BadRequest,
            "bad params: unknown field `afterwards`, expected `project`",
        ),
        (
            ErrorCode::BadRequest,
            "bad params: invalid type: integer `1`, expected a map",
        ),
        // The old text under another code.
        (ErrorCode::Forbidden, OLD_REFUSAL),
        (ErrorCode::Hub, OLD_REFUSAL),
    ];
    for (n, (code, message)) in cases.into_iter().enumerate() {
        let socket = dir.path().join(format!("refuses{n}.sock"));
        let mut other = FakeAgmend::start(&socket, me(&identity), 0, move |_, _| {
            Answer::Refuse(code, message)
        });
        let link = link_at(&socket, &identity);
        let error = link
            .watch(WatchState::Delivery(&dir.path().join("watcher")))
            .await
            .expect_err("this refusal ends the watch");
        assert_eq!(format!("{error:#}"), message);
        let asked = other.asked();
        assert_eq!(
            asked.len(),
            1,
            "{code:?} {message:?} was retried: {asked:?}"
        );
    }
}

#[tokio::test]
async fn an_older_agmend_is_reported_once_however_often_the_watch_reconnects() {
    let (logs, _guard) = LogBytes::capture();
    let dir = tempfile::Builder::new().prefix("lo").tempdir().unwrap();
    let socket = dir.path().join("agmend.sock");
    let identity = identity();
    // Request 0 is refused and 1 opens, with seq 2, before the connection is
    // cut. After the reconnect 2 is refused again and 3 opens, with seq 4.
    let mut older = FakeAgmend::start(&socket, me(&identity), 1, |n, params| {
        if after_of(params).is_some() {
            Answer::Refuse(ErrorCode::BadRequest, OLD_REFUSAL)
        } else {
            Answer::Open {
                events: vec![feed_message(n as u64 + 1)],
                drop_after: n == 1,
            }
        }
    });
    let link = link_at(&socket, &identity);
    let mut rx = link.watch(WatchState::Notifications).await.unwrap();
    assert_eq!(delivered(&mut rx, 2).await, vec![2, 4]);
    let resumes: Vec<_> = older
        .asked()
        .iter()
        .map(|params| after_of(params).is_some())
        .collect();
    assert_eq!(resumes, [true, false, true, false]);
    assert_eq!(logs.older_notices(), 1, "{}", logs.text());
}

#[tokio::test]
async fn resume_returns_when_agmend_restarts_on_a_newer_version() {
    let (logs, _guard) = LogBytes::capture();
    let dir = tempfile::Builder::new().prefix("lo").tempdir().unwrap();
    let socket = dir.path().join("agmend.sock");
    let identity = identity();
    // Request 0 is refused; 1 opens without resume and delivers seqs 2 and 4
    // before the connection is cut; 2 is the restarted, newer agmend, which
    // takes `after` and replays what follows seq 4.
    let mut agmend = FakeAgmend::start(&socket, me(&identity), 1, |n, params| {
        match (n, after_of(params).is_some()) {
            (0, true) => Answer::Refuse(ErrorCode::BadRequest, OLD_REFUSAL),
            (1, false) => Answer::Open {
                events: vec![feed_message(2), feed_message(4)],
                drop_after: true,
            },
            (2, true) => Answer::Open {
                events: vec![feed_message(5)],
                drop_after: false,
            },
            _ => Answer::Refuse(ErrorCode::Hub, "an unexpected watch request"),
        }
    });
    let link = link_at(&socket, &identity);
    let mut rx = link.watch(WatchState::Notifications).await.unwrap();
    assert_eq!(delivered(&mut rx, 3).await, vec![2, 4, 5]);
    let asked = agmend.asked();
    assert_eq!(asked.len(), 3, "{asked:?}");
    let inbox = Stream::Inbox(identity.identity_id.clone()).to_string();
    assert_eq!(asked[0]["after"], json!({"feed": 1, inbox.clone(): 1}));
    assert!(after_of(&asked[1]).is_none());
    assert_eq!(asked[2]["after"], json!({"feed": 4, inbox: 1}));
    assert_eq!(logs.older_notices(), 1, "{}", logs.text());
}

#[tokio::test(start_paused = true)]
async fn a_quiet_watch_on_an_older_agmend_is_not_cut_off() {
    let dir = tempfile::Builder::new().prefix("lo").tempdir().unwrap();
    let socket = dir.path().join("agmend.sock");
    let identity = identity();
    // An agmend this old sends no keepalive, so silence says nothing about it.
    let mut older = FakeAgmend::start(&socket, me(&identity), 0, |_, params| {
        if after_of(params).is_some() {
            Answer::Refuse(ErrorCode::BadRequest, OLD_REFUSAL)
        } else {
            Answer::Open {
                events: Vec::new(),
                drop_after: false,
            }
        }
    });
    let link = link_at(&socket, &identity);
    let mut rx = link.watch(WatchState::Notifications).await.unwrap();
    assert_eq!(
        rx.recv().await,
        Some(ClientEvent::Connection { connected: true })
    );
    for _ in 0..20 {
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(rx.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    assert_eq!(older.asked().len(), 2, "the watch reconnected");
}
