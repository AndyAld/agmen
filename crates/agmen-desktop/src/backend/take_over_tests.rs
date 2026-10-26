use super::*;

fn text(screen: &TerminalScreen, row: u16) -> String {
    screen.row(row).iter().map(|cell| cell.ch).collect()
}

#[test]
fn put_clips_at_the_edge_and_new_caps_the_size() {
    let mut g = TerminalScreen::new(4, 2);
    assert_eq!(g.put(0, 2, "abc", TermColor::Ansi(1), true), 5);
    assert_eq!(text(&g, 0), "  ab");
    assert_eq!(g.row(0)[2].fg, TermColor::Ansi(1));
    assert!(g.row(0)[2].bold);
    assert!(g.row(2).is_empty());
    g.set_cursor(5, 0);
    assert_eq!(g.cursor(), None);
    let big = TerminalScreen::new(u16::MAX, u16::MAX);
    assert_eq!((big.cols(), big.rows()), (MAX_COLS, MAX_ROWS));
}

fn agent(found: AgentFound) -> InstalledAgent {
    InstalledAgent {
        name: "agy".into(),
        title: "Antigravity".into(),
        found,
        acp: "agy_acp_server".into(),
        source: None,
    }
}

#[test]
fn only_a_ready_agent_can_run_a_worker_and_only_a_missing_one_is_unlisted() {
    // C261: one rule for the palette, the switch menu and the model picker.
    let ready = agent(AgentFound::Ready {
        version: "1.1.15".into(),
    });
    assert!(ready.is_installed());
    assert_eq!(ready.not_ready(), None);
    let outdated = agent(AgentFound::Outdated {
        version: "1.1.7".into(),
        minimum: "1.1.15".into(),
        why: "JSON".into(),
    });
    assert!(outdated.is_installed());
    assert_eq!(
        outdated.not_ready().as_deref(),
        Some("update to 1.1.15+ for JSON")
    );
    // A probe that timed out or printed nothing parseable: on PATH, so it
    // is listed, but agmen cannot tell whether it is new enough.
    let unknown = agent(AgentFound::VersionUnknown);
    assert!(unknown.is_installed());
    assert_eq!(unknown.version(), None);
    assert_eq!(
        unknown.not_ready().as_deref(),
        Some("could not read its version")
    );
    let missing = agent(AgentFound::Missing);
    assert!(!missing.is_installed());
    assert_eq!(missing.not_ready().as_deref(), Some("not installed"));
}
