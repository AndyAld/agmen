use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agmen_local::console::{ConsoleEvent, ConsoleSnapshot, Me, RosterEntry, Status};
use agmen_local::{ApiError, ErrorCode, Request, Response, frame};
use agmen_proto::{IdentityId, IdentityKind, StampedEnvelope};
use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};
use gpui::TestAppContext;
use serde_json::{Value, json};

use super::*;
use crate::backend::{Action, Backend, BackendError, ChangeId, Draft, Link, Update};

const PROJECT: &str = "pr-3331";
/// Long enough for a loaded test machine, short enough to fail fast.
const WAIT: Duration = Duration::from_secs(10);
/// Reconnects in tests wait milliseconds, not seconds.
const FAST: Timing = Timing {
    first: Duration::from_millis(10),
    cap: Duration::from_millis(40),
};

fn ident(id: &str) -> IdentityId {
    id.parse().unwrap()
}

fn entry(id: &str, handle: &str, kind: IdentityKind, status: Status) -> RosterEntry {
    RosterEntry {
        identity_id: ident(id),
        handle: handle.into(),
        kind,
        status,
        agent: None,
        model: None,
        delegated_by: None,
    }
}

/// The roster as the hub orders it: by handle.
fn roster() -> Vec<RosterEntry> {
    vec![
        entry("idandy00", "andy", IdentityKind::Human, Status::Online),
        RosterEntry {
            agent: Some("claude-code/2.1.310".into()),
            delegated_by: Some(ident("idlisten")),
            ..entry("idfx1abc", "andy/fx1", IdentityKind::Agent, Status::Online)
        },
        RosterEntry {
            agent: Some("codex".into()),
            ..entry(
                "idlisten",
                "andy/listen",
                IdentityKind::Listener,
                Status::Away,
            )
        },
        entry("idsean00", "sean", IdentityKind::Human, Status::Offline),
        entry(
            "idseanl1",
            "sean/listen",
            IdentityKind::Listener,
            Status::Online,
        ),
        entry("idseanp1", "sean/plan", IdentityKind::Agent, Status::Online),
    ]
}

/// A feed message from sean with `seq`, as the hub stamps it.
fn message(seq: u64) -> StampedEnvelope {
    message_with(seq, "FYI")
}

fn message_with(seq: u64, verb: &str) -> StampedEnvelope {
    let data = if verb == "ASK" {
        json!({"urgency": "normal"})
    } else {
        json!({})
    };
    serde_json::from_value(json!({
        "v": 1,
        "id": format!("01JBX6Q4T7W3C2M8Y9K{seq:07}"),
        "body": format!("message {seq}"),
        "sent_at": "2026-10-25T22:00:00Z",
        "verb": verb,
        "data": data,
        "stream": "feed",
        "seq": seq,
        "from": "idsean00",
        "from_handle": "sean",
        "from_kind": "human",
        "hub_at": "2026-10-25T22:00:00Z",
    }))
    .unwrap()
}

fn console(messages: Vec<StampedEnvelope>) -> ConsoleSnapshot {
    ConsoleSnapshot {
        project: PROJECT.into(),
        me: Me {
            id: ident("idandy00"),
            handle: "andy".into(),
        },
        roster: roster(),
        messages,
        older: false,
    }
}

fn seqs(messages: &[StampedEnvelope]) -> Vec<u64> {
    messages.iter().map(|m| m.seq.0).collect()
}

fn draft(body: &str) -> Draft {
    Draft {
        to: vec![ident("idlisten")],
        body: body.into(),
        reply_to: None,
        model: None,
    }
}

// The fake agmend: a std UnixListener that speaks the local API's frames.

#[derive(Debug, Default)]
struct FakeState {
    snapshot: Option<ConsoleSnapshot>,
    /// Sent to every watch before a snapshot is answered, as if they
    /// arrived while agmend read the hub.
    during_snapshot: Vec<ConsoleEvent>,
    /// What a watch hears first.
    opening: Vec<ConsoleEvent>,
    post_error: Option<String>,
    posts: Vec<Request>,
    snapshots_served: usize,
    /// Each watch's connection and its request id.
    watchers: Vec<(UnixStream, u64)>,
    /// Every accepted connection, to drop on stop.
    streams: Vec<UnixStream>,
}

#[derive(Debug)]
struct FakeAgmend {
    dir: tempfile::TempDir,
    state: Arc<Mutex<FakeState>>,
    stop: Arc<AtomicBool>,
}

