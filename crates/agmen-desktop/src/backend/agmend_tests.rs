use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use agmen_local::console::ConsoleEvent;
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{Seq, StampedEnvelope};
use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};
use gpui::TestAppContext;
use serde_json::json;

use super::*;
use crate::backend::fake_agmend::{
    self, FakeAgmend, PROJECT, ROOT, console, ident, message, message_with, roster,
};
use crate::backend::{
    Action, Backend, BackendError, ChangeId, Draft, GutterMark, Link, Older, RoleLayer, Snapshot,
    Update, WorktreeHead, WorktreeState,
};

/// Long enough for a loaded test machine, short enough to fail fast.
const WAIT: Duration = Duration::from_secs(10);
/// Reconnects in tests wait milliseconds, not seconds.
const FAST: Timing = Timing {
    first: Duration::from_millis(10),
    cap: Duration::from_millis(40),
};

#[test]
fn host_status_and_restart_use_the_local_api() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.host_status = Some(json!({
            "configured": true, "state": "failed", "last_error": "Postgres unavailable",
            "public_url": "https://hub.example/"
        }))
    });
    let status = AgmendBackend::host_status(&fake.path(), false).unwrap();
    assert_eq!(status.last_error.as_deref(), Some("Postgres unavailable"));
    let status = AgmendBackend::host_status(&fake.path(), true).unwrap();
    assert_eq!(status.state, HostState::Stopped);
    assert_eq!(fake.state.lock().unwrap().host_restarts, 1);
}

#[test]
fn hosted_startup_requires_a_hub_snapshot() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| state.snapshot = None);
    assert!(
        AgmendBackend::connect_host(&fake.path(), PROJECT, None, Instant::now() + WAIT).is_err()
    );
}

#[test]
fn hosted_startup_obeys_the_request_deadline() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| state.snapshot_delay = Duration::from_millis(250));
    let started = Instant::now();
    assert!(
        AgmendBackend::connect_host(
            &fake.path(),
            PROJECT,
            None,
            started + Duration::from_millis(40)
        )
        .is_err()
    );
    assert!(started.elapsed() < Duration::from_millis(200));
}

#[test]
fn persistent_connection_drops_the_handshake_deadline() {
    let fake = FakeAgmend::start(console(vec![]));
    let conn = Conn::open(&fake.path()).unwrap();
    assert!(
        conn.deadline.is_none(),
        "a cached request connection must use a fresh deadline on every call"
    );
}

#[test]
fn call_keeps_a_shorter_timeout_its_caller_set() {
    // probe and shutdown_owned bound their calls at 2 s, below REQUEST_TIMEOUT.
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| state.snapshot_delay = Duration::from_millis(500));
    let mut conn = Conn::connect(&fake.path()).unwrap();
    conn.stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let started = Instant::now();
    assert!(
        conn.call("console_snapshot", json!({"project": PROJECT}))
            .is_err()
    );
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn opened_connection_restores_the_full_request_timeout() {
    let fake = FakeAgmend::start(console(vec![]));
    let conn = Conn::open(&fake.path()).unwrap();
    assert_eq!(conn.stream.read_timeout().unwrap(), Some(REQUEST_TIMEOUT));
    assert_eq!(conn.stream.write_timeout().unwrap(), Some(REQUEST_TIMEOUT));
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
fn each_connection_reads_readiness_once() {
    let fake = FakeAgmend::start(console(vec![]));
    let backend = AgmendBackend::connect_with(&fake.path(), PROJECT, None, FAST).unwrap();
    until(|| !fake.state.lock().unwrap().watchers.is_empty());
    assert_eq!(fake.state.lock().unwrap().ready_calls, 2);
    drop(backend);
}

#[test]
fn reconnect_refuses_changed_identity_and_unready_stores() {
    let fake = FakeAgmend::start(console(vec![]));
    let expected = AgmendBackend::probe(&fake.path()).unwrap().0;
    fake.edit(|state| state.not_ready = true);
    assert!(matches!(
        open_for_request(&fake.path(), &expected),
        Err(RequestError::Refused(_))
    ));
    fake.edit(|state| {
        state.not_ready = false;
        state.hello_answer =
            Some(json!({"api": 1, "version": env!("CARGO_PKG_VERSION"), "exe": "/new/agmend"}));
    });
    assert!(matches!(
        open_for_request(&fake.path(), &expected),
        Err(RequestError::Refused(_))
    ));
    fake.edit(|state| state.hello_answer = Some(json!({"api": 1, "version": "newer"})));
    assert!(matches!(
        open_for_request(&fake.path(), &expected),
        Err(RequestError::Refused(_))
    ));
}

#[test]
fn probe_keeps_unknown_release_for_service_upgrade() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| state.hello_answer = Some(json!({"api": 1})));
    let (hello, ready) = AgmendBackend::probe(&fake.path()).unwrap();
    assert_eq!(hello.version, None);
    assert_eq!(ready, None);
    assert_eq!(fake.state.lock().unwrap().ready_calls, 0);
}

