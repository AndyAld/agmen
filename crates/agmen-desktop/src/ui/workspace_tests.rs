use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{STATUS_BAR_HEIGHT, TAB_BAR_HEIGHT, TITLE_BAR_HEIGHT};
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
    init(cx);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let mut snapshot = chat_snapshot();
        snapshot.status.needs_you = 1;
        Workspace::new(snapshot, window, cx)
    });
    redraw(cx);
    assert!(cx.debug_bounds("needs-you-pill").is_some());

    let mut quiet = TestAppContext::single();
    let (_, quiet) = open(&mut quiet);
    assert!(quiet.debug_bounds("needs-you-pill").is_none());
}

#[gpui::test]
fn bars_include_their_rules(cx: &mut TestAppContext) {
    // Mockup check V61: each rule is drawn inside its bar, so the bars keep
    // the mockup's sizes.
    let (_workspace, cx) = open(cx);
    let height = |cx: &mut gpui::VisualTestContext, sel: &'static str| {
        cx.debug_bounds(sel)
            .unwrap_or_else(|| panic!("no {sel}"))
            .size
            .height
    };
    assert_eq!(height(cx, "title-bar"), TITLE_BAR_HEIGHT);
    assert_eq!(height(cx, "status-bar"), STATUS_BAR_HEIGHT);
    assert_eq!(height(cx, "dock-tabs"), TAB_BAR_HEIGHT);
    assert_eq!(height(cx, "right-dock-head"), TAB_BAR_HEIGHT);
    let left = cx.debug_bounds("left-dock").unwrap();
    assert_eq!(left.size.width, LEFT_DOCK_WIDTH);
    let tabs = cx.debug_bounds("dock-tabs").unwrap();
    assert_eq!(
        tabs.origin.y, TITLE_BAR_HEIGHT,
        "the body starts right under the title bar"
    );
}