impl FakeAgmend {
    fn start(snapshot: ConsoleSnapshot) -> Self {
        let state = FakeState {
            snapshot: Some(snapshot),
            opening: vec![
                ConsoleEvent::Hub {
                    connected: false,
                    error: None,
                },
                ConsoleEvent::Hub {
                    connected: true,
                    error: None,
                },
            ],
            ..FakeState::default()
        };
        let fake = Self {
            dir: tempfile::tempdir().unwrap(),
            state: Arc::new(Mutex::new(state)),
            stop: Arc::new(AtomicBool::new(false)),
        };
        fake.listen(0o600);
        fake
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("agmend.sock")
    }

    fn listen(&self, mode: u32) {
        let path = self.path();
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let state = self.state.clone();
        let stop = self.stop.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { break };
                state
                    .lock()
                    .unwrap()
                    .streams
                    .push(stream.try_clone().unwrap());
                let state = state.clone();
                std::thread::spawn(move || serve(stream, &state));
            }
        });
    }

    /// agmend exits: the socket goes, and every connection drops.
    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accept loop, which then sees the flag.
        drop(UnixStream::connect(self.path()));
        std::fs::remove_file(self.path()).unwrap();
        let mut state = self.state.lock().unwrap();
        state.watchers.clear();
        for stream in state.streams.drain(..) {
            drop(stream.shutdown(std::net::Shutdown::Both));
        }
    }

    /// agmend starts again on the same socket.
    fn restart(&mut self) {
        self.stop = Arc::new(AtomicBool::new(false));
        self.listen(0o600);
    }

    fn edit(&self, change: impl FnOnce(&mut FakeState)) {
        change(&mut self.state.lock().unwrap());
    }

    fn push(&self, event: &ConsoleEvent) {
        push(&mut self.state.lock().unwrap(), event);
    }

    fn posts(&self) -> Vec<Request> {
        self.state.lock().unwrap().posts.clone()
    }
}

fn push(state: &mut FakeState, event: &ConsoleEvent) {
    let value = serde_json::to_value(event).unwrap();
    state
        .watchers
        .retain_mut(|(stream, id)| frame::write(stream, &Response::ok(*id, value.clone())).is_ok());
}

fn serve(mut stream: UnixStream, state: &Mutex<FakeState>) {
    while let Ok(request) = frame::read::<Request>(&mut stream) {
        let id = request.id;
        let answer = match request.method.as_str() {
            "hello" => Response::ok(id, json!({"api": 1})),
            "whoami" => Response::ok(id, json!({"id": "idandy00", "handle": "andy"})),
            "console_snapshot" => {
                let shared = state;
                let mut state = shared.lock().unwrap();
                state.snapshots_served += 1;
                let early = state.during_snapshot.clone();
                for event in &early {
                    push(&mut state, event);
                }
                if !early.is_empty() {
                    // The watch reads these before this answer lands.
                    drop(state);
                    std::thread::sleep(Duration::from_millis(100));
                    state = shared.lock().unwrap();
                }
                match &state.snapshot {
                    Some(snapshot) => Response::ok(id, serde_json::to_value(snapshot).unwrap()),
                    None => Response::err(id, ErrorCode::Hub, "the hub cannot be reached"),
                }
            }
            "post" => {
                let mut state = state.lock().unwrap();
                state.posts.push(request.clone());
                match &state.post_error {
                    Some(why) => Response::err(id, ErrorCode::Hub, why.clone()),
                    None => Response::ok(id, json!({"stream": "feed", "seq": 99})),
                }
            }
            "console_watch" => {
                let mut state = state.lock().unwrap();
                let mut watch = stream.try_clone().unwrap();
                for event in state.opening.clone() {
                    let value = serde_json::to_value(&event).unwrap();
                    frame::write(&mut watch, &Response::ok(id, value)).unwrap();
                }
                state.watchers.push((watch, id));
                drop(state);
                // A watch takes no more requests; any read ends it.
                drop(frame::read::<Value>(&mut stream));
                return;
            }
            other => Response::err(id, ErrorCode::NotFound, format!("no method {other}")),
        };
        if frame::write(&mut stream, &answer).is_err() {
            return;
        }
    }
}

// Waiting on the backend's threads.