#[test]
fn desktop_keeps_hello_identity() {
    let fake = FakeAgmend::start(console(vec![]));
    let backend = AgmendBackend::connect_with(&fake.path(), PROJECT, None, FAST).unwrap();
    assert_eq!(backend.identity().api, agmen_local::API);
    assert_eq!(
        backend.identity().version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert!(backend.readiness().ready);
}

#[test]
fn hello_answer_without_version_parses() {
    let hello: agmen_local::HelloAnswer = serde_json::from_value(json!({"api": 1})).unwrap();
    assert_eq!(hello.version, None);
    assert_eq!(hello.exe, None);
}

#[test]
fn desktop_waits_for_ready() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.state.lock().unwrap().not_ready = true;
    assert!(AgmendBackend::connect_with(&fake.path(), PROJECT, None, FAST).is_err());
    assert_eq!(fake.state.lock().unwrap().snapshots_served, 0);
}

#[test]
fn snapshot_maps_roster_into_person_groups() {
    let fake = FakeAgmend::start(console(vec![message(1), message(2)]));
    fake.edit(|state| {
        state.agent_list_answer = Some(json!({"agents": [{
            "identity": "idfx1abc", "project": PROJECT,
            "handle": "andy/fx1", "state": "running"
        }]}));
    });
    let (backend, mut updates) = live_backend(&fake);
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.contains_key(&ident("idfx1abc")))
    });
    let s = backend.snapshot();
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

    assert_eq!(s.agents.keys().collect::<Vec<_>>(), [&ident("idfx1abc")]);
    assert_eq!(s.agents[&ident("idfx1abc")].handle, "andy/fx1");
    // Nothing without a live source is filled.
    assert!(s.worktrees.is_empty());
    assert!(s.roles.is_empty());
    assert!(s.models.is_empty());
    assert!(s.installed_agents.is_empty());
    assert!(s.plan.is_none());
    assert!(s.listener.as_ref().unwrap().details.is_none());
    assert_eq!(s.status.worktrees, 0);
}

#[test]
fn agent_panel_shows_spend_against_cap() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.agent_list_answer = Some(json!({"agents": [{
            "identity": "idmanaged", "project": PROJECT,
            "handle": "andy/worker", "state": "running",
            "model": "claude-sonnet-5-5", "role": "engineer",
            "budget_usd": "5", "spent_usd": "1.25",
            "worktree": "/repo/.worktrees/agent/worker-1234",
            "branch": "agent/worker-1234", "source": "launched"
        }]}));
    });
    let (backend, mut updates) = live_backend(&fake);
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.contains_key(&ident("idmanaged")))
    });
    let snapshot = backend.snapshot();
    let agent = &snapshot.agents[&ident("idmanaged")];
    assert_eq!(agent.id, ident("idmanaged"));
    assert_eq!(agent.handle, "andy/worker");
    assert_eq!(agent.model, "claude-sonnet-5-5");
    assert_eq!(agent.role, "engineer");
    assert_eq!(agent.role_label, "engineer");
    assert_eq!(agent.harness_label, "Claude Code");
    assert_eq!(agent.state, "running");
    assert_eq!(agent.status, AgentStatus::Working);
    assert_eq!(agent.branch, "agent/worker-1234");
    assert_eq!(
        agent.worktree_state,
        snapshot.managed_work[&agent.id]
            .worktree
            .as_deref()
            .unwrap()
    );
    assert_eq!(agent.spent_usd, 1.25);
    assert_eq!(agent.budget_usd, 5.0);
    assert!(agent.delegated_by.is_none());
    assert!(agent.tools.is_empty());
    assert!(!agent.tools_lossy);
    assert!(agent.verbs.is_empty());
    assert!(agent.refused.is_empty());
    assert!(agent.edits.is_empty());
    assert!(agent.base.is_empty());
    assert!(agent.cleanup.is_empty());
    assert_eq!(agent.since_launch_minutes, 0);
    assert_eq!(agent.working_minutes, 0);
    assert_eq!(agent.tokens_in, 0);
    assert_eq!(agent.tokens_out, 0);
    assert_eq!(agent.cache_read, 0);

    // A spend-only change must publish a new snapshot too.
    fake.edit(|state| {
        state.agent_list_answer.as_mut().unwrap()["agents"][0]["spent_usd"] = json!("2.50");
    });
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.agents.get(&ident("idmanaged")).is_some_and(|agent| agent.spent_usd == 2.5))
    });
    assert_eq!(
        backend.snapshot().agents[&ident("idmanaged")].spent_usd,
        2.5
    );
}

