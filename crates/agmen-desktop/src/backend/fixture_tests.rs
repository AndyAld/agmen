use std::path::Path;

use agmen_proto::{Payload, Urgency};
use futures::StreamExt as _;

use super::*;
use crate::backend::{Action, Backend, ChangeState, Draft, Screen, Update};

fn chat() -> Snapshot {
    FixtureBackend::for_screen(Screen::Chat).unwrap().snapshot()
}

fn handles(s: &Snapshot) -> Vec<&str> {
    s.messages.iter().map(|m| m.from_handle.as_str()).collect()
}

#[test]
fn fixture_chat_matches_mockup() {
    let s = chat();
    assert_eq!(s.messages.len(), 9);
    assert_eq!(
        handles(&s),
        [
            "andy/a1",
            "andy/a1",
            "andy",
            "andy/listen",
            "andy/listen",
            "hub",
            "andy/fx1",
            "andy/rv2",
            "sean"
        ]
    );
    let verbs: Vec<&str> = s
        .messages
        .iter()
        .map(|m| {
            m.client
                .payload
                .verb()
                .map_or("?", agmen_proto::Verb::as_str)
        })
        .collect();
    assert_eq!(
        verbs,
        [
            "DONE",
            "ALERT",
            "FYI",
            "ANSWER",
            "DELEGATED",
            "GRANT",
            "DONE",
            "REVIEW",
            "FYI"
        ]
    );
    assert_eq!(
        s.messages[1].client.body.as_str(),
        "main does not compile: 4d9fdc8 committed conflict markers at src/main.rs:2-8. \
         @sean/a1 please hold pushes to main until the fix lands."
    );
    assert_eq!(s.messages[5].from_kind, agmen_proto::IdentityKind::Hub);
    // The listener's answer threads under andy's message.
    assert_eq!(
        s.messages[3].client.reply_to,
        Some(s.messages[2].client.id.clone())
    );
}

#[test]
fn fixture_messages_survive_the_wire_format() {
    for screen in Screen::ALL {
        for m in FixtureBackend::for_screen(screen)
            .unwrap()
            .snapshot()
            .messages
        {
            let json = serde_json::to_string(&m).unwrap();
            let back: agmen_proto::StampedEnvelope = serde_json::from_str(&json).unwrap();
            assert_eq!(back, m, "{screen:?}");
            assert!(
                !matches!(m.client.payload, Payload::Unknown { .. }),
                "{screen:?}"
            );
        }
    }
}

#[test]
fn fixture_roster_groups() {
    let s = chat();
    let counts: Vec<(&str, usize)> = s
        .roster
        .iter()
        .map(|g| (g.person.as_str(), g.agents.len()))
        .collect();
    assert_eq!(counts, [("andy", 5), ("sean", 2), ("adam", 2)]);
    let rv2 = &s.roster[0].agents[3];
    assert_eq!(
        (rv2.name.as_str(), rv2.role.as_str(), rv2.depth),
        ("rv2", "reviewer", 2)
    );
    // Mockup fix 28: rv2 runs on a harness andy has installed, not grok.
    assert_eq!(rv2.harness, "opencode");
}

#[test]
fn status_counts_follow_one_rule() {
    // Mockup fixes 29, 34 and 36. The alert and needs-you counts are not
    // stored: the status bar derives them from the messages (Task 13).
    use crate::alerts::{AlertState, Me, alert_count};
    let chat = chat();
    assert_eq!(chat.status.worktrees, 5);
    let total = chat.listener.workers_today_usd + chat.listener.listener_today_usd;
    assert!(
        (total - chat.status.spend_today_usd).abs() < 0.005,
        "{total}"
    );
    for (screen, alerts, needs_you) in [
        (Screen::Chat, 1, 0),
        (Screen::NeedsYou, 2, 1),
        (Screen::NeedsYouAnswered, 1, 0),
    ] {
        let s = FixtureBackend::for_screen(screen).unwrap().snapshot();
        let me = Me::from(&s);
        let mut state = AlertState::default();
        for m in &s.messages {
            state.on_message(m, &me);
        }
        assert_eq!(
            (alert_count(&s.messages, &me), state.badge()),
            (alerts, needs_you),
            "{screen:?}"
        );
    }
}

