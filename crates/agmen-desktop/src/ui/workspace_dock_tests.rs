use gpui::{SharedString, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::chat_snapshot;
use crate::keymap::bind_all;
use crate::ui::dock::DockSide;
use crate::ui::test_support::{click, init, redraw};

fn open(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    init(cx);
    cx.update(bind_all);
    let (workspace, cx) =
        cx.add_window_view(|window, cx| Workspace::new(chat_snapshot(), window, cx));
    workspace.update_in(cx, |w, window, cx| w.focus_handle.focus(window, cx));
    redraw(cx);
    (workspace, cx)
}

fn shown(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Option<LeftDockTab> {
    workspace.read_with(cx, |w, _| w.shown_left_tab())
}

fn active_item(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Option<TabItem> {
    workspace.read_with(cx, |w, cx| w.pane_group.read(cx).active_item(cx))
}

fn pane_count(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> usize {
    workspace.read_with(cx, |w, cx| w.pane_group.read(cx).panes().len())
}

fn active_pane(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> usize {
    workspace.read_with(cx, |w, cx| w.pane_group.read(cx).active_pane())
}

#[gpui::test]
fn clicking_each_tab_switches_content_and_selection(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    assert_eq!(shown(&workspace, cx), Some(LeftDockTab::Agents));
    assert!(cx.debug_bounds("agent:andy/fx1").is_some());
    let cases = [
        ("tab-project", LeftDockTab::Project, "project-panel"),
        ("tab-git", LeftDockTab::Git, "git-scope"),
        ("tab-roles", LeftDockTab::Roles, "role:engineer"),
        ("tab-agents", LeftDockTab::Agents, "agent:andy/fx1"),
    ];
    let contents = cases.map(|(_, _, content)| content);
    for (tab, expected, content) in cases {
        click(cx, tab);
        assert_eq!(shown(&workspace, cx), Some(expected), "{tab}");
        for other in contents {
            assert_eq!(
                cx.debug_bounds(other).is_some(),
                other == content,
                "{tab} shows {content}, not {other}"
            );
        }
    }
}

#[gpui::test]
fn opening_a_role_shows_its_file_tab_and_role_panel(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    click(cx, "tab-roles");
    click(cx, "role:engineer");
    assert_eq!(
        active_item(&workspace, cx),
        Some(TabItem::RoleFile("engineer".into()))
    );
    assert!(cx.debug_bounds("role-file").is_some());
    assert!(cx.debug_bounds("role-panel").is_some());
    assert!(cx.debug_bounds("listener-panel").is_none());

    click(cx, "role:base");
    assert_eq!(
        active_item(&workspace, cx),
        Some(TabItem::RoleFile("base".into()))
    );
    assert!(cx.debug_bounds("role-file").is_some());
    assert!(cx.debug_bounds("role-panel").is_none(), "base has none");
    assert!(cx.debug_bounds("listener-panel").is_none());
    assert!(cx.debug_bounds("agent-panel").is_none());
}

#[gpui::test]
fn opening_a_project_file_opens_its_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    click(cx, "tab-project");
    click(cx, "tree:Cargo.toml");
    assert!(matches!(
        active_item(&workspace, cx),
        Some(TabItem::File { path, .. }) if path == "Cargo.toml"
    ));
    assert!(cx.debug_bounds("file-view").is_some());
}

#[gpui::test]
fn selecting_a_change_opens_its_diff_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    click(cx, "tab-git");
    click(cx, "git-change:src/main.rs");
    assert!(matches!(
        active_item(&workspace, cx),
        Some(TabItem::Diff { path, .. }) if path == "src/main.rs"
    ));
    assert!(cx.debug_bounds("diff-hunk:0").is_some());
}

#[gpui::test]
fn a_diff_tab_names_its_changes_and_opens_its_file(cx: &mut TestAppContext) {
    // V95, frame 5057 node 5206: andy/fx1 › changes › src/main.rs, then
    // Open file, which opens the file's own tab.
    let (workspace, cx) = open(cx);
    click(cx, "tab-git");
    click(cx, "git-change:src/main.rs");
    redraw(cx);
    crate::ui::test_support::assert_breadcrumb_glyphs(cx, 3);
    click(cx, "diff-open-file");
    assert!(matches!(
        active_item(&workspace, cx),
        Some(TabItem::File { worktree, path }) if worktree == "andy/fx1" && path == "src/main.rs"
    ));
    assert!(cx.debug_bounds("file-view").is_some());
}

#[gpui::test]
fn files_and_git_buttons_open_and_toggle(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    click(cx, "status-project");
    assert_eq!(shown(&workspace, cx), Some(LeftDockTab::Project));
    click(cx, "status-git");
    assert_eq!(shown(&workspace, cx), Some(LeftDockTab::Git), "switches");
    click(cx, "status-git");
    assert_eq!(shown(&workspace, cx), None, "hides the dock");
    click(cx, "status-project");
    assert_eq!(shown(&workspace, cx), Some(LeftDockTab::Project), "reopens");
    click(cx, "status-project");
    assert_eq!(shown(&workspace, cx), None);
}

#[gpui::test]
fn panel_buttons_show_their_active_state(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    let buttons = |cx: &VisualTestContext| workspace.read_with(cx, |w, cx| w.panel_buttons(cx));
    assert_eq!(
        buttons(cx),
        PanelButtons::new(Some(LeftDockTab::Agents), false)
    );
    click(cx, "status-git");
    assert_eq!(
        buttons(cx),
        PanelButtons::new(Some(LeftDockTab::Git), false)
    );
    click(cx, "status-search");
    assert_eq!(active_item(&workspace, cx), Some(TabItem::Search));
    assert_eq!(buttons(cx), PanelButtons::new(Some(LeftDockTab::Git), true));
    click(cx, "status-git");
    assert_eq!(buttons(cx), PanelButtons::new(None, true));
}

#[gpui::test]
fn panel_keys_toggle_their_panels(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-e");
    assert_eq!(shown(&workspace, cx), Some(LeftDockTab::Project));
    cx.simulate_keystrokes("ctrl-shift-g");
    assert_eq!(shown(&workspace, cx), Some(LeftDockTab::Git));
    cx.simulate_keystrokes("ctrl-shift-g");
    assert_eq!(shown(&workspace, cx), None);
    cx.simulate_keystrokes("cmd-shift-e");
    assert_eq!(shown(&workspace, cx), Some(LeftDockTab::Project));
}

#[gpui::test]
fn dock_keys_toggle_the_docks(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    let open = |cx: &VisualTestContext, side| workspace.read_with(cx, |w, _| w.is_dock_open(side));
    cx.simulate_keystrokes("cmd-b");
    assert!(!open(cx, DockSide::Left));
    cx.simulate_keystrokes("cmd-r");
    assert!(!open(cx, DockSide::Right));
    cx.simulate_keystrokes("cmd-alt-b");
    assert!(open(cx, DockSide::Right));
}

#[gpui::test]
fn cmd_number_activates_pane_n(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    cx.simulate_keystrokes("cmd-1");
    assert_eq!(active_pane(&workspace, cx), 0);
    cx.simulate_keystrokes("cmd-2");
    assert_eq!(active_pane(&workspace, cx), 0, "no second pane");

    // The chat has one tab, so splitting it would only focus it; split
    // another tab.
    click(cx, "status-search");
    cx.simulate_keystrokes("cmd-\\");
    assert_eq!(pane_count(&workspace, cx), 2);
    assert_eq!(active_pane(&workspace, cx), 1);
    cx.simulate_keystrokes("cmd-1");
    assert_eq!(active_pane(&workspace, cx), 0);
    cx.simulate_keystrokes("cmd-2");
    assert_eq!(active_pane(&workspace, cx), 1);
    cx.simulate_keystrokes("cmd-3");
    assert_eq!(active_pane(&workspace, cx), 1, "past the last pane");
}

#[gpui::test]
fn cmd_w_closes_the_active_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    click(cx, "status-search");
    assert_eq!(active_item(&workspace, cx), Some(TabItem::Search));
    cx.simulate_keystrokes("cmd-w");
    let tabs = workspace.read_with(cx, |w, cx| {
        w.pane_group.read(cx).panes()[0].read(cx).tabs().to_vec()
    });
    assert!(!tabs.contains(&TabItem::Search), "the Search tab closed");
    assert_ne!(active_item(&workspace, cx), Some(TabItem::Search));

    // The chat's tab does not close.
    workspace.update(cx, |w, cx| {
        w.pane_group
            .update(cx, |group, cx| group.open(TabItem::Chat, cx));
    });
    cx.simulate_keystrokes("cmd-w");
    assert_eq!(active_item(&workspace, cx), Some(TabItem::Chat));
}

#[gpui::test]
fn cmd_backslash_splits_right(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    click(cx, "status-search");
    cx.simulate_keystrokes("cmd-\\");
    let group = workspace.read_with(cx, |w, _| w.pane_group.clone());
    let (panes, direction) = group.read_with(cx, |g, _| (g.panes(), g.direction()));
    assert_eq!(panes.len(), 2);
    assert_eq!(direction, Some(crate::ui::pane::SplitDirection::Right));
    panes[1].read_with(cx, |pane, _| {
        assert_eq!(pane.active_item(), Some(&TabItem::Search), "the active tab");
    });
}

/// Points Settings at a throwaway home whose `settings.toml` holds one
/// comment line, never the real one.
fn settings_home(cx: &mut VisualTestContext) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".config");
    let file = config.join("agmen/settings.toml");
    std::fs::create_dir_all(config.join("agmen")).unwrap();
    std::fs::write(&file, "# agmen settings\n").unwrap();
    let sources = crate::ui::SettingsSources {
        config,
        extensions: home.path().join("extensions"),
        market: "http://127.0.0.1:9".into(),
    };
    cx.update(|_, cx| cx.set_global(sources));
    (home, file)
}