#[test]
fn older_agent_list_without_spend_still_parses() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.agent_list_answer = Some(json!({"agents": [{
            "identity": "idmanaged", "project": PROJECT,
            "handle": "andy/worker", "state": "offline"
        }]}));
    });
    let (backend, mut updates) = live_backend(&fake);
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.contains_key(&ident("idmanaged")))
    });
    let snapshot = backend.snapshot();
    assert!(snapshot.agent_error.is_none());
    let agent = &snapshot.agents[&ident("idmanaged")];
    assert!(agent.model.is_empty());
    assert!(agent.role.is_empty());
    assert!(agent.role_label.is_empty());
    assert!(agent.branch.is_empty());
    assert!(agent.worktree_state.is_empty());
    assert_eq!(agent.spent_usd, 0.0);
    assert_eq!(agent.budget_usd, 0.0);
    assert_eq!(agent.state, "offline");
    assert_eq!(agent.status, AgentStatus::Offline);
}

#[test]
fn agent_list_invalid_dollars_show_zero() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        let agents: Vec<_> = ["not-usd", "NaN", "inf", "1e999", "-5"]
            .into_iter()
            .enumerate()
            .map(|(i, amount)| {
                json!({
                    "identity": format!("idmoney{i}"), "project": PROJECT,
                    "handle": format!("andy/worker-{i}"), "state": "idle",
                    "budget_usd": amount, "spent_usd": amount
                })
            })
            .collect();
        state.agent_list_answer = Some(json!({"agents": agents}));
    });
    let (backend, mut updates) = live_backend(&fake);
    wait_for(
        &mut updates,
        |update| matches!(update, Update::Snapshot(snapshot) if snapshot.managed_agents.len() == 5),
    );
    let snapshot = backend.snapshot();
    assert_eq!(snapshot.agents.len(), 5);
    for agent in snapshot.agents.values() {
        assert_eq!(agent.spent_usd, 0.0);
        assert_eq!(agent.budget_usd, 0.0);
    }
}

#[test]
fn agent_panel_only_lists_this_projects_managed_agents() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.agent_list_answer = Some(json!({"agents": [
            {"identity": "idfx1abc", "project": PROJECT,
             "handle": "andy/fx1", "state": "idle", "role": "reviewer"},
            {"identity": "idmanaged", "project": PROJECT,
             "handle": "andy/joined", "state": "attached", "source": "joined"},
            {"identity": "idother0", "project": "another-project",
             "handle": "andy/other", "state": "running"}
        ]}));
    });
    let (backend, mut updates) = live_backend(&fake);
    wait_for(
        &mut updates,
        |update| matches!(update, Update::Snapshot(snapshot) if snapshot.managed_agents.len() == 2),
    );
    let snapshot = backend.snapshot();
    assert_eq!(
        snapshot.agents.keys().collect::<Vec<_>>(),
        [&ident("idfx1abc"), &ident("idmanaged")]
    );
    assert_eq!(snapshot.agents[&ident("idfx1abc")].state, "idle");
    assert_eq!(
        snapshot.agents[&ident("idfx1abc")].status,
        AgentStatus::Idle
    );
    assert_eq!(snapshot.agents[&ident("idfx1abc")].role, "reviewer");
    assert_eq!(snapshot.agents[&ident("idmanaged")].state, "attached");
    assert_eq!(
        snapshot.agents[&ident("idmanaged")].status,
        AgentStatus::Working
    );
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
    held.merge(vec![message(1), message(2)], false);
    // More was posted while agmend was down than one snapshot carries, so
    // 3 to 9 are missing; the window shows no hole.
    held.merge(vec![message(10), message(11)], true);
    assert_eq!(seqs(&held.to_vec()), vec![10, 11]);
}