// Task 13: answering an alert.

#[test]
fn answering_an_alert_posts_my_answer() {
    let backend = FixtureBackend::for_screen(Screen::NeedsYou).unwrap();
    let alert = backend.snapshot().messages.last().unwrap().clone();
    let mut updates = backend.subscribe();
    backend
        .act(Action::AnswerAlert {
            alert: alert.client.id.clone(),
            choice: "Revert #10".into(),
        })
        .unwrap();
    let Some(Update::Message(m)) = futures::executor::block_on(updates.next()) else {
        panic!("no update");
    };
    assert!(matches!(m.client.payload, Payload::Answer {}));
    assert_eq!(m.client.reply_to.as_ref(), Some(&alert.client.id));
    assert_eq!(m.client.body.as_str(), "Revert #10");
    assert_eq!(m.client.to, vec![alert.from.clone()]);
    assert_eq!(m.from_handle, "andy");
    assert_eq!(backend.snapshot().messages.last(), Some(&*m));
}

#[test]
fn answering_an_unknown_alert_is_refused() {
    let backend = FixtureBackend::for_screen(Screen::Chat).unwrap();
    let before = backend.snapshot().messages.len();
    let missing = backend.act(Action::AnswerAlert {
        alert: "01JZ0000000000000000000000".parse().unwrap(),
        choice: "Revert #10".into(),
    });
    assert!(missing.is_err());
    assert_eq!(backend.snapshot().messages.len(), before);
}

#[test]
fn a_reply_is_an_answer() {
    let backend = FixtureBackend::for_screen(Screen::NeedsYou).unwrap();
    let alert = backend.snapshot().messages.last().unwrap().clone();
    backend
        .send(Draft {
            to: vec![alert.from.clone()],
            body: "Reverting".into(),
            reply_to: Some(alert.client.id.clone()),
            model: None,
        })
        .unwrap();
    let sent = backend.snapshot().messages.last().unwrap().clone();
    assert!(matches!(sent.client.payload, Payload::Answer {}));
    assert_eq!(sent.client.reply_to, Some(alert.client.id));
}

#[test]
fn needs_you_has_an_urgent_alert_for_me() {
    let s = FixtureBackend::for_screen(Screen::NeedsYou)
        .unwrap()
        .snapshot();
    let alert = s.messages.last().unwrap();
    let Payload::Alert {
        urgency, actions, ..
    } = &alert.client.payload
    else {
        panic!("{:?}", alert.client.payload);
    };
    assert_eq!(*urgency, Urgency::Urgent);
    assert!(alert.mentioned.contains(&s.me_id));
    assert_eq!(
        actions,
        &["Hotfix: spawn an engineer", "Revert #10", "Snooze 15 min"]
    );
}

#[test]
fn fixture_send_appends_and_notifies() {
    let backend = FixtureBackend::for_screen(Screen::Chat).unwrap();
    let mut updates = backend.subscribe();
    backend
        .send(Draft {
            to: vec![backend.snapshot().listener_id.clone()],
            body: "hi".into(),
            reply_to: None,
            model: None,
        })
        .unwrap();
    let Some(Update::Message(m)) = futures::executor::block_on(updates.next()) else {
        panic!("no update");
    };
    assert_eq!(m.from_handle, "andy");
    assert_eq!(m.client.body.as_str(), "hi");
    assert_eq!(backend.snapshot().messages.len(), 10);
}

#[test]
fn fixture_send_refuses_an_oversized_body() {
    let backend = FixtureBackend::for_screen(Screen::Chat).unwrap();
    let err = backend
        .send(Draft {
            to: Vec::new(),
            body: "x".repeat(agmen_proto::MAX_BODY_BYTES + 1),
            reply_to: None,
            model: None,
        })
        .unwrap_err();
    assert!(err.to_string().contains("32768"), "{err}");
}

