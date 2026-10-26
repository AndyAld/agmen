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

/// The chat the first pane hosts.
fn chat_of(workspace: &Entity<Workspace>, cx: &gpui::VisualTestContext) -> Entity<ChatView> {
    workspace.read_with(cx, |w, cx| {
        w.pane_group.read(cx).panes()[0]
            .read(cx)
            .chat()
            .cloned()
            .expect("the first pane hosts the chat")
    })
}

/// The selector of part `part` of the chat fixture's message `ix`.
fn message_sel(ix: usize, part: &str) -> &'static str {
    let id = chat_snapshot().messages[ix].client.id.clone();
    Box::leak(format!("{}:{part}", id.as_str()).into_boxed_str())
}

#[gpui::test]
fn center_hosts_the_chat_with_its_composer(cx: &mut TestAppContext) {
    let (_workspace, cx) = open(cx);
    assert!(cx.debug_bounds("pane-0:tab-0").is_some());
    assert!(cx.debug_bounds("chat").is_some());
    assert!(cx.debug_bounds("composer:model").is_some());
    assert!(cx.debug_bounds(message_sel(0, "row")).is_some());
}

#[gpui::test]
fn cmd_click_ref_opens_split(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    // Message 5 is andy/listen's DELEGATED; its first chip is andy/fx1.
    let chip = message_sel(4, "ref:0");
    redraw(cx);
    let center = cx.debug_bounds(chip).unwrap().center();
    let secondary = cx.update(|_, cx| gpui::Modifiers::secondary_key(cx));
    cx.simulate_click(center, secondary);
    redraw(cx);
    let group = workspace.read_with(cx, |w, _| w.pane_group.clone());
    let (panes, direction) = group.read_with(cx, |g, _| (g.panes(), g.direction()));
    assert_eq!(panes.len(), 2, "the center split in two");
    assert_eq!(direction, Some(SplitDirection::Right), "side by side");
    let fx1 = chat_snapshot()
        .roster
        .iter()
        .flat_map(|g| &g.agents)
        .find(|a| a.handle == "andy/fx1")
        .map(|a| a.id.clone())
        .unwrap();
    panes[1].read_with(cx, |pane, _| {
        assert_eq!(pane.active_item(), Some(&TabItem::Agent(fx1)));
    });
    panes[0].read_with(cx, |pane, _| assert_eq!(pane.tabs(), &[TabItem::Chat]));
    assert!(cx.debug_bounds("pane-1").is_some());
}

#[gpui::test]
fn click_on_a_ref_opens_a_tab_in_the_pane(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    // Message 2 is andy/a1's ALERT; its first chip is the commit 4d9fdc8.
    click(cx, message_sel(1, "ref:0"));
    let group = workspace.read_with(cx, |w, _| w.pane_group.clone());
    let panes = group.read_with(cx, |g, _| g.panes());
    assert_eq!(panes.len(), 1);
    panes[0].read_with(cx, |pane, _| {
        assert_eq!(
            pane.tabs(),
            &[
                TabItem::Chat,
                TabItem::Ref(crate::refs::RefTarget::Commit("4d9fdc8".into()))
            ]
        );
    });
}

#[gpui::test]
fn show_ledger_button_reveals_presence_lines(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    let chat = chat_of(&workspace, cx);
    assert!(!chat.read_with(cx, |chat, _| chat.show_ledger()));
    click(cx, "pane-0:show-ledger");
    assert!(chat.read_with(cx, |chat, _| chat.show_ledger()));
}

#[gpui::test]
fn chat_tab_badge_matches_needs_you(cx: &mut TestAppContext) {
    use crate::backend::{Backend as _, FixtureBackend, Screen};
    init(cx);
    for screen in [Screen::Chat, Screen::NeedsYou, Screen::NeedsYouAnswered] {
        let mut app = TestAppContext::single();
        init(&mut app);
        let snapshot = FixtureBackend::for_screen(screen).unwrap().snapshot();
        let needs_you = snapshot.status.needs_you as usize;
        let (workspace, app) =
            app.add_window_view(|window, cx| Workspace::new(snapshot, window, cx));
        redraw(app);
        let chat = chat_of(&workspace, app);
        assert_eq!(
            chat.read_with(app, |c, _| c.badge()),
            needs_you,
            "{screen:?}"
        );
        assert_eq!(
            app.debug_bounds("pane-0:tab-0:badge").is_some(),
            needs_you > 0,
            "{screen:?}"
        );
    }
}