const SETTINGS: TabItem = TabItem::Settings(crate::ui::pane::SettingsPage::Appearance);

fn status_context(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Option<SharedString> {
    workspace.read_with(cx, |w, cx| w.status_context(cx))
}

#[gpui::test]
fn settings_button_opens_the_settings_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    let _home = settings_home(cx);
    assert_eq!(
        status_context(&workspace, cx),
        None,
        "the chat shows the listener"
    );
    click(cx, "status-settings");
    assert_eq!(active_item(&workspace, cx), Some(SETTINGS));
    assert!(cx.debug_bounds("settings-page:appearance").is_some());
    // Frame "Desktop · settings", node 8240.
    assert_eq!(status_context(&workspace, cx), Some("settings".into()));
    assert!(cx.debug_bounds("status-context").is_some());
}

#[gpui::test]
fn cmd_comma_opens_the_settings_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    let _home = settings_home(cx);
    cx.simulate_keystrokes("cmd-,");
    assert_eq!(active_item(&workspace, cx), Some(SETTINGS));
}

#[gpui::test]
fn open_settings_toml_shows_the_file_in_a_code_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    let (_home, file) = settings_home(cx);
    click(cx, "status-settings");
    click(cx, "open-settings-toml");
    assert_eq!(
        active_item(&workspace, cx),
        Some(TabItem::SettingsFile(file))
    );
    assert!(cx.debug_bounds("line:1:text").is_some());
    assert!(cx.debug_bounds("line:2:text").is_none());
    assert_eq!(status_context(&workspace, cx), None);
}

#[gpui::test]
fn settings_screen_opens_with_the_settings_tab_active(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    let _home = settings_home(cx);
    workspace.update(cx, |w, cx| {
        w.show_screen(crate::backend::Screen::Settings, cx);
    });
    assert_eq!(active_item(&workspace, cx), Some(SETTINGS));
}