#[test]
fn fixture_apply_changes_card_state() {
    let backend = FixtureBackend::for_screen(Screen::SettingsFromChat).unwrap();
    let card = backend.snapshot().changes.values().next().unwrap().clone();
    assert!(card.widens);
    assert_eq!(card.state, ChangeState::Pending);
    backend.act(Action::Apply(card.id.clone())).unwrap();
    let after = backend.snapshot().changes.values().next().unwrap().clone();
    assert_eq!(after.state, ChangeState::Applied);
    backend.act(Action::Undo(card.id.clone())).unwrap();
    let undone = backend.snapshot().changes.values().next().unwrap().clone();
    assert_eq!(
        undone.state,
        ChangeState::Pending,
        "V72: undoing a widening change waits for Apply again (fix 44)"
    );
    assert!(
        backend
            .act(Action::Apply(crate::backend::ChangeId("nope".into())))
            .is_err()
    );
}

#[test]
fn settings_applied_screen_starts_applied() {
    let s = FixtureBackend::for_screen(Screen::SettingsApplied)
        .unwrap()
        .snapshot();
    assert_eq!(
        s.changes.values().next().unwrap().state,
        ChangeState::Applied
    );
}

#[test]
fn settings_screens_lead_recent_with_their_change() {
    // V65: console.py 1504 to 1508 (frames 6674 and 7290).
    let first = |screen| {
        FixtureBackend::for_screen(screen)
            .unwrap()
            .snapshot()
            .listener
            .recent
            .remove(0)
    };
    let pending = first(Screen::SettingsFromChat);
    assert_eq!(
        (pending.time.as_str(), pending.text.as_str(), pending.kind),
        (
            "15:50",
            "waiting for Apply: andy/fx1 lanes and budget",
            RecentKind::Waiting
        )
    );
    let applied = first(Screen::SettingsApplied);
    assert_eq!(
        (applied.time.as_str(), applied.text.as_str(), applied.kind),
        (
            "15:50",
            "changed andy/fx1 lanes and budget (Undo)",
            RecentKind::Applied
        )
    );
}

#[test]
fn plan_fixture_team_and_shares() {
    let plan = FixtureBackend::for_screen(Screen::Plan)
        .unwrap()
        .snapshot()
        .plan
        .unwrap();
    assert_eq!(plan.team.len(), 7);
    let share = |who: &str| -> u32 {
        plan.team
            .iter()
            .filter(|t| t.owner == who)
            .map(|t| t.usd_per_day)
            .sum()
    };
    assert_eq!((share("andy"), share("sean")), (65, 40));
    assert_eq!(plan.lanes.len(), 4);
    let ready = FixtureBackend::for_screen(Screen::PlanReady)
        .unwrap()
        .snapshot()
        .plan
        .unwrap();
    assert!(
        ready
            .specs
            .iter()
            .all(|s| s.path.ends_with("tile-sources.md") || s.approved_by_all())
    );
}

#[test]
fn screen_names_cover_every_render() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/mockups/console");
    let mut stems: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter_map(|n| {
            n.strip_prefix("desktop-")?
                .strip_suffix(".webp")
                .map(str::to_owned)
        })
        .collect();
    stems.sort();
    let mut ours: Vec<String> = Screen::ALL.iter().map(|s| s.name().to_owned()).collect();
    ours.sort();
    assert_eq!(stems, ours);
    for stem in stems {
        assert_eq!(stem.parse::<Screen>().unwrap().name(), stem);
    }
    assert!("nope".parse::<Screen>().is_err());
}