#[test]
fn a_restart_reconnects_without_a_refusal() {
    let mut fake = FakeAgmend::start(console(vec![message(1), message(2)]));
    let (backend, mut updates) = live_backend(&fake);
    fake.stop();
    wait_for(&mut updates, |u| {
        matches!(u, Update::Link(Link::AgmendDown { .. }))
    });
    fake.edit(|s| s.snapshot = Some(console(vec![message(1), message(2), message(3)])));
    let started = Instant::now();
    fake.restart();
    let seen = wait_for(&mut updates, is_live);
    // The request connection still pointed at the dead agmend; the first
    // snapshot must retry on a fresh one instead of showing a refusal.
    assert!(
        !seen
            .iter()
            .any(|u| matches!(u, Update::Link(Link::Refused { .. }))),
        "a restart showed a refusal: {seen:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(seqs(&backend.snapshot().messages), vec![1, 2, 3]);
}

#[test]
fn a_failed_retry_connection_does_not_strand_a_healthy_watch() {
    let fake = FakeAgmend::start(console(vec![message(1), message(2), message(3)]));
    let (backend, mut updates) = live_backend(&fake);
    // The request connection is dead, and the fresh one agmend gives for
    // the retry is cut off too, while the watch itself stays up.
    fake.sever_requests();
    fake.edit(|s| s.drop_hellos_while_watching = 1);
    fake.push(&ConsoleEvent::Hub {
        connected: false,
        error: None,
    });
    fake.push(&ConsoleEvent::Hub {
        connected: true,
        error: None,
    });
    let seen = wait_for(&mut updates, |u| {
        matches!(u, Update::Link(Link::AgmendDown { .. }))
    });
    assert!(
        !seen
            .iter()
            .any(|u| matches!(u, Update::Link(Link::Refused { .. }))),
        "a transport failure showed a refusal: {seen:?}"
    );
    // The watch reconnects and snapshots again instead of staying down.
    wait_for(&mut updates, is_live);
    assert_eq!(seqs(&backend.snapshot().messages), vec![1, 2, 3]);
}

#[test]
fn a_refused_hello_is_a_refusal_not_agmend_down() {
    let fake = FakeAgmend::start(console(vec![]));
    let (_backend, mut updates) = live_backend(&fake);
    fake.sever_requests();
    fake.edit(|s| s.hello_error = Some("api 9 is not supported".into()));
    fake.push(&ConsoleEvent::Hub {
        connected: false,
        error: None,
    });
    fake.push(&ConsoleEvent::Hub {
        connected: true,
        error: None,
    });
    let seen = wait_for(&mut updates, |u| {
        matches!(u, Update::Link(Link::Refused { .. }))
    });
    let Some(Update::Link(Link::Refused { why })) = seen.last() else {
        panic!("the last update is the one matched")
    };
    assert!(why.contains("api 9 is not supported"), "{why}");
}

#[test]
fn a_post_on_a_dead_connection_is_retried_once() {
    let fake = FakeAgmend::start(console(vec![]));
    let (backend, mut updates) = live_backend(&fake);
    fake.sever_requests();
    backend.send(draft("hi")).unwrap();
    until(|| !fake.posts().is_empty());
    // Time for a duplicate or a failure to show.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(fake.posts().len(), 1, "the post landed once");
    while let Ok(update) = updates.try_recv() {
        assert!(
            !matches!(update, Update::NotSent { .. }),
            "the post was reported unsent: {update:?}"
        );
    }
}

#[test]
fn a_post_whose_answer_was_lost_reuses_its_envelope_id() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|s| s.drop_post_answers = 1);
    let (backend, _updates) = live_backend(&fake);
    backend.send(draft("hi")).unwrap();
    until(|| fake.posts().len() == 2);
    let posts = fake.posts();
    // The hub dedupes on (from, id), so the second try returns the first ACK.
    assert_eq!(
        posts[0].params["envelope"]["id"],
        posts[1].params["envelope"]["id"]
    );
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

// Final review fix 1: paging older messages on scroll-back.

/// Asks for the next older page and waits for the snapshot that ends the
/// load, Loading first.
fn page_back(backend: &AgmendBackend, updates: &mut UnboundedReceiver<Update>) -> Snapshot {
    backend.load_older();
    assert_eq!(backend.snapshot().older, Older::Loading);
    let seen = wait_for(
        updates,
        |u| matches!(u, Update::Snapshot(s) if s.older != Older::Loading),
    );
    let Some(Update::Snapshot(s)) = seen.last() else {
        panic!("the last update is the one matched")
    };
    (**s).clone()
}

