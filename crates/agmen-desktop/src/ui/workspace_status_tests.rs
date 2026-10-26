use gpui::{Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{Backend as _, FixtureBackend, Screen};
use crate::ui::test_support::{init, redraw};

/// The workspace `--screen` opens for `screen`, drawn once.
fn open(cx: &mut TestAppContext, screen: Screen) -> (Entity<Workspace>, &mut VisualTestContext) {
    init(cx);
    let snapshot = FixtureBackend::for_screen(screen).unwrap().snapshot();
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(snapshot, window, cx);
        workspace.show_screen(screen, cx);
        workspace
    });
    redraw(cx);
    (workspace, cx)
}

/// Asserts the status bar's context text (`None` is the listener's state)
/// and whether it offers the Listener toggle, as `screen`'s frame draws them.
fn assert_status(cx: &mut TestAppContext, screen: Screen, context: Option<&str>, toggle: bool) {
    let (workspace, cx) = open(cx, screen);
    let shown = workspace.read_with(cx, |w, cx| w.status_context(cx));
    assert_eq!(shown.as_deref(), context, "{screen:?}");
    assert_eq!(
        cx.debug_bounds("listener-toggle").is_some(),
        toggle,
        "{screen:?} toggle"
    );
    assert!(cx.debug_bounds("status-context").is_some());
}

#[gpui::test]
fn chat_shows_the_listener_and_no_toggle(cx: &mut TestAppContext) {
    // Frame 2: "listener codex · idle", the right dock open.
    assert_status(cx, Screen::Chat, None, false);
}

#[gpui::test]
fn listener_collapsed_offers_the_toggle(cx: &mut TestAppContext) {
    // Frame 4079: the chat with the right dock closed.
    assert_status(cx, Screen::ListenerCollapsed, None, true);
}

#[gpui::test]
fn agent_tab_names_the_agent_and_its_state(cx: &mut TestAppContext) {
    // Frame 479: "andy/fx1 · working".
    assert_status(cx, Screen::Agent, Some("andy/fx1 · working"), false);
}

#[gpui::test]
fn role_file_tab_names_the_file_and_its_language(cx: &mut TestAppContext) {
    // Frame 11514: "reviewer.xml · XML".
    assert_status(cx, Screen::RoleFile, Some("reviewer.xml · XML"), false);
}

#[gpui::test]
fn file_and_diff_tabs_name_the_worktree_and_branch(cx: &mut TestAppContext) {
    // Frames 4507 and 5057: "andy/fx1 · fix/c015-main-conflict", and the
    // Listener toggle (node 4760), since the right dock is closed.
    for screen in [Screen::Project, Screen::Git] {
        assert_status(cx, screen, Some("andy/fx1 · fix/c015-main-conflict"), true);
    }
}

#[gpui::test]
fn settings_shows_settings_and_no_toggle(cx: &mut TestAppContext) {
    // Frame 7908: "settings", the right dock closed and no toggle (node
    // 8215 has 9 children, one fewer than 4733's).
    assert_status(cx, Screen::Settings, Some("settings"), false);
}

#[test]
fn toggle_needs_a_closed_dock_and_a_tab_other_than_settings() {
    let settings = TabItem::Settings(crate::ui::pane::SettingsPage::Appearance);
    assert!(offers_listener_toggle(false, Some(&TabItem::Chat)));
    assert!(offers_listener_toggle(false, None));
    assert!(!offers_listener_toggle(true, Some(&TabItem::Chat)));
    assert!(!offers_listener_toggle(false, Some(&settings)));
}