/// Updates until one matches, failing after [`WAIT`].
fn wait_for(
    updates: &mut UnboundedReceiver<Update>,
    mut wanted: impl FnMut(&Update) -> bool,
) -> Vec<Update> {
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match updates.try_recv() {
            Ok(update) => {
                let done = wanted(&update);
                seen.push(update);
                if done {
                    return seen;
                }
            }
            Err(TryRecvError::Closed) => panic!("the backend closed its updates; saw {seen:?}"),
            Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    panic!("no matching update within {WAIT:?}; saw {seen:?}");
}

fn is_live(update: &Update) -> bool {
    matches!(update, Update::Link(Link::Live))
}

fn until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out after {WAIT:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn live_backend(fake: &FakeAgmend) -> (AgmendBackend, UnboundedReceiver<Update>) {
    let backend = AgmendBackend::connect_with(&fake.path(), PROJECT, FAST).unwrap();
    let mut updates = backend.subscribe();
    wait_for(&mut updates, is_live);
    (backend, updates)
}

// The tests.

#[test]
fn snapshot_maps_roster_into_person_groups() {
    let s = snapshot_from(console(vec![message(1), message(2)]));
    assert_eq!(s.project, PROJECT);
    assert_eq!(s.me, "andy");
    assert_eq!(s.me_id, ident("idandy00"));
    assert_eq!(seqs(&s.messages), vec![1, 2]);
    // The person's own listener, not sean's.
    assert_eq!(s.listener_id, ident("idlisten"));

    let people: Vec<(&str, bool, Vec<&str>)> = s
        .roster
        .iter()
        .map(|g| {
            let agents = g.agents.iter().map(|a| a.handle.as_str()).collect();
            (g.person.as_str(), g.you, agents)
        })
        .collect();
    assert_eq!(
        people,
        vec![
            ("andy", true, vec!["andy/fx1", "andy/listen"]),
            ("sean", false, vec!["sean/listen", "sean/plan"]),
        ]
    );
    assert_eq!(s.roster[0].id, ident("idandy00"));
    assert_eq!(s.roster[1].id, ident("idsean00"));
    let fx1 = &s.roster[0].agents[0];
    assert_eq!(fx1.name, "fx1");
    assert_eq!(fx1.harness, "claude-code");
    assert_eq!(fx1.status, AgentStatus::Working);
    // Delegated by the listener, so nested under it.
    assert_eq!(fx1.depth, 1);
    let listen = &s.roster[0].agents[1];
    assert_eq!(listen.role, "listener");
    assert_eq!(listen.status, AgentStatus::Idle);
    assert_eq!(listen.depth, 0);
    // Four of the six are online.
    assert_eq!(s.status.online, 4);

    // Nothing without a live source is filled.
    assert!(s.agents.is_empty());
    assert!(s.worktrees.is_empty());
    assert!(s.roles.is_empty());
    assert!(s.models.is_empty());
    assert!(s.installed_agents.is_empty());
    assert!(s.plan.is_none());
    assert!(s.listener.recent.is_empty());
    assert_eq!(s.status.worktrees, 0);
}

#[test]
fn watch_messages_arrive_as_updates() {
    let fake = FakeAgmend::start(console(vec![message(1)]));
    // Message 2 lands while the watch's snapshot is being taken; it is
    // kept once the snapshot arrives.
    fake.edit(|s| s.during_snapshot = vec![ConsoleEvent::Message(message(2))]);
    let (backend, mut updates) = live_backend(&fake);
    until(|| seqs(&backend.snapshot().messages) == vec![1, 2]);

    fake.push(&ConsoleEvent::Message(message(3)));
    let seen = wait_for(&mut updates, |u| matches!(u, Update::Message(_)));
    let Some(Update::Message(m)) = seen.last() else {
        panic!("the last update is the one matched")
    };
    assert_eq!(m.seq.0, 3);
    assert_eq!(seqs(&backend.snapshot().messages), vec![1, 2, 3]);

    // A roster change is a new snapshot.
    let mut fewer = roster();
    fewer.truncate(3);
    fake.push(&ConsoleEvent::Roster(fewer));
    let seen = wait_for(&mut updates, |u| matches!(u, Update::Snapshot(_)));
    let Some(Update::Snapshot(s)) = seen.last() else {
        panic!("the last update is the one matched")
    };
    assert_eq!(s.roster.len(), 1);
}

#[test]
fn hub_refusal_reaches_subscribers() {
    let fake = FakeAgmend::start(console(vec![]));
    let (_backend, mut updates) = live_backend(&fake);
    fake.push(&ConsoleEvent::Hub {
        connected: false,
        error: Some(ApiError {
            code: ErrorCode::Unauthorized,
            message: "signed out: run agmen login".into(),
        }),
    });
    let seen = wait_for(&mut updates, |u| {
        matches!(u, Update::Link(Link::HubDown { .. }))
    });
    assert_eq!(
        seen.last(),
        Some(&Update::Link(Link::HubDown {
            why: Some("signed out: run agmen login".into())
        }))
    );
}

#[test]
fn reconnect_resnapshots_without_duplicates() {
    let mut fake = FakeAgmend::start(console(vec![message(1), message(2)]));
    let (backend, mut updates) = live_backend(&fake);
    fake.push(&ConsoleEvent::Message(message(3)));
    wait_for(&mut updates, |u| matches!(u, Update::Message(_)));

    // agmend restarts; message 4 was posted while it was down, and its new
    // snapshot overlaps what the window already holds.
    fake.stop();
    wait_for(&mut updates, |u| {
        matches!(u, Update::Link(Link::AgmendDown { .. }))
    });
    fake.edit(|s| {
        s.snapshot = Some(console(vec![message(2), message(3), message(4)]));
        s.during_snapshot = vec![ConsoleEvent::Message(message(4))];
    });
    fake.restart();
    let seen = wait_for(&mut updates, is_live);

    let resnapshot = seen
        .iter()
        .rev()
        .find_map(|u| match u {
            Update::Snapshot(s) => Some(s),
            _ => None,
        })
        .expect("a reconnect sends a fresh snapshot");
    assert_eq!(seqs(&resnapshot.messages), vec![1, 2, 3, 4]);
    assert_eq!(seqs(&backend.snapshot().messages), vec![1, 2, 3, 4]);
    // The new stream overlaps the snapshot, so message 4 comes once more;
    // only message 5 is new.
    fake.push(&ConsoleEvent::Message(message(4)));
    fake.push(&ConsoleEvent::Message(message(5)));
    let seen = wait_for(&mut updates, |u| matches!(u, Update::Message(_)));
    let Some(Update::Message(m)) = seen.last() else {
        panic!("the last update is the one matched")
    };
    assert_eq!(m.seq.0, 5, "message 4 arrived twice");
    assert_eq!(seqs(&backend.snapshot().messages), vec![1, 2, 3, 4, 5]);
    // connect, the first watch, and the watch after the restart.
    assert_eq!(fake.state.lock().unwrap().snapshots_served, 3);
}

#[test]
fn a_gap_after_reconnect_drops_the_older_page() {
    let mut held = Held::default();
    held.merge(vec![message(1), message(2)]);
    // More was posted while agmend was down than one snapshot carries, so
    // 3 to 9 are missing; the window shows no hole.
    held.merge(vec![message(10), message(11)]);
    assert_eq!(seqs(&held.to_vec()), vec![10, 11]);
}

#[test]
fn backoff_doubles_up_to_30_s() {
    let mut delay = BACKOFF.first;
    let mut waits = vec![delay.as_secs()];
    for _ in 0..6 {
        delay = next_delay(delay, BACKOFF);
        waits.push(delay.as_secs());
    }
    assert_eq!(waits, vec![1, 2, 4, 8, 16, 30, 30]);
}

#[test]
fn send_posts_as_the_human() {
    let fake = FakeAgmend::start(console(vec![message_with(1, "ASK")]));
    let (backend, _updates) = live_backend(&fake);
    backend.send(draft("hi")).unwrap();
    until(|| fake.posts().len() == 1);
    let post = &fake.posts()[0];
    assert_eq!(post.as_, None, "the desktop never sends as");
    assert_eq!(post.params["project"], PROJECT);
    let envelope = &post.params["envelope"];
    assert_eq!(envelope["verb"], "FYI");
    assert_eq!(envelope["body"], "hi");
    // FYI takes no `to` (protocol 5): the listener is mentioned instead.
    assert_eq!(envelope["to"], json!([]));
    assert_eq!(envelope["mentions"], json!(["idlisten"]));
    let parsed: agmen_proto::ClientEnvelope =
        serde_json::from_value(envelope.clone()).expect("the hub would parse it");
    assert_eq!(parsed.reply_to, None);

    // A reply to an ASK is an ANSWER.
    let ask = message_with(1, "ASK").client.id;
    backend
        .send(Draft {
            reply_to: Some(ask.clone()),
            ..draft("yes")
        })
        .unwrap();
    until(|| fake.posts().len() == 2);
    let envelope = &fake.posts()[1].params["envelope"];
    assert_eq!(envelope["verb"], "ANSWER");
    assert_eq!(envelope["reply_to"], json!(ask));
}

#[test]
fn a_refused_post_comes_back_as_not_sent() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|s| s.post_error = Some("the hub refused it: forbidden_mention".into()));
    let (backend, mut updates) = live_backend(&fake);
    backend.send(draft("hi")).unwrap();
    let seen = wait_for(&mut updates, |u| matches!(u, Update::NotSent { .. }));
    assert_eq!(
        seen.last(),
        Some(&Update::NotSent {
            draft: draft("hi"),
            why: "the hub refused it: forbidden_mention".into(),
        })
    );
}