#[test]
fn history_pages_back_on_request() {
    let fake = FakeAgmend::with_history(1200);
    let (backend, mut updates) = live_backend(&fake);
    let first = backend.snapshot();
    assert_eq!(seqs(&first.messages), (701..=1200).collect::<Vec<_>>());
    assert_eq!(first.older, Older::More);

    let s = page_back(&backend, &mut updates);
    assert_eq!(seqs(&s.messages), (201..=1200).collect::<Vec<_>>());
    assert_eq!(s.older, Older::More);
    let s = page_back(&backend, &mut updates);
    // Each message once across the pages, oldest first.
    assert_eq!(seqs(&s.messages), (1..=1200).collect::<Vec<_>>());
    assert_eq!(s.older, Older::Start);
    assert!(!s.newer);
    let asked = fake.state.lock().unwrap().history_params.clone();
    assert_eq!(
        asked,
        [
            json!({"project": PROJECT, "before": 701, "limit": 500}),
            json!({"project": PROJECT, "before": 201, "limit": 500}),
        ]
    );
    // Nothing older: no more requests.
    backend.load_older();
    assert_eq!(backend.snapshot().older, Older::Start);
    assert_eq!(fake.state.lock().unwrap().history_params.len(), 2);
}

#[test]
fn paging_back_past_2000_lets_the_newest_go() {
    let fake = FakeAgmend::with_history(3000);
    let (backend, mut updates) = live_backend(&fake);
    // 2501 to 3000, then three pages back: 2000 held, at the cap.
    for _ in 0..2 {
        page_back(&backend, &mut updates);
    }
    let s = page_back(&backend, &mut updates);
    assert_eq!(seqs(&s.messages), (1001..=3000).collect::<Vec<_>>());
    assert!(!s.newer);

    // The fourth page passes the cap: the page the person scrolled to
    // stays, and the newest 500 go.
    let s = page_back(&backend, &mut updates);
    assert_eq!(s.messages.len(), MAX_MESSAGES);
    assert_eq!(seqs(&s.messages), (501..=2500).collect::<Vec<_>>());
    assert_eq!(s.older, Older::More);
    assert!(s.newer);
    // A live message would leave a hole after 2500; it waits.
    fake.push(&ConsoleEvent::Message(message(3001)));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(backend.snapshot().messages.len(), MAX_MESSAGES);
    assert_eq!(backend.snapshot().messages.last().unwrap().seq.0, 2500);

    // Back to the newest page: a fresh snapshot, and live messages again.
    backend.load_newest();
    wait_for(
        &mut updates,
        |u| matches!(u, Update::Snapshot(s) if !s.newer),
    );
    let s = backend.snapshot();
    assert_eq!(seqs(&s.messages), (2501..=3000).collect::<Vec<_>>());
    assert_eq!(s.older, Older::More);
    fake.push(&ConsoleEvent::Message(message(3001)));
    until(|| backend.snapshot().messages.last().map(|m| m.seq.0) == Some(3001));
}

#[test]
fn a_message_while_paused_is_announced_not_held() {
    let fake = FakeAgmend::with_history(3000);
    let (backend, mut updates) = live_backend(&fake);
    for _ in 0..3 {
        page_back(&backend, &mut updates);
    }
    assert!(page_back(&backend, &mut updates).newer);

    // The chat does not change, but the view hears each message once, with
    // the count waiting.
    fake.push(&ConsoleEvent::Message(message(3001)));
    let seen = wait_for(&mut updates, |u| matches!(u, Update::Waiting { .. }));
    let Some(Update::Waiting {
        message: heard,
        waiting,
    }) = seen.last()
    else {
        panic!("no waiting update in {seen:?}");
    };
    assert_eq!((heard.seq.0, *waiting), (3001, 1));
    fake.push(&ConsoleEvent::Message(message(3001)));
    fake.push(&ConsoleEvent::Message(message(3002)));
    let seen = wait_for(&mut updates, |u| matches!(u, Update::Waiting { .. }));
    let Some(Update::Waiting {
        message: heard,
        waiting,
    }) = seen.last()
    else {
        panic!("no waiting update in {seen:?}");
    };
    assert_eq!(
        (heard.seq.0, *waiting),
        (3002, 2),
        "a redelivery is not counted"
    );
    let s = backend.snapshot();
    assert_eq!(seqs(&s.messages), (501..=2500).collect::<Vec<_>>());
    assert_eq!(s.waiting, 2);

    // The newest page takes them, and the count restarts.
    backend.load_newest();
    wait_for(
        &mut updates,
        |u| matches!(u, Update::Snapshot(s) if !s.newer),
    );
    assert_eq!(backend.snapshot().waiting, 0);
}

