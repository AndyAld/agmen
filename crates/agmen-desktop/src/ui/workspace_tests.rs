use super::*;
use crate::backend::{chat_snapshot, fixture_id};
use crate::ui::left_dock::LeftDockTab;
use crate::ui::pane::SplitDirection;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{STATUS_BAR_HEIGHT, TAB_BAR_HEIGHT, TITLE_BAR_HEIGHT};
use agmen_proto::GitSha;
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
    // Task 13: the pill follows the chat's alerts, not a stored number.
    use crate::backend::{Backend as _, FixtureBackend, Screen};
    init(cx);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let snapshot = FixtureBackend::for_screen(Screen::NeedsYou)
            .unwrap()
            .snapshot();
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
    let fx1 = fx1_id();
    panes[1].read_with(cx, |pane, _| {
        assert_eq!(pane.active_item(), Some(&TabItem::Agent(fx1.clone())));
    });
    panes[0].read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), starting_center(fx1));
        assert_eq!(pane.active_item(), Some(&TabItem::Chat));
    });
    assert!(cx.debug_bounds("pane-1").is_some());
}

#[gpui::test]
fn click_on_a_ref_opens_a_tab_in_the_pane(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    // Message 2 is andy/a1's ALERT; its second chip is src/main.rs:2-8.
    // andy/a1 has no worktree here, so it opens the reader's own copy.
    click(cx, message_sel(1, "ref:1"));
    let group = workspace.read_with(cx, |w, _| w.pane_group.clone());
    let panes = group.read_with(cx, |g, _| g.panes());
    assert_eq!(panes.len(), 1);
    let file = TabItem::File {
        worktree: "andy/fx1".into(),
        path: "src/main.rs".into(),
    };
    panes[0].read_with(cx, |pane, _| {
        let mut tabs = starting_center(fx1_id());
        tabs.insert(1, file.clone());
        assert_eq!(pane.tabs(), tabs);
        assert_eq!(pane.active_item(), Some(&file));
    });
}

#[gpui::test]
fn click_on_a_commit_ref_selects_it_in_the_git_panel(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    // Message 2's first chip is the commit 4d9fdc8, which fx1 holds.
    // Ruling (a): it shows in fx1's Commits, not as fx1's whole change.
    click(cx, message_sel(1, "ref:0"));
    workspace.read_with(cx, |w, cx| {
        assert_eq!(w.shown_left_tab(), Some(LeftDockTab::Git));
        let panel = w.left.git.read(cx);
        assert_eq!(panel.scoped_owner(), Some(&fixture_id("andy/fx1")));
        assert_eq!(panel.selected_commit().map(GitSha::as_str), Some("4d9fdc8"));
        let group = w.pane_group.read(cx);
        assert_eq!(group.panes()[0].read(cx).tabs(), starting_center(fx1_id()));
    });
}

fn fx1_id() -> IdentityId {
    chat_snapshot()
        .roster
        .iter()
        .flat_map(|g| &g.agents)
        .find(|a| a.handle == "andy/fx1")
        .map(|a| a.id.clone())
        .unwrap()
}

/// The mockup's tab bar: chat, andy/fx1, reviewer.xml (console.py `tab_bar`).
fn starting_center(fx1: IdentityId) -> Vec<TabItem> {
    vec![
        TabItem::Chat,
        TabItem::Agent(fx1),
        TabItem::RoleFile("reviewer".into()),
    ]
}