#[test]
fn fixture_handles_make_ids() {
    for screen in Screen::ALL {
        let s = FixtureBackend::for_screen(screen).unwrap().snapshot();
        for group in &s.roster {
            for agent in &group.agents {
                super::super::fixture_data::id(&agent.handle).unwrap();
            }
        }
    }
    assert_eq!(
        super::super::fixture_data::id("andy/fx1").unwrap().as_str(),
        "andyfx10"
    );
    assert!(
        super::super::fixture_data::id("Andy/FX1").is_err(),
        "uppercase is refused"
    );
}

#[test]
fn every_chat_body_matches_console_py() {
    // CHAT in docs/mockups/scripts/console.py, in order.
    let want = [
        "C014 feat/c014-release-matrix@7c8835e · PR #9 · release.yml matrix: windows, macos-26, linux",
        "main does not compile: 4d9fdc8 committed conflict markers at src/main.rs:2-8. @sean/a1 please hold pushes to main until the fix lands.",
        "Spawn another worker to fix main and keep Sean's changes",
        "git_show: Sean's 016f235 and 4d9fdc8 are on main; 4d9fdc8 left markers at src/main.rs:2-8. Delegating.",
        "andy/fx1 · Engineer · claude · sonnet · $5.00 · fix/c015-main-conflict",
        "C015 to andy/fx1, lease 30 min",
        "C015 fix/c015-main-conflict@9e2a71c · cargo test 71 passed, 0 failed · keeps cylinder, document, minimap and tabs from both sides",
        "pass · 0 findings · traced the mod list and both callers",
        "@andy merging #10 after CI. Sorry about the markers.",
    ];
    let bodies: Vec<String> = chat()
        .messages
        .iter()
        .map(|m| m.client.body.as_str().to_owned())
        .collect();
    assert_eq!(bodies, want);
}

#[test]
fn every_roster_row_matches_console_py() {
    use crate::backend::AgentStatus::{Idle, Reading, Working};
    // TREE in console.py, with mockup fix 28 (rv2 on opencode).
    let want = [
        ("andy/listen", "listener", "codex", Idle, 0),
        ("andy/fx1", "engineer", "claude", Working, 1),
        ("andy/t7k", "tester", "opencode", Working, 1),
        ("andy/rv2", "reviewer", "opencode", Reading, 2),
        ("andy/a1", "engineer", "claude", Idle, 0),
        ("sean/a1", "engineer", "codex", Working, 0),
        ("sean/plan", "planner", "gemini", Idle, 0),
        ("adam/coord", "coordinator", "claude", Working, 0),
        ("adam/res", "researcher", "agy", Working, 0),
    ];
    let rows: Vec<_> = chat()
        .roster
        .into_iter()
        .flat_map(|g| g.agents)
        .map(|a| (a.handle, a.role, a.harness, a.status, a.depth))
        .collect();
    let want: Vec<_> = want
        .iter()
        .map(|(h, r, x, s, d)| ((*h).to_owned(), (*r).to_owned(), (*x).to_owned(), *s, *d))
        .collect();
    assert_eq!(rows, want);
}

#[test]
fn settings_screens_use_their_own_listener_text() {
    let body = |screen| {
        let s = FixtureBackend::for_screen(screen).unwrap().snapshot();
        let (id, _) = s.changes.iter().next().unwrap();
        let m = s.messages.iter().find(|m| &m.client.id == id).unwrap();
        m.client.body.as_str().to_owned()
    };
    assert_eq!(
        body(Screen::SettingsFromChat),
        "Both changes widen what andy/fx1 may do, so they wait for your Apply."
    );
    assert_eq!(
        body(Screen::SettingsApplied),
        "Done. andy/fx1 can edit L-INFRA deploy/** until 18:00, and its budget is $8.00."
    );
}

#[test]
fn undoing_a_narrowing_change_ends_undone() {
    // Andy's answer to C93 (V72): narrowing changes apply at once, so Undo
    // leaves a finished, undone card with no buttons.
    let backend = FixtureBackend::for_screen(Screen::SettingsApplied).unwrap();
    let id = {
        let mut state = backend.state.borrow_mut();
        let card = state.changes.values_mut().next().unwrap();
        card.widens = false;
        card.id.clone()
    };
    backend.act(Action::Undo(id)).unwrap();
    let card = backend.snapshot().changes.values().next().unwrap().clone();
    assert_eq!(card.state, ChangeState::Undone);
}

