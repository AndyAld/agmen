use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{click, init, redraw};
use gpui::TestAppContext;

fn open(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut gpui::VisualTestContext) {
    init(cx);
    let (workspace, cx) =
        cx.add_window_view(|window, cx| Workspace::new(chat_snapshot(), window, cx));
    workspace.update_in(cx, |w, window, cx| w.focus_handle.focus(window, cx));
    redraw(cx);
    (workspace, cx)
}

#[gpui::test]
fn workspace_toggles_docks(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    workspace.read_with(cx, |w, _| {
        assert!(w.is_dock_open(DockSide::Left));
        assert!(w.is_dock_open(DockSide::Right));
    });

    cx.dispatch_action(ToggleRightDock);
    workspace.read_with(cx, |w, _| {
        assert!(w.is_dock_open(DockSide::Left));
        assert!(!w.is_dock_open(DockSide::Right));
    });

    cx.dispatch_action(ToggleRightDock);
    workspace.read_with(cx, |w, _| assert!(w.is_dock_open(DockSide::Right)));

    cx.dispatch_action(ToggleLeftDock);
    workspace.read_with(cx, |w, _| assert!(!w.is_dock_open(DockSide::Left)));
}

#[gpui::test]
fn status_bar_shows_toggle_only_when_dock_closed(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    assert!(cx.debug_bounds("listener-toggle").is_none());

    click(cx, "close-right-dock");
    workspace.read_with(cx, |w, _| assert!(!w.is_dock_open(DockSide::Right)));
    assert!(cx.debug_bounds("listener-toggle").is_some());
    assert!(cx.debug_bounds("listener-panel").is_none());

    click(cx, "listener-toggle");
    workspace.read_with(cx, |w, _| assert!(w.is_dock_open(DockSide::Right)));
    assert!(cx.debug_bounds("listener-toggle").is_none());
    assert!(cx.debug_bounds("listener-panel").is_some());
}

#[gpui::test]
fn listener_toggle_shows_the_listener_after_an_agent(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    click(cx, "agent:andy/fx1");
    assert!(cx.debug_bounds("agent-panel").is_some());
    click(cx, "close-right-dock");
    click(cx, "listener-toggle");
    assert!(
        cx.debug_bounds("listener-panel").is_some(),
        "the toggle says Listener"
    );
    assert!(cx.debug_bounds("agent-panel").is_none());
    workspace.read_with(cx, |w, _| assert!(w.shown_agent().is_none()));
}

#[gpui::test]
fn rows_without_details_are_not_highlighted(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    click(cx, "agent:andy/fx1");
    click(cx, "agent:sean/a1");
    workspace.read_with(cx, |w, cx| {
        assert!(w.shown_agent().is_none());
        assert_eq!(w.agents_panel.read(cx).selected(), None);
    });
    assert!(cx.debug_bounds("listener-panel").is_some());
}

#[gpui::test]
fn selecting_agent_shows_agent_panel(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    assert!(cx.debug_bounds("listener-panel").is_some());
    assert!(cx.debug_bounds("agent-panel").is_none());

    click(cx, "agent:andy/fx1");
    workspace.read_with(cx, |w, _| {
        assert_eq!(w.shown_agent().map(|a| a.handle.as_str()), Some("andy/fx1"));
        assert_eq!(
            w.shown_agent().map(|a| a.id.as_str()),
            Some("andyfx10"),
            "selection carries the identity id"
        );
    });
    assert!(cx.debug_bounds("agent-panel").is_some());
    assert!(cx.debug_bounds("listener-panel").is_none());

    // The listener has no Agent panel: picking it shows the Listener panel.
    click(cx, "agent:andy/listen");
    assert!(cx.debug_bounds("listener-panel").is_some());
}

#[gpui::test]
fn selecting_agent_opens_closed_dock(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    cx.dispatch_action(ToggleRightDock);
    click(cx, "agent:andy/fx1");
    workspace.read_with(cx, |w, _| assert!(w.is_dock_open(DockSide::Right)));
    assert!(cx.debug_bounds("agent-panel").is_some());
}

#[gpui::test]
fn hide_button_closes_left_dock(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    click(cx, "hide-left-dock");
    workspace.read_with(cx, |w, _| assert!(!w.is_dock_open(DockSide::Left)));
}

#[gpui::test]
fn needs_you_pill_only_above_zero(cx: &mut TestAppContext) {
    use crate::ui::test_support::rendered_size;
    use gpui::IntoElement as _;
    let quiet = rendered_size(cx, || {
        StatusBar::new(&chat_snapshot(), true).into_any_element()
    });
    let mut cx2 = TestAppContext::single();
    let loud = rendered_size(&mut cx2, || {
        let mut snapshot = chat_snapshot();
        snapshot.status.needs_you = 1;
        StatusBar::new(&snapshot, true).into_any_element()
    });
    assert!(loud.width > quiet.width);
}