#[test]
fn held_counts_each_waiting_message_once() {
    let mut held = Held::default();
    held.merge((1..=2000).map(message).collect(), true);
    held.newer = true;
    assert_eq!(held.insert(message(2001)), Added::Waiting);
    assert_eq!(held.insert(message(2001)), Added::Nothing);
    assert_eq!(held.insert(message(1500)), Added::Nothing);
    assert_eq!(held.insert(message(2002)), Added::Waiting);
    assert_eq!(held.waiting, 2);
    assert_eq!(held.to_vec().len(), 2000);
}

#[test]
fn a_page_overlapping_the_chat_adds_each_message_once() {
    let mut held = Held::default();
    held.merge((501..=1000).map(message).collect(), true);
    // agmend's page reaches into what is held.
    assert!(held.prepend(Seq(501), (1..=600).map(message).collect(), false));
    assert_eq!(seqs(&held.to_vec()), (1..=1000).collect::<Vec<_>>());
    assert!(!held.older);
}

#[test]
fn a_stale_page_is_dropped() {
    let mut held = Held::default();
    held.merge((501..=1000).map(message).collect(), true);
    // A reconnect replaced the chat while the page was on its way.
    held.merge((5001..=5500).map(message).collect(), true);
    assert!(!held.prepend(Seq(501), (1..=500).map(message).collect(), false));
    assert_eq!(seqs(&held.to_vec()), (5001..=5500).collect::<Vec<_>>());
    assert!(held.older);
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

#[test]
fn spawn_other_agent_is_unsupported() {
    let fake = FakeAgmend::start(console(vec![]));
    let (backend, _updates) = live_backend(&fake);
    let result = backend.act(Action::SpawnWorker {
        agent: "codex".into(),
    });
    assert!(matches!(
        result,
        Err(BackendError::Unsupported("starting a worker on that agent"))
    ));
    assert!(fake.state.lock().unwrap().agent_requests.is_empty());
}

#[test]
fn spawn_worker_launches_claude() {
    for occupied in [false, true] {
        let fake = FakeAgmend::start(console(vec![]));
        fake.edit(|state| {
            let agents = if occupied {
                vec![json!({"identity": "idmanaged", "project": PROJECT,
                    "handle": "andy/worker-1", "state": "offline"})]
            } else {
                vec![]
            };
            state.agent_list_answer = Some(json!({"agents": agents}));
        });
        let root = occupied.then(|| Path::new(ROOT));
        let backend = AgmendBackend::connect_with(&fake.path(), PROJECT, root, FAST).unwrap();
        let mut updates = backend.subscribe();
        wait_for(&mut updates, is_live);
        if occupied {
            wait_for(&mut updates, |update| {
                matches!(update, Update::Snapshot(snapshot)
                if snapshot.managed_agents.contains_key(&ident("idmanaged")))
            });
        }
        backend
            .act(Action::SpawnWorker {
                agent: "claude".into(),
            })
            .unwrap();
        until(|| !fake.state.lock().unwrap().agent_requests.is_empty());
        let state = fake.state.lock().unwrap();
        assert_eq!(state.agent_requests.len(), 1);
        let request = &state.agent_requests[0];
        assert_eq!(request.method, "agent_launch");
        let mut expected = json!({"project": PROJECT,
            "name": if occupied { "worker-2" } else { "worker-1" }, "role": "engineer"});
        if occupied {
            expected["root"] = json!(ROOT);
        }
        assert_eq!(request.params, expected);
        assert!(request.params.get("model").is_none());
        assert!(request.params.get("budget_usd").is_none());
    }
}

#[test]
fn spawn_worker_uses_the_first_free_name_in_this_project() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.agent_list_answer = Some(json!({"agents": [
            {"identity": "idworker1", "project": PROJECT,
             "handle": "andy/worker-1", "state": "idle"},
            {"identity": "idworker3", "project": PROJECT,
             "handle": "andy/worker-3", "state": "offline"},
            {"identity": "idother0", "project": "another-project",
             "handle": "andy/worker-2", "state": "running"}
        ]}));
    });
    let (backend, mut updates) = live_backend(&fake);
    wait_for(
        &mut updates,
        |update| matches!(update, Update::Snapshot(snapshot) if snapshot.managed_agents.len() == 2),
    );
    backend
        .act(Action::SpawnWorker {
            agent: "claude".into(),
        })
        .unwrap();
    until(|| !fake.state.lock().unwrap().agent_requests.is_empty());
    let state = fake.state.lock().unwrap();
    assert_eq!(state.agent_requests.len(), 1);
    assert_eq!(state.agent_requests[0].params["name"], "worker-2");
}