#[gpui::test]
fn send_while_disconnected_keeps_the_draft(cx: &mut TestAppContext) {
    use gpui::Focusable as _;

    use crate::ui::{Composer, bind_keys, init_for_tests as init};

    let fake = FakeAgmend::start(console(vec![]));
    let (backend, mut updates) = live_backend(&fake);
    fake.stop();
    wait_for(&mut updates, |u| {
        matches!(u, Update::Link(Link::AgmendDown { .. }))
    });
    assert!(matches!(
        backend.send(draft("hi")),
        Err(BackendError::NotConnected)
    ));

    init(cx);
    cx.update(bind_keys);
    let snapshot = backend.snapshot();
    let backend = std::rc::Rc::new(backend);
    let (composer, cx) = cx.add_window_view(|window, cx| {
        let backend = backend.clone();
        Composer::new(
            &snapshot,
            move |draft, _, _| backend.send(draft),
            window,
            cx,
        )
    });
    composer.update_in(cx, |composer, window, cx| {
        composer.focus_handle(cx).focus(window, cx);
    });
    cx.simulate_input("hi there");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.text(cx), "hi there");
    });
}

#[test]
fn holds_at_most_2000() {
    let fake = FakeAgmend::start(console((1..=500).map(message).collect()));
    let (backend, _updates) = live_backend(&fake);
    for seq in 501..=2100 {
        fake.push(&ConsoleEvent::Message(message(seq)));
    }
    until(|| backend.snapshot().messages.last().map(|m| m.seq.0) == Some(2100));
    let held = seqs(&backend.snapshot().messages);
    assert_eq!(held.len(), MAX_MESSAGES);
    assert_eq!(held.first(), Some(&101));
}

