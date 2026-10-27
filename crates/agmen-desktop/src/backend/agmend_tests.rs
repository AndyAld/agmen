use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use agmen_local::console::ConsoleEvent;
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::StampedEnvelope;
use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};
use gpui::TestAppContext;
use serde_json::json;

use super::*;
use crate::backend::fake_agmend::{
    self, FakeAgmend, PROJECT, ROOT, console, ident, message, message_with, roster,
};
use crate::backend::{
    Action, Backend, BackendError, ChangeId, Draft, GutterMark, Link, RoleLayer, Update,
    WorktreeHead, WorktreeState,
};

/// Long enough for a loaded test machine, short enough to fail fast.
const WAIT: Duration = Duration::from_secs(10);
/// Reconnects in tests wait milliseconds, not seconds.
const FAST: Timing = Timing {
    first: Duration::from_millis(10),
    cap: Duration::from_millis(40),
};

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
    let backend = AgmendBackend::connect_with(&fake.path(), PROJECT, None, FAST).unwrap();
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
    assert_eq!(s.listener_id(), Some(&ident("idlisten")));

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
    assert!(s.listener.as_ref().unwrap().details.is_none());
    assert_eq!(s.status.worktrees, 0);
}

#[test]
fn offline_agents_read_offline() {
    // Task 7: an agent the hub reports offline is not reachable, so it does
    // not read idle; an away one still is.
    let mut c = console(vec![]);
    for entry in &mut c.roster {
        if entry.handle == "andy/fx1" {
            entry.status = agmen_local::console::Status::Offline;
        }
    }
    let s = snapshot_from(c);
    let status = |handle: &str| {
        s.roster
            .iter()
            .flat_map(|g| &g.agents)
            .find(|a| a.handle == handle)
            .map(|a| a.status)
    };
    assert_eq!(status("andy/fx1"), Some(AgentStatus::Offline));
    assert_eq!(status("andy/listen"), Some(AgentStatus::Idle));
    assert_eq!(status("sean/plan"), Some(AgentStatus::Working));
}

