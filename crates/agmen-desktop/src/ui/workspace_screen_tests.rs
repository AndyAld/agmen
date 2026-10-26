use agmen_proto::IdentityId;
use gpui::{Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{Backend as _, FixtureBackend};
use crate::ui::dock::DockSide;
use crate::ui::test_support::{init, redraw};

/// The right dock's panels, by their test selectors.
const LISTENER: &str = "listener-panel";
const AGENT: &str = "agent-panel";
const ROLE: &str = "role-panel";
const PANELS: [&str; 3] = [LISTENER, AGENT, ROLE];

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

fn fx1_id(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> IdentityId {
    workspace.read_with(cx, |w, _| {
        w.snapshot
            .roster
            .iter()
            .flat_map(|g| &g.agents)
            .find(|a| a.handle == "andy/fx1")
            .map(|a| a.id.clone())
            .unwrap()
    })
}

fn fx1(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> TabItem {
    TabItem::Agent(fx1_id(workspace, cx))
}

fn main_rs() -> (String, String) {
    ("andy/fx1".to_owned(), "src/main.rs".to_owned())
}

/// Asserts the left dock's tab, the active center tab and the right dock's
/// panel (`None` for a closed dock) that `screen` opens. The center keeps
/// the starting tabs (chat, andy/fx1 and reviewer.xml in every frame that
/// has them) and opens a screen's own tab after them. Returns the
/// workspace for further checks.
fn assert_screen<'a>(
    cx: &'a mut TestAppContext,
    screen: Screen,
    left: LeftDockTab,
    active: impl FnOnce(&Entity<Workspace>, &VisualTestContext) -> TabItem,
    right: Option<&str>,
) -> (Entity<Workspace>, &'a mut VisualTestContext) {
    let (workspace, cx) = open(cx, screen);
    let active = active(&workspace, cx);
    workspace.read_with(cx, |w, cx| {
        assert_eq!(w.shown_left_tab(), Some(left), "{screen:?} left dock");
        let pane = w.pane_group.read(cx).panes()[0].clone();
        let pane = pane.read(cx);
        assert_eq!(pane.active_item(), Some(&active), "{screen:?} center");
        let mut tabs = vec![TabItem::Chat];
        tabs.extend(super::super::starting_tabs(&w.snapshot));
        if !tabs.contains(&active) {
            tabs.push(active.clone());
        }
        assert_eq!(pane.tabs(), tabs, "{screen:?} tabs");
        assert_eq!(
            w.is_dock_open(DockSide::Right),
            right.is_some(),
            "{screen:?} right dock"
        );
    });
    for panel in PANELS {
        assert_eq!(
            cx.debug_bounds(panel).is_some(),
            right == Some(panel),
            "{screen:?} shows {right:?}, not {panel}"
        );
    }
    (workspace, cx)
}

#[gpui::test]
fn chat_screens_show_agents_the_chat_and_the_listener(_cx: &mut TestAppContext) {
    for screen in [
        Screen::Chat,
        Screen::Ask,
        Screen::ModelPicker,
        Screen::NeedsYou,
        Screen::NeedsYouAnswered,
        Screen::Palette,
        Screen::SettingsFromChat,
        Screen::SettingsApplied,
    ] {
        let mut app = TestAppContext::single();
        assert_screen(
            &mut app,
            screen,
            LeftDockTab::Agents,
            |_, _| TabItem::Chat,
            Some(LISTENER),
        );
    }
}

#[gpui::test]
fn take_over_screens_show_the_chat_and_fx1s_agent_panel(_cx: &mut TestAppContext) {
    for screen in [
        Screen::ChatTerminal,
        Screen::ChatCodex,
        Screen::SwitchAgent,
        Screen::AddAgent,
    ] {
        let mut app = TestAppContext::single();
        assert_screen(
            &mut app,
            screen,
            LeftDockTab::Agents,
            |_, _| TabItem::Chat,
            Some(AGENT),
        );
    }
}

#[gpui::test]
fn listener_collapsed_closes_the_right_dock(cx: &mut TestAppContext) {
    assert_screen(
        cx,
        Screen::ListenerCollapsed,
        LeftDockTab::Agents,
        |_, _| TabItem::Chat,
        None,
    );
}

#[gpui::test]
fn agent_screen_shows_fx1s_tab_and_panel(cx: &mut TestAppContext) {
    let (workspace, cx) = assert_screen(cx, Screen::Agent, LeftDockTab::Agents, fx1, Some(AGENT));
    // The Agents dock highlights fx1, as the frame does.
    let id = fx1_id(&workspace, cx);
    workspace.read_with(cx, |w, cx| {
        assert_eq!(w.agents_panel.read(cx).selected(), Some(&id));
    });
}

#[gpui::test]
fn role_file_screen_shows_roles_reviewer_xml_and_its_panel(cx: &mut TestAppContext) {
    let (workspace, cx) = assert_screen(
        cx,
        Screen::RoleFile,
        LeftDockTab::Roles,
        |_, _| TabItem::RoleFile("reviewer".into()),
        Some(ROLE),
    );
    workspace.read_with(cx, |w, cx| {
        assert_eq!(w.left.roles.read(cx).selected(), Some("reviewer"));
    });
}

#[gpui::test]
fn project_screens_show_main_rs_with_the_right_dock_closed(_cx: &mut TestAppContext) {
    for (screen, tests_open) in [(Screen::Project, false), (Screen::ProjectTestsOpen, true)] {
        let mut app = TestAppContext::single();
        let (workspace, app) = assert_screen(
            &mut app,
            screen,
            LeftDockTab::Project,
            |_, _| {
                let (worktree, path) = main_rs();
                TabItem::File { worktree, path }
            },
            None,
        );
        workspace.read_with(app, |w, cx| {
            assert_eq!(w.left.project.read(cx).selected(), Some("src/main.rs"));
        });
        assert!(
            app.debug_bounds("tree:src/ui/tabs.rs").is_some(),
            "{screen:?}"
        );
        assert_eq!(
            app.debug_bounds("tree:tests/it_main.rs").is_some(),
            tests_open,
            "{screen:?} tests folder"
        );
    }
}

#[gpui::test]
fn git_screens_show_main_rs_diff_with_the_right_dock_closed(_cx: &mut TestAppContext) {
    for (screen, changes_open) in [(Screen::Git, true), (Screen::GitCollapsed, false)] {
        let mut app = TestAppContext::single();
        let (workspace, app) = assert_screen(
            &mut app,
            screen,
            LeftDockTab::Git,
            |_, _| {
                let (worktree, path) = main_rs();
                TabItem::Diff { worktree, path }
            },
            None,
        );
        workspace.read_with(app, |w, cx| {
            assert_eq!(w.left.git.read(cx).selected(), Some("src/main.rs"));
        });
        assert_eq!(
            app.debug_bounds("git-change:src/main.rs").is_some(),
            changes_open,
            "{screen:?} Changes section"
        );
    }
}

#[gpui::test]
fn settings_screen_shows_settings_with_the_right_dock_closed(cx: &mut TestAppContext) {
    assert_screen(
        cx,
        Screen::Settings,
        LeftDockTab::Agents,
        |_, _| TabItem::Settings(SettingsPage::Appearance),
        None,
    );
}

#[gpui::test]
fn plan_screens_keep_the_chat_and_the_listener_for_now(_cx: &mut TestAppContext) {
    for screen in [
        Screen::Plan,
        Screen::Spec,
        Screen::PlanReady,
        Screen::KickOff,
        Screen::Running,
    ] {
        let mut app = TestAppContext::single();
        assert_screen(
            &mut app,
            screen,
            LeftDockTab::Agents,
            |_, _| TabItem::Chat,
            Some(LISTENER),
        );
    }
}

// Task 15: the model picker screen opens the composer's picker on codex.
#[gpui::test]
fn model_picker_screen_opens_the_picker(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, Screen::ModelPicker);
    assert!(cx.debug_bounds("model-picker").is_some());
    assert!(
        cx.debug_bounds("model-picker:codex:default:check")
            .is_some()
    );
}

#[gpui::test]
fn ask_screen_keeps_the_picker_closed(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, Screen::Ask);
    assert!(cx.debug_bounds("model-picker").is_none());
}

#[gpui::test]
fn toggling_the_right_dock_keeps_the_picker_open(cx: &mut TestAppContext) {
    // Fix 45 and V86: cmd-r and cmd-alt-b leave the picker as it is.
    let (workspace, cx) = open(cx, Screen::ModelPicker);
    for _ in 0..2 {
        workspace.update(cx, |w, cx| w.toggle_dock(DockSide::Right, cx));
        redraw(cx);
        assert!(cx.debug_bounds("model-picker").is_some());
    }
}
