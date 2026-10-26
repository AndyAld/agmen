use super::*;
use crate::backend::{Backend, FixtureBackend, Screen, Update, fixture_id};
use futures::StreamExt as _;

fn screen(screen: Screen) -> Snapshot {
    FixtureBackend::for_screen(screen).unwrap().snapshot()
}

fn text(screen: &TerminalScreen, row: u16) -> String {
    screen.row(row).iter().map(|cell| cell.ch).collect()
}

#[test]
fn take_over_resumes_the_session_and_hand_back_ends_it() {
    let mut s = screen(Screen::Chat);
    let fx1 = fixture_id("andy/fx1");
    assert!(s.take_overs.is_empty());
    assert!(can_take_over(&s, &fx1));

    act(&mut s, Action::TakeOver(fx1.clone())).unwrap();
    let t = &s.take_overs[&fx1];
    assert_eq!(t.harness, "claude");
    assert_eq!(t.start, SessionStart::Resumed(FX1_SESSION.into()));
    assert!(text(&t.screen, 0).starts_with(" $ claude --resume 3f9c2e1a"));
    assert_eq!(t.screen.cursor(), Some((16, 4)));
    assert!(!can_take_over(&s, &fx1), "already taken over");

    act(&mut s, Action::HandBack(fx1.clone())).unwrap();
    assert!(s.take_overs.is_empty());
    assert!(act(&mut s, Action::HandBack(fx1)).is_err());
}

#[test]
fn only_agents_with_a_session_here_can_be_taken_over() {
    // andy/a1 is andy's, but has no live session on this machine.
    let mut s = screen(Screen::Chat);
    let a1 = fixture_id("andy/a1");
    assert!(!can_take_over(&s, &a1));
    assert!(act(&mut s, Action::TakeOver(a1)).is_err());
    assert!(s.take_overs.is_empty());
}

#[test]
fn switching_starts_a_handoff_session_and_moves_the_harness() {
    let mut s = screen(Screen::ChatTerminal);
    let fx1 = fixture_id("andy/fx1");
    let switch = |agent: &str| Action::SwitchAgent {
        worker: fx1.clone(),
        agent: agent.into(),
    };
    // grok is not installed; nothing changes.
    assert!(act(&mut s, switch("grok")).is_err());
    assert_eq!(s.take_overs[&fx1].harness, "claude");

    act(&mut s, switch("codex")).unwrap();
    let t = &s.take_overs[&fx1];
    assert_eq!(
        (t.harness.as_str(), &t.start),
        ("codex", &SessionStart::Handoff)
    );
    assert_eq!(text(&t.screen, 2).trim(), "agmen handoff brief");
    let row = s
        .roster
        .iter()
        .flat_map(|g| &g.agents)
        .find(|a| a.id == fx1)
        .unwrap();
    assert_eq!(row.harness, "codex");
    let info = &s.agents[&fx1];
    assert_eq!(info.harness_label, "Codex 0.164.0 · ACP, since 15:52");
    assert_eq!(info.model, "Codex default (standard tier)");
    assert_eq!(
        info.tools,
        ["sandbox workspace-write (Codex has no tool list)"]
    );
    assert!(info.tools_lossy);

    // Back to claude: its tool list again.
    act(&mut s, switch("claude")).unwrap();
    assert!(!s.agents[&fx1].tools_lossy);
    assert_eq!(s.agents[&fx1].tools.len(), 6);
}

#[test]
fn switching_needs_a_taken_over_session() {
    let mut s = screen(Screen::Chat);
    let action = Action::SwitchAgent {
        worker: fixture_id("andy/fx1"),
        agent: "codex".into(),
    };
    assert!(act(&mut s, action).is_err());
}

#[test]
fn installed_agents_follow_the_mockup() {
    // console.py INSTALLED: agy is too old for JSON, grok is missing.
    let s = screen(Screen::Chat);
    let names: Vec<&str> = s.installed_agents.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        ["claude", "codex", "opencode", "gemini", "agy", "grok"]
    );
    assert_eq!(
        s.installed_agents[4].found,
        AgentFound::Outdated {
            version: "1.1.7".into(),
            minimum: "1.1.15".into(),
            why: "JSON".into()
        }
    );
    let grok = &s.installed_agents[5];
    assert_eq!(grok.found, AgentFound::Missing);
    let source = grok.source.as_ref().unwrap();
    assert_eq!(source.url, "github.com/xai-org/grok-build");
    assert!(
        source
            .look_alike
            .as_deref()
            .unwrap()
            .contains("@vibe-kit/grok-cli")
    );
}

#[test]
fn take_over_screens_show_fx1_taken_over() {
    // V66: the four take-over frames.
    let fx1 = fixture_id("andy/fx1");
    for shown in [Screen::ChatTerminal, Screen::SwitchAgent, Screen::AddAgent] {
        let s = screen(shown);
        assert_eq!(s.take_overs[&fx1].harness, "claude", "{shown:?}");
        assert!(!s.agents[&fx1].tools_lossy);
    }
    let s = screen(Screen::ChatCodex);
    assert_eq!(s.take_overs[&fx1].harness, "codex");
    assert!(s.agents[&fx1].tools_lossy);
    assert!(screen(Screen::Chat).take_overs.is_empty());
}

#[test]
fn fixture_publishes_take_over_and_hand_back() {
    let backend = FixtureBackend::for_screen(Screen::Chat).unwrap();
    let mut updates = backend.subscribe();
    let fx1 = fixture_id("andy/fx1");
    backend.act(Action::TakeOver(fx1.clone())).unwrap();
    let Some(Update::Snapshot(s)) = futures::executor::block_on(updates.next()) else {
        panic!("no snapshot");
    };
    assert!(s.take_overs.contains_key(&fx1));
    backend.act(Action::HandBack(fx1)).unwrap();
    assert!(backend.snapshot().take_overs.is_empty());
    backend.act(Action::ScanAgents).unwrap();
}

#[test]
fn taking_over_a_switched_agent_starts_from_the_handoff() {
    // Review: codex must not resume Claude's session id.
    let mut s = screen(Screen::ChatTerminal);
    let fx1 = fixture_id("andy/fx1");
    act(
        &mut s,
        Action::SwitchAgent {
            worker: fx1.clone(),
            agent: "codex".into(),
        },
    )
    .unwrap();
    act(&mut s, Action::HandBack(fx1.clone())).unwrap();
    act(&mut s, Action::TakeOver(fx1.clone())).unwrap();
    let t = &s.take_overs[&fx1];
    assert_eq!(
        (t.harness.as_str(), &t.start),
        ("codex", &SessionStart::Handoff)
    );
}

#[test]
fn a_scan_counts_itself() {
    // Check again waits for this count to move (review).
    let mut s = screen(Screen::ChatTerminal);
    let before = s.agent_scans;
    act(&mut s, Action::ScanAgents).unwrap();
    assert_eq!(s.agent_scans, before + 1);
}