#[test]
fn refuses_foreign_socket() {
    let fake = FakeAgmend::start(console(vec![]));
    let me = rustix::process::geteuid().as_raw();

    // Owned by someone else (a uid that is not ours, since only root can
    // chown the file itself).
    let refused = check_socket(&fake.path(), me + 1);
    assert!(
        matches!(&refused, Err(BackendError::Agmend(why)) if why.contains("owned by")),
        "{refused:?}"
    );
    assert!(check_socket(&fake.path(), me).is_ok());

    // Readable by others.
    std::fs::set_permissions(fake.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    let refused = AgmendBackend::connect(&fake.path(), PROJECT).map(|_| ());
    assert!(
        matches!(&refused, Err(BackendError::Agmend(why)) if why.contains("0644")),
        "{refused:?}"
    );

    // Not a socket at all.
    let file = fake.dir.path().join("plain");
    std::fs::write(&file, "").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        check_socket(&file, me),
        Err(BackendError::Agmend(_))
    ));

    // Nothing there: agmend is not running.
    assert!(matches!(
        AgmendBackend::connect(Path::new("/nonexistent/agmend.sock"), PROJECT),
        Err(BackendError::NotConnected)
    ));
}

#[test]
fn unsupported_actions_say_so() {
    let fake = FakeAgmend::start(console(vec![]));
    let (backend, _updates) = live_backend(&fake);
    let change = ChangeId("C-015".into());
    let actions = [
        Action::Apply(change.clone()),
        Action::Cancel(change.clone()),
        Action::Undo(change),
        Action::TakeOver(ident("idfx1abc")),
        Action::HandBack(ident("idfx1abc")),
        Action::SwitchAgent {
            worker: ident("idfx1abc"),
            agent: "codex".into(),
        },
        Action::ScanAgents,
        Action::ApproveSpec {
            path: "docs/spec.md".into(),
            sha: "0123456789abcdef0123456789abcdef01234567".parse().unwrap(),
        },
        Action::ApproveShare,
        Action::KickOff,
        Action::SpawnWorker {
            agent: "claude".into(),
        },
    ];
    for action in actions {
        let result = backend.act(action.clone());
        assert!(
            matches!(result, Err(BackendError::Unsupported(_))),
            "{action:?} gave {result:?}"
        );
    }
    // Snoozing is the window's own state, so it needs no source.
    assert!(
        backend
            .act(Action::SnoozeAlert(message(1).client.id))
            .is_ok()
    );
}
