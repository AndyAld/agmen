use super::*;
use crate::appearance::Appearance;

#[gpui::test]
fn terminal_agent_renders_through_chip_button_and_opens(cx: &mut gpui::TestAppContext) {
    use crate::backend::{Backend as _, FixtureBackend, Screen, fixture_id};
    use crate::ui::test_support::{click, init, redraw};
    init(cx);
    let snapshot = FixtureBackend::for_screen(Screen::ChatTerminal)
        .unwrap()
        .snapshot();
    let fx1 = fixture_id("andy/fx1");
    let take_over = snapshot.take_overs[&fx1].clone();
    let (_, cx) = cx.add_window_view(|_, cx| {
        TerminalView::new(
            fx1,
            "andy/fx1",
            "worker",
            take_over,
            snapshot.installed_agents,
            snapshot.agent_scans,
            cx,
        )
    });
    redraw(cx);
    assert!(cx.debug_bounds("chip-button:terminal:agent").is_some());
    click(cx, "terminal:agent");
    for _ in 0..3 {
        cx.run_until_parked();
        redraw(cx);
    }
    let menu = cx.debug_bounds("switch-menu").unwrap();
    let chip = cx.debug_bounds("terminal:agent").unwrap();
    assert_eq!(menu.left(), chip.left());
    assert!(menu.bottom() <= chip.top() || menu.top() >= chip.bottom());
}

fn cell(ch: char, fg: TermColor, bold: bool) -> TermCell {
    TermCell {
        ch,
        fg,
        bg: TermColor::Default,
        bold,
    }
}

#[test]
fn cells_of_one_look_make_one_run() {
    let red = TermColor::Ansi(1);
    let cells = [
        cell('a', red, false),
        cell('b', red, false),
        cell('c', red, true),
        cell(' ', TermColor::Default, false),
    ];
    let runs = row_runs(&cells, None);
    let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["ab", "c", " "]);
    assert!(runs[1].bold);
}

#[test]
fn the_cursor_is_a_run_of_its_own() {
    let cells = [
        cell('>', TermColor::Default, true),
        cell(' ', TermColor::Default, false),
        cell(' ', TermColor::Default, false),
    ];
    let runs = row_runs(&cells, Some(1));
    let texts: Vec<(&str, bool)> = runs.iter().map(|r| (r.text.as_str(), r.cursor)).collect();
    assert_eq!(texts, [(">", false), (" ", true), (" ", false)]);
}

#[test]
fn cell_colors_come_from_the_theme_or_the_program() {
    let theme = Appearance::builtin().theme;
    let fg = theme.terminal_foreground;
    assert_eq!(term_color(TermColor::Default, fg, &theme), fg);
    assert_eq!(term_color(TermColor::Ansi(8), fg, &theme), theme.ansi[8]);
    assert_eq!(term_color(TermColor::Ansi(99), fg, &theme), fg);
    let orange = term_color(TermColor::Rgb([0xea, 0x89, 0x4b]), fg, &theme).to_rgb();
    assert!((orange.r - 0xea as f32 / 255.).abs() < 0.002);
}

#[test]
fn the_head_says_how_the_session_started() {
    let resumed = SessionStart::Resumed("3f9c2e1a-7b40-4c1e-9d6a-2f0e8c5b1a77".into());
    assert_eq!(start_label(&resumed), "--resume 3f9c2e1a");
    assert_eq!(
        start_label(&SessionStart::Handoff),
        "new session · handoff brief"
    );
}

mod check_again {
    use gpui::{Entity, TestAppContext, VisualTestContext};

    use super::*;
    use crate::backend::{Backend as _, FixtureBackend, Screen, Snapshot, fixture_id};
    use crate::ui::test_support::{init, redraw};

    /// fx1's terminal over the take-over fixture, with grok's add dialog
    /// open over the menu.
    fn open(cx: &mut TestAppContext) -> (Entity<TerminalView>, Snapshot, &mut VisualTestContext) {
        init(cx);
        // Escape closes the menu and the dialog through src/keymap.rs.
        cx.update(crate::keymap::bind_all);
        let snapshot = FixtureBackend::for_screen(Screen::ChatTerminal)
            .unwrap()
            .snapshot();
        let fx1 = fixture_id("andy/fx1");
        let take_over = snapshot.take_overs[&fx1].clone();
        let (agents, scans) = (snapshot.installed_agents.clone(), snapshot.agent_scans);
        let (view, cx) = cx.add_window_view(|_, cx| {
            TerminalView::new(fx1, "andy/fx1", "worker", take_over, agents, scans, cx)
        });
        view.update(cx, |v, cx| {
            v.open_dialog(AgentDialog::Add("grok".into()), cx)
        });
        settle(cx);
        (view, snapshot, cx)
    }

    fn settle(cx: &mut VisualTestContext) {
        for _ in 0..3 {
            cx.run_until_parked();
            redraw(cx);
        }
    }

    /// The fixture's agents with grok found.
    fn grok_ready(snapshot: &Snapshot) -> Vec<InstalledAgent> {
        let mut agents = snapshot.installed_agents.clone();
        for agent in &mut agents {
            if agent.name == "grok" {
                agent.found = AgentFound::Ready {
                    version: "1.0.0".into(),
                };
            }
        }
        agents
    }

    #[gpui::test]
    fn an_update_before_the_scans_result_leaves_the_check_pending(cx: &mut TestAppContext) {
        // Review: a chat message's update must not answer Check again.
        let (view, snapshot, cx) = open(cx);
        let take_over = snapshot.take_overs[&fixture_id("andy/fx1")].clone();
        view.update(cx, |v, cx| v.check_again(cx));
        let stale = snapshot.installed_agents.clone();
        view.update(cx, |v, cx| {
            v.set_state(take_over.clone(), stale, snapshot.agent_scans, cx);
        });
        view.read_with(cx, |v, _| {
            assert!(v.dialog.is_some());
            assert_eq!(v.note, None, "no verdict before the scan's result");
        });
        let scanned = snapshot.agent_scans + 1;
        view.update(cx, |v, cx| {
            v.set_state(take_over, grok_ready(&snapshot), scanned, cx);
        });
        view.read_with(cx, |v, _| assert!(v.dialog.is_none(), "grok is ready"));
        // The dialog is gone; Escape still reaches the menu (review).
        settle(cx);
        assert!(cx.debug_bounds("switch-menu").is_some());
        cx.simulate_keystrokes("escape");
        settle(cx);
        assert!(cx.debug_bounds("switch-menu").is_none());
    }

    #[gpui::test]
    fn a_failed_scan_says_why(cx: &mut TestAppContext) {
        let (view, snapshot, cx) = open(cx);
        let take_over = snapshot.take_overs[&fixture_id("andy/fx1")].clone();
        view.update(cx, |v, cx| {
            v.check_again(cx);
            v.scan_failed("agmend is not running".into(), cx);
        });
        view.read_with(cx, |v, _| {
            assert_eq!(v.note.as_deref(), Some("agmend is not running"));
        });
        // A later scan does not overwrite it: nothing is pending.
        view.update(cx, |v, cx| {
            v.set_state(
                take_over,
                grok_ready(&snapshot),
                snapshot.agent_scans + 1,
                cx,
            );
        });
        view.read_with(cx, |v, _| {
            assert!(v.dialog.is_some());
            assert_eq!(v.note.as_deref(), Some("agmend is not running"));
        });
    }
}
