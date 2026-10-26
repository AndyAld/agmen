use std::path::Path;

use agmen_proto::{Payload, Urgency};
use futures::StreamExt as _;

use super::*;
use crate::backend::{Action, Backend, ChangeState, Draft, Screen, Update};

fn chat() -> Snapshot {
    FixtureBackend::for_screen(Screen::Chat).snapshot()
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
        for m in FixtureBackend::for_screen(screen).snapshot().messages {
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
    // Mockup fixes 29, 34 and 36.
    let chat = chat();
    assert_eq!(chat.status.worktrees, 5);
    assert_eq!(chat.status.alerts, 1);
    let total = chat.listener.workers_today_usd + chat.listener.listener_today_usd;
    assert!(
        (total - chat.status.spend_today_usd).abs() < 0.005,
        "{total}"
    );
    let needs = FixtureBackend::for_screen(Screen::NeedsYou).snapshot();
    assert_eq!((needs.status.alerts, needs.status.needs_you), (2, 1));
    let answered = FixtureBackend::for_screen(Screen::NeedsYouAnswered).snapshot();
    assert_eq!((answered.status.alerts, answered.status.needs_you), (1, 0));
}

#[test]
fn needs_you_has_an_urgent_alert_for_me() {
    let s = FixtureBackend::for_screen(Screen::NeedsYou).snapshot();
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
    let backend = FixtureBackend::for_screen(Screen::Chat);
    let mut updates = backend.subscribe();
    backend
        .send(Draft {
            to: vec![backend.snapshot().listener_id.clone()],
            body: "hi".into(),
            reply_to: None,
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
    let backend = FixtureBackend::for_screen(Screen::Chat);
    let err = backend
        .send(Draft {
            to: Vec::new(),
            body: "x".repeat(agmen_proto::MAX_BODY_BYTES + 1),
            reply_to: None,
        })
        .unwrap_err();
    assert!(err.to_string().contains("32768"), "{err}");
}

#[test]
fn fixture_apply_changes_card_state() {
    let backend = FixtureBackend::for_screen(Screen::SettingsFromChat);
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
        "mockup fix 44: Undo returns to pending"
    );
    assert!(
        backend
            .act(Action::Apply(crate::backend::ChangeId("nope".into())))
            .is_err()
    );
}

#[test]
fn settings_applied_screen_starts_applied() {
    let s = FixtureBackend::for_screen(Screen::SettingsApplied).snapshot();
    assert_eq!(
        s.changes.values().next().unwrap().state,
        ChangeState::Applied
    );
}

#[test]
fn plan_fixture_team_and_shares() {
    let plan = FixtureBackend::for_screen(Screen::Plan)
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
        let s = FixtureBackend::for_screen(screen).snapshot();
        for group in &s.roster {
            for agent in &group.agents {
                let _ = super::super::fixture_data::id(&agent.handle);
            }
        }
    }
    assert_eq!(
        super::super::fixture_data::id("andy/fx1").as_str(),
        "andyfx10"
    );
}