#[test]
fn a_project_without_my_listener_has_none() {
    // Task 7: sean's listener is not mine, and nothing falls back to me,
    // so the composer mentions no one rather than myself.
    let mut c = console(vec![]);
    c.roster.retain(|e| e.handle != "andy/listen");
    let s = snapshot_from(c);
    assert_eq!(s.listener, None);
    assert_eq!(s.listener_id(), None);
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
fn a_refused_watch_says_why() {
    // Final review fix 2: agmend is up, but refuses the watch; that is
    // not "can't reach agmend".
    let fake = FakeAgmend::start(console(vec![]));
    let (_backend, mut updates) = live_backend(&fake);
    fake.edit(|s| {
        s.watch_error = Some((
            ErrorCode::Unauthorized,
            "signed out: run agmen login".into(),
        ));
    });
    fake.end_watches(ErrorCode::Hub, "the hub closed the stream");
    let seen = wait_for(
        &mut updates,
        |u| matches!(u, Update::Link(Link::Refused { why }) if why.contains("signed out")),
    );
    assert_eq!(
        seen.last(),
        Some(&Update::Link(Link::Refused {
            why: "signed out: run agmen login".into()
        }))
    );
}

#[test]
fn a_lagged_console_says_why() {
    let fake = FakeAgmend::start(console(vec![]));
    let (_backend, mut updates) = live_backend(&fake);
    let lagged = "this console fell 3 events behind; take a new snapshot and watch again";
    fake.end_watches(ErrorCode::Hub, lagged);
    let seen = wait_for(&mut updates, |u| {
        matches!(u, Update::Link(Link::Refused { .. }))
    });
    assert_eq!(
        seen.last(),
        Some(&Update::Link(Link::Refused { why: lagged.into() }))
    );
    // The backend watches again and catches up.
    wait_for(&mut updates, is_live);
}

#[test]
fn agmend_gone_says_why() {
    let fake = FakeAgmend::start(console(vec![]));
    let (_backend, mut updates) = live_backend(&fake);
    fake.stop();
    let seen = wait_for(
        &mut updates,
        |u| matches!(u, Update::Link(Link::AgmendDown { why }) if why.contains("nothing answers")),
    );
    let Some(Update::Link(Link::AgmendDown { why })) = seen.last() else {
        panic!("the last update is the one matched")
    };
    assert_eq!(why, "nothing answers at its socket");
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
    let refused = AgmendBackend::connect(&fake.path(), PROJECT, None).map(|_| ());
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
        AgmendBackend::connect(Path::new("/nonexistent/agmend.sock"), PROJECT, None),
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

// Task 8: roles, worktrees, git facts and file text from agmend.

#[test]
fn local_data_maps_into_the_snapshot() {
    let fake = FakeAgmend::start(fake_agmend::console_with_checkout(vec![]));
    let backend =
        AgmendBackend::connect_with(&fake.path(), PROJECT, Some(Path::new(ROOT)), FAST).unwrap();
    let mut updates = backend.subscribe();
    wait_for(&mut updates, is_live);
    // The snapshot names the checkout the window found.
    let asked = fake.state.lock().unwrap().snapshot_params.clone();
    assert!(asked.iter().all(|p| p["root"] == ROOT), "{asked:?}");
    let s = backend.snapshot();
    let main = &s.worktrees[0];
    assert_eq!(main.handle, "andy/fx1");
    assert_eq!(main.owner, ident("idfx1abc"));
    assert_eq!(main.root.as_deref(), Some(ROOT));
    assert_eq!(main.head, WorktreeHead::Branch("main".into()));
    assert_eq!(main.state, WorktreeState::LocalOnly);
    assert_eq!(main.files, ["src/main.rs"]);
    assert_eq!(main.changes[0].added, 1);
    assert_eq!(main.commits[0].sha.as_str(), "9e2a71c");
    // A worktree with no identity file of its own is the person's.
    let review = &s.worktrees[1];
    assert_eq!(review.owner, s.me_id);
    assert_eq!(review.handle, "andy/review");
    assert_eq!(review.state, WorktreeState::Reading);
    assert_eq!(s.status.worktrees, 2);
    assert!(s.git.branches.contains("main"));
    assert!(s.git.files.contains("src/main.rs"));
    // Roles resolve as the fixture's do.
    let reviewer = s.roles.iter().find(|r| r.name == "reviewer").unwrap();
    assert_eq!(reviewer.layer, RoleLayer::BuiltIn);
    assert!(reviewer.info.is_some());
}

#[gpui::test]
fn file_view_reads_through_agmend(cx: &mut TestAppContext) {
    use crate::ui::{FileView, init_for_tests as init};

    let fake = FakeAgmend::start(fake_agmend::console_with_checkout(vec![]));
    let backend =
        AgmendBackend::connect_with(&fake.path(), PROJECT, Some(Path::new(ROOT)), FAST).unwrap();
    let mut updates = backend.subscribe();
    wait_for(&mut updates, is_live);
    let wt = backend.snapshot().worktrees[0].clone();
    init(cx);
    cx.update(|cx| cx.set_global(crate::backend::BackendHandle(std::rc::Rc::new(backend))));
    let (view, cx) = cx.add_window_view(|_, cx| FileView::new(&wt, "src/main.rs", cx));
    let deadline = Instant::now() + WAIT;
    loop {
        cx.run_until_parked();
        let text = view.read_with(cx, |file, cx| file.text(cx));
        if text == "fn live() {}\nfn more() {}" {
            break;
        }
        assert!(Instant::now() < deadline, "still showing {text:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
    view.read_with(cx, |file, cx| {
        assert_eq!(file.marks(cx), [GutterMark::Added(2..3)]);
    });
    let asked = fake.state.lock().unwrap().file_text_params.clone();
    assert_eq!(
        asked,
        [json!({"project": PROJECT, "root": ROOT, "worktree": ROOT, "path": "src/main.rs"})]
    );
}

#[test]
fn a_cut_file_keeps_its_full_size() {
    // Task 8 review fix 2: the desktop must know a file was cut.
    let entry = |truncated| agmen_local::console::FileTextEntry {
        text: "a\n".into(),
        marks: Vec::new(),
        truncated,
        size: 700_000,
    };
    assert_eq!(
        super::agmend_local::file_text_from(entry(true)).full_size,
        Some(700_000)
    );
    assert_eq!(
        super::agmend_local::file_text_from(entry(false)).full_size,
        None
    );
}

#[test]
fn local_events_update_the_snapshot() {
    // Task 8 review fix 3: the panels follow edits made after connect.
    let start = fake_agmend::console_with_checkout(vec![]);
    let fake = FakeAgmend::start(start.clone());
    let backend =
        AgmendBackend::connect_with(&fake.path(), PROJECT, Some(Path::new(ROOT)), FAST).unwrap();
    let mut updates = backend.subscribe();
    wait_for(&mut updates, is_live);
    let asked = fake.state.lock().unwrap().watch_params.clone();
    assert!(
        !asked.is_empty() && asked.iter().all(|p| p["root"] == ROOT),
        "{asked:?}"
    );
    let mut local = agmen_local::console::LocalData {
        roles: start.roles,
        worktrees: start.worktrees,
        git: start.git,
    };
    local.worktrees[0].files.push("src/new.rs".into());
    local.git.files.push("src/new.rs".into());
    fake.push(&ConsoleEvent::Local(Box::new(local)));
    wait_for(
        &mut updates,
        |u| matches!(u, Update::Snapshot(s) if s.worktrees[0].files.contains(&"src/new.rs".to_owned())),
    );
    let s = backend.snapshot();
    assert!(s.git.files.contains("src/new.rs"));
    // The chat and roster stay as they were.
    assert_eq!(s.roster.len(), backend.snapshot().roster.len());
    assert!(s.roles.iter().any(|r| r.name == "reviewer"));
}