#[test]
fn spawn_worker_refuses_when_all_99_names_are_taken() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        let agents: Vec<_> = (1..=99)
            .map(|n| {
                json!({"identity": format!("idwork{n:03}"), "project": PROJECT,
                "handle": format!("andy/worker-{n}"), "state": "offline"})
            })
            .collect();
        state.agent_list_answer = Some(json!({"agents": agents}));
    });
    let (backend, mut updates) = live_backend(&fake);
    wait_for(
        &mut updates,
        |update| matches!(update, Update::Snapshot(snapshot) if snapshot.managed_agents.len() == 99),
    );
    assert!(matches!(
        backend.act(Action::SpawnWorker {
            agent: "claude".into()
        }),
        Err(BackendError::Unsupported("too many workers"))
    ));
    assert!(fake.state.lock().unwrap().agent_requests.is_empty());
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

#[test]
fn shutdown_owned_waits_for_exit_and_has_a_deadline() {
    use std::os::unix::net::UnixListener;
    for exits in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agmend.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let socket = path.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let hello: Request = frame::read(&mut stream).unwrap();
            assert_eq!(hello.method, "hello");
            frame::write(
                &mut stream,
                &Response::ok(
                    hello.id,
                    json!({"api":API,"version":env!("CARGO_PKG_VERSION"),"exe":"/app/agmend"}),
                ),
            )
            .unwrap();
            let stop: Request = frame::read(&mut stream).unwrap();
            assert_eq!(stop.method, "shutdown");
            frame::write(&mut stream, &Response::ok(stop.id, json!({}))).unwrap();
            if exits {
                std::thread::sleep(Duration::from_millis(80));
                std::fs::remove_file(socket).unwrap();
            }
        });
        let started = Instant::now();
        let result = AgmendBackend::shutdown_owned(
            &path,
            Path::new("/app/agmend"),
            Duration::from_millis(200),
        );
        assert_eq!(result.is_ok(), exits);
        assert!(started.elapsed() >= Duration::from_millis(80));
        assert!(started.elapsed() < Duration::from_secs(2));
        server.join().unwrap();
    }
}

#[test]
fn shutdown_owned_refuses_foreign_executable() {
    let fake = FakeAgmend::start(console(vec![]));
    assert!(
        AgmendBackend::shutdown_owned(
            &fake.path(),
            Path::new("/our/agmend"),
            Duration::from_secs(15)
        )
        .is_err()
    );
    assert!(AgmendBackend::probe(&fake.path()).is_ok());
}

#[test]
fn managed_agent_actions_use_local_launch_and_stop() {
    let fake = FakeAgmend::start(console(vec![]));
    let (backend, mut updates) = live_backend(&fake);
    backend
        .act(Action::LaunchAgent {
            name: "worker".into(),
            model: "sonnet".into(),
            budget_cents: 500,
        })
        .unwrap();
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.contains_key(&ident("idmanaged")))
    });
    assert_eq!(
        fake.state.lock().unwrap().agent_requests[0].method,
        "agent_launch"
    );
    assert_eq!(
        fake.state.lock().unwrap().agent_requests[0].params,
        json!({"project": PROJECT, "name": "worker", "model": "sonnet", "budget_usd": 5.0})
    );
    backend.act(Action::StopAgent(ident("idmanaged"))).unwrap();
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.get(&ident("idmanaged")).is_some_and(|s| s == "offline"))
    });
    let requests = &fake.state.lock().unwrap().agent_requests;
    assert_eq!(requests[1].method, "agent_stop");
    assert_eq!(requests[1].params, json!({"identity": "idmanaged"}));
}