#[gpui::test]
fn center_opens_the_mockups_tabs_behind_the_chat(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    let group = workspace.read_with(cx, |w, _| w.pane_group.clone());
    let pane = group.read_with(cx, |g, _| g.panes()[0].clone());
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), starting_center(fx1_id()));
        assert_eq!(pane.active_item(), Some(&TabItem::Chat));
    });
    for tab in ["pane-0:tab-0", "pane-0:tab-1", "pane-0:tab-2"] {
        assert!(cx.debug_bounds(tab).is_some(), "{tab}");
    }
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
    for (screen, needs_you) in [
        (Screen::Chat, 0),
        (Screen::NeedsYou, 1),
        (Screen::NeedsYouAnswered, 0),
    ] {
        let mut app = TestAppContext::single();
        init(&mut app);
        let snapshot = FixtureBackend::for_screen(screen).unwrap().snapshot();
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

#[gpui::test]
fn escape_closes_an_open_settings_dropdown(cx: &mut TestAppContext) {
    // Task 19 binds Escape to close the kick-off dialog; an open menu in the
    // workspace still takes Escape first.
    let (_, cx) = open(cx);
    let home = tempfile::tempdir().unwrap();
    let sources = crate::ui::SettingsSources {
        config: home.path().join(".config"),
        extensions: home.path().join("extensions"),
        market: "http://127.0.0.1:9".into(),
        github: "http://127.0.0.1:9".into(),
    };
    cx.update(|_, cx| {
        cx.set_global(sources);
        crate::keymap::bind_all(cx);
    });
    cx.dispatch_action(crate::keymap::OpenSettings);
    click(cx, "theme");
    assert!(cx.debug_bounds("theme:0").is_some());
    cx.simulate_keystrokes("escape");
    redraw(cx);
    assert!(
        cx.debug_bounds("theme:0").is_none(),
        "escape left the menu open"
    );
}

// agmen-c7 fix-hosting-review minor 1 and 4: the chat and the center's tab
// views read the workspace's one snapshot, not copies of it.
#[gpui::test]
fn the_chat_and_the_center_share_the_workspaces_snapshot(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    workspace.read_with(cx, |workspace, cx| {
        let chat = workspace.chat.read(cx).snapshot();
        assert!(std::rc::Rc::ptr_eq(&workspace.snapshot, chat), "chat");
        let center = workspace.pane_group.read(cx).sources().snapshot();
        assert!(std::rc::Rc::ptr_eq(&workspace.snapshot, center), "center");
    });
}

/// Task 7: a live workspace over agmend's roster and chat, drawn once.
fn open_live(
    cx: &mut TestAppContext,
    snapshot: Snapshot,
) -> (Entity<Workspace>, &mut gpui::VisualTestContext) {
    init(cx);
    let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::live(snapshot, window, cx));
    redraw(cx);
    (workspace, cx)
}

#[gpui::test]
fn a_live_workspace_opens_no_mockup_tabs(cx: &mut TestAppContext) {
    // The roster has an andy/fx1, as the mockup does, but a live window
    // opens only the chat.
    let snapshot = crate::backend::live_snapshot();
    let fx1 = snapshot
        .roster
        .iter()
        .flat_map(|g| &g.agents)
        .find(|a| a.handle == "andy/fx1")
        .map(|a| a.id.clone())
        .expect("the fake roster has andy/fx1");
    let (workspace, cx) = open_live(cx, snapshot);
    workspace.read_with(cx, |w, cx| {
        let group = w.pane_group.read(cx);
        assert!(!group.holds(&TabItem::Agent(fx1), cx));
        assert!(!group.holds(&TabItem::RoleFile(MOCKUP_ROLE.into()), cx));
        assert_eq!(group.active_item(cx), Some(TabItem::Chat));
    });
}

#[gpui::test]
fn live_panels_without_data_say_so(cx: &mut TestAppContext) {
    // Task 7: the panels agmend has no source for yet show plain empty
    // states, never the fixture's worktrees, roles or agents.
    let mut snapshot = crate::backend::live_snapshot();
    snapshot.listener = None;
    let fx1 = snapshot
        .roster
        .iter()
        .flat_map(|g| &g.agents)
        .find(|a| a.handle == "andy/fx1")
        .map(|a| a.id.clone())
        .expect("the fake roster has andy/fx1");
    let (workspace, cx) = open_live(cx, snapshot);
    assert!(cx.debug_bounds("listener-panel:none").is_some());
    for (tab, selector) in [
        (LeftDockTab::Project, "project-panel:none"),
        (LeftDockTab::Git, "git-panel:none"),
        (LeftDockTab::Roles, "roles-panel:none"),
    ] {
        workspace.update(cx, |w, cx| w.show_left_tab(tab, cx));
        redraw(cx);
        assert!(cx.debug_bounds(selector).is_some(), "{tab:?}");
    }
    // An agent with no details on this machine: no local agents yet.
    workspace.update(cx, |w, cx| w.select_agent(&fx1, cx));
    redraw(cx);
    assert!(cx.debug_bounds("agent-panel:none").is_some());
    assert!(cx.debug_bounds("listener-panel").is_none());
}