#[test]
fn fx1_session_matches_console_py() {
    // SESSION in console.py, with mockup fix 41 (the edit is +2 -5).
    use crate::backend::SessionRow;
    let s = chat();
    let fx1 = s
        .sessions
        .get(&crate::backend::fixture_id("andy/fx1"))
        .unwrap();
    assert_eq!(fx1.item, "C015");
    assert_eq!(fx1.branch, "fix/c015-main-conflict");
    assert_eq!(fx1.rows.len(), 12);
    let tools: Vec<(&str, &str)> = fx1
        .rows
        .iter()
        .filter_map(|r| match r {
            SessionRow::Tool { name, arg } => Some((name.as_str(), arg.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(tools[0], ("Read", "src/main.rs"));
    assert_eq!(tools[2], ("Edit", "src/main.rs  +2 -5"));
    assert_eq!(tools.len(), 6);
    assert!(matches!(&fx1.rows[9], SessionRow::Cost(t) if t == "turn 4 · $0.31 · 44 s"));
}

#[test]
fn role_files_cover_three_layers() {
    use crate::backend::RoleLayer;
    let s = chat();
    let count = |layer| s.roles.iter().filter(|r| r.layer == layer).count();
    assert_eq!(count(RoleLayer::BuiltIn), 8);
    assert_eq!(count(RoleLayer::Project), 1);
    assert_eq!(count(RoleLayer::Personal), 1);
    let reviewer = s.roles.iter().find(|r| r.name == "reviewer").unwrap();
    assert!(reviewer.text.contains("<role name=\"reviewer\""));
    let info = reviewer.info.as_ref().unwrap();
    assert_eq!(info.tier, crate::roles::Tier::Deep);
    // Mockup fix 51: no adam/sec. Used by comes from the roster, and the
    // roles that extend this one.
    assert!(info.used_by.iter().all(|(who, _)| who != "adam/sec"));
    // Mockup fix 51 (C179): rv2 is reading C015.
    assert_eq!(
        info.used_by,
        [("andy/rv2".to_owned(), "opencode · reading C015".to_owned())]
    );
    // The roles that extend it are their own list, drawn dimmer.
    assert_eq!(info.extended_by, ["security-reviewer".to_owned()]);
}

#[test]
fn role_panels_come_from_the_role_files() {
    // Reviews C163 and C164: every role but base has kind and tier, and the
    // reviewer's verbs and tools are what reviewer.xml says.
    let s = chat();
    for role in &s.roles {
        assert_eq!(role.info.is_some(), role.name != "base", "{}", role.name);
    }
    let info = s
        .roles
        .iter()
        .find(|r| r.name == "reviewer")
        .and_then(|r| r.info.clone())
        .unwrap();
    assert_eq!(info.tools, ["read", "search", "shell"]);
    assert_eq!(info.verbs[0], "REVIEW");
    assert!(info.verbs.iter().any(|v| v == "HELLO"));
    assert!(info.refused.iter().any(|v| v == "CLAIM"));
    assert!(info.refused.iter().all(|v| !info.verbs.contains(v)));
    assert!(
        info.bash_note.is_some(),
        "shell with no edits is read-only by rule"
    );
    assert_eq!(info.mention.0, "@reviewers");
    let engineer = s.roles.iter().find(|r| r.name == "engineer").unwrap();
    assert!(engineer.info.as_ref().unwrap().bash_note.is_none());
}

#[test]
fn status_labels_are_words() {
    // Review C180: the label is spelled out, not taken from Debug.
    use crate::backend::AgentStatus;
    assert_eq!(AgentStatus::Working.label(), "working");
    assert_eq!(AgentStatus::Idle.label(), "idle");
    assert_eq!(AgentStatus::Reading.label(), "reading");
}

#[test]
fn role_dots_take_their_built_in_ancestors_color() {
    // Mockup check: security-reviewer is drawn as a reviewer and docs-writer
    // as an engineer; base has no role color.
    let s = chat();
    let palette = |name: &str| {
        s.roles
            .iter()
            .find(|r| r.name == name)
            .unwrap()
            .palette_role
            .clone()
    };
    assert_eq!(palette("security-reviewer").as_deref(), Some("reviewer"));
    assert_eq!(palette("docs-writer").as_deref(), Some("engineer"));
    assert_eq!(palette("tester").as_deref(), Some("tester"));
    assert_eq!(palette("base"), None);
}

#[test]
fn refused_is_what_the_kind_could_send_but_the_role_cannot() {
    // Ruling D12: Refused is the verbs the hub lets an agent send (its kind
    // cap) minus the role's own; verbs the kind cap blocks drop out.
    let s = chat();
    let info = s
        .roles
        .iter()
        .find(|r| r.name == "reviewer")
        .and_then(|r| r.info.clone())
        .unwrap();
    assert_eq!(
        info.refused,
        // In agmen_proto::Verb::ALL's order.
        [
            "CLAIM",
            "HEARTBEAT",
            "RELEASE",
            "LEND",
            "DONE",
            "PROPOSAL",
            "PUSH_REQUEST"
        ]
    );
}

#[test]
fn git_facts_know_every_worktree_commit() {
    // Review C191: refs confirm commits against git_facts, so every commit
    // the Git panel lists must be there.
    let s = chat();
    for wt in &s.worktrees {
        for c in &wt.commits {
            assert!(s.git.commits.contains(&c.sha.to_string()), "{}", c.sha);
        }
    }
}

// Task 18: the palette's "spawn worker with <agent>" plays the 15:31 exchange.
#[test]
fn spawn_worker_plays_the_delegated_exchange() {
    let backend = FixtureBackend::for_screen(Screen::Chat).unwrap();
    let mut updates = backend.subscribe();
    let before = backend.snapshot().messages.len();
    backend
        .act(Action::SpawnWorker {
            agent: "codex".into(),
        })
        .unwrap();
    let s = backend.snapshot();
    let new = &s.messages[before..];
    let verbs: Vec<_> = new
        .iter()
        .map(|m| m.client.payload.verb().map(agmen_proto::Verb::as_str))
        .collect();
    assert_eq!(verbs, [Some("FYI"), Some("ANSWER"), Some("DELEGATED")]);
    assert_eq!(new[0].from_handle.as_str(), "andy");
    assert_eq!(new[0].client.body.as_str(), "Spawn a worker with codex");
    assert_eq!(new[0].client.to, std::slice::from_ref(&s.listener_id));
    for reply in &new[1..] {
        assert_eq!(reply.from_handle.as_str(), "andy/listen");
        assert_eq!(reply.client.reply_to, Some(new[0].client.id.clone()));
    }
    let Payload::Delegated { worker, agent, .. } = &new[2].client.payload else {
        panic!("not a DELEGATED: {:?}", new[2].client.payload);
    };
    assert_eq!(agent, "codex");
    // andy/fx1 is the fixture's first worker, so this one is andy/fx2.
    assert_eq!(worker.as_str(), "andyfx20");
    assert!(
        new[2]
            .client
            .body
            .as_str()
            .starts_with("andy/fx2 · Engineer · codex")
    );
    // Each message reaches the views as it is added.
    for expected in new {
        let Some(Update::Message(env)) = futures::executor::block_on(updates.next()) else {
            panic!("no message update");
        };
        assert_eq!(&*env, expected);
    }
    // A second spawn gets the next worker.
    backend
        .act(Action::SpawnWorker {
            agent: "claude".into(),
        })
        .unwrap();
    let last = backend.snapshot().messages.pop().unwrap();
    assert!(
        last.client
            .body
            .as_str()
            .starts_with("andy/fx3 · Engineer · claude")
    );
}