#[test]
fn managed_agent_polling_updates_running_and_offline_states() {
    let fake = FakeAgmend::start(console(vec![]));
    let (backend, mut updates) = live_backend(&fake);
    fake.edit(|state| state.managed_state = Some("running".into()));
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.get(&ident("idmanaged")).is_some_and(|state| state == "running"))
    });
    assert!(
        backend
            .snapshot()
            .roster
            .iter()
            .flat_map(|group| &group.agents)
            .any(|agent| agent.id == ident("idmanaged") && agent.status == AgentStatus::Working)
    );
    fake.edit(|state| state.managed_state = Some("offline".into()));
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.get(&ident("idmanaged")).is_some_and(|state| state == "offline"))
    });
}

#[test]
fn managed_agents_remain_stoppable_without_hub_roster() {
    let mut snapshot = console(vec![]);
    snapshot.roster.clear();
    let fake = FakeAgmend::start(snapshot);
    fake.edit(|state| state.managed_state = Some("idle".into()));
    let (backend, mut updates) = live_backend(&fake);
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.contains_key(&ident("idmanaged")))
    });
    assert!(backend.snapshot().roster.iter().any(|group| {
        group.you
            && group
                .agents
                .iter()
                .any(|agent| agent.id == ident("idmanaged"))
    }));
}

#[test]
fn managed_status_polling_is_not_starved_by_other_jobs() {
    let fake = FakeAgmend::start(console(vec![]));
    let (backend, _updates) = live_backend(&fake);
    fake.edit(|state| state.managed_state = Some("running".into()));
    for _ in 0..60 {
        let (reply, _result) = futures::channel::oneshot::channel();
        backend
            .jobs
            .try_send(Job::FileText {
                worktree: "worker".into(),
                path: "src/main.rs".into(),
                reply,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        backend
            .snapshot()
            .managed_agents
            .get(&ident("idmanaged"))
            .map(String::as_str),
        Some("running")
    );
}

#[test]
fn agent_menu_actions_reach_agmend_with_the_project() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| state.managed_state = Some("offline".into()));
    let (backend, mut updates) = live_backend(&fake);
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_work.get(&ident("idmanaged")).is_some_and(|work| {
            work.branch.as_deref() == Some("agent/worker-1234") && !work.joined
        }))
    });
    backend
        .act(Action::StartAgent {
            identity: ident("idmanaged"),
            root: Some("/work/sol".into()),
        })
        .unwrap();
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.get(&ident("idmanaged")).is_some_and(|s| s == "idle"))
    });
    backend
        .act(Action::AttachAgent {
            identity: ident("idmanaged"),
            root: None,
        })
        .unwrap();
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.get(&ident("idmanaged")).is_some_and(|s| s == "attached"))
    });
    backend
        .act(Action::DetachAgent(ident("idmanaged")))
        .unwrap();
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.managed_agents.get(&ident("idmanaged")).is_some_and(|s| s == "idle"))
    });
    backend
        .act(Action::RemoveAgent {
            identity: ident("idmanaged"),
            root: None,
        })
        .unwrap();
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if !snapshot.managed_agents.contains_key(&ident("idmanaged")))
    });
    let requests: Vec<(String, Value)> = fake
        .state
        .lock()
        .unwrap()
        .agent_requests
        .iter()
        .map(|request| (request.method.clone(), request.params.clone()))
        .collect();
    assert_eq!(
        requests,
        vec![
            (
                "agent_start".into(),
                json!({"identity": "idmanaged", "project": PROJECT, "root": "/work/sol"})
            ),
            (
                "agent_attach".into(),
                json!({"identity": "idmanaged", "project": PROJECT})
            ),
            ("agent_detach".into(), json!({"identity": "idmanaged"})),
            (
                "agent_remove".into(),
                json!({"identity": "idmanaged", "project": PROJECT})
            ),
        ]
    );
}

#[test]
fn start_that_skipped_previous_context_says_so() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| state.agent_answer = json!({"state": "idle", "context": "skipped_others"}));
    let (backend, mut updates) = live_backend(&fake);
    backend
        .act(Action::StartAgent {
            identity: ident("idmanaged"),
            root: Some("/work/sol".into()),
        })
        .unwrap();
    wait_for(&mut updates, |update| {
        matches!(update, Update::Snapshot(snapshot)
        if snapshot.agent_notice.as_deref()
            == Some("Started fresh: its last session included messages from other people."))
    });
    fake.edit(|state| state.agent_answer = json!({"state": "idle", "context": "resumed"}));
    backend
        .act(Action::StartAgent {
            identity: ident("idmanaged"),
            root: None,
        })
        .unwrap();
    wait_for(
        &mut updates,
        |update| matches!(update, Update::Snapshot(snapshot) if snapshot.agent_notice.is_none()),
    );
}
