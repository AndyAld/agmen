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
    // The tab hosts fx1's session view (frame 479), not a placeholder.
    assert!(cx.debug_bounds("agent-view").is_some());
    assert!(cx.debug_bounds("session:0:tool").is_some());
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

// Task 19: the planning room in the Agents tab, the chat as the plan tab,
// the plan panel or the spec's Review panel, and the kick-off dialog.
const PLAN_PANEL: &str = "plan-panel";
const REVIEW: &str = "review-panel";
const DESIGN: &str = "docs/specs/ridgeline-design.md";

#[gpui::test]
fn plan_screens_show_the_planning_room_and_the_plan_panel(_cx: &mut TestAppContext) {
    for screen in [
        Screen::Plan,
        Screen::PlanReady,
        Screen::KickOff,
        Screen::Running,
    ] {
        let mut app = TestAppContext::single();
        let (workspace, app) = assert_screen(
            &mut app,
            screen,
            LeftDockTab::Agents,
            |_, _| TabItem::Chat,
            Some(PLAN_PANEL),
        );
        for selector in [PLAN_PANEL, "plan-room"] {
            assert!(
                app.debug_bounds(selector).is_some(),
                "{screen:?} {selector}"
            );
        }
        assert!(app.debug_bounds(REVIEW).is_none(), "{screen:?}");
        assert_eq!(
            app.debug_bounds("kickoff-dialog").is_some(),
            screen == Screen::KickOff,
            "{screen:?} dialog"
        );
        // The plan panel is the mockup's 470 px (node 8935).
        assert_eq!(
            app.debug_bounds(PLAN_PANEL).unwrap().size.width,
            crate::ui::tokens::PLAN_PANEL_WIDTH - crate::ui::tokens::BORDER_WIDTH
        );
        let context = if screen == Screen::Running {
            "ridgeline · 7 agents"
        } else {
            "ridgeline · planning"
        };
        workspace.read_with(app, |w, cx| {
            assert_eq!(w.status_context(cx).as_deref(), Some(context), "{screen:?}");
            // The spec the mockup opens is the plan tab's neighbour.
            let pane = w.pane_group.read(cx).panes()[0].clone();
            assert!(pane.read(cx).tabs().contains(&TabItem::Spec(DESIGN.into())));
        });
    }
}

// Shot diff (kick-off, nodes 10289 and 10290): the dim covers the window and
// the card sits MODAL_TOP below its top, not in a zero-height row under the
// status bar.
#[gpui::test]
fn kick_off_dims_the_whole_window(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, Screen::KickOff);
    let viewport = cx.update(|window, _| window.viewport_size());
    let dim = cx.debug_bounds("kickoff-dialog").unwrap();
    assert_eq!(dim.origin, gpui::point(gpui::px(0.), gpui::px(0.)));
    assert_eq!(dim.size, viewport);
    let card = cx.debug_bounds("kickoff-dialog:card").unwrap();
    assert_eq!(card.origin.y, crate::ui::tokens::MODAL_TOP);
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

#[gpui::test]
fn spec_screen_shows_the_spec_tab_and_its_review(cx: &mut TestAppContext) {
    let (_, cx) = assert_screen(
        cx,
        Screen::Spec,
        LeftDockTab::Agents,
        |_, _| TabItem::Spec(DESIGN.into()),
        Some(REVIEW),
    );
    assert!(cx.debug_bounds("spec-view").is_some());
    assert!(cx.debug_bounds(PLAN_PANEL).is_none());
}

#[gpui::test]
fn a_plan_spec_click_opens_its_tab_and_escape_closes_the_dialog(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx, Screen::KickOff);
    cx.update(|_, cx| crate::keymap::bind_all(cx));
    // main.rs focuses the workspace once the window opens; the dialog takes
    // focus from it and gives it back when it closes.
    cx.update(|window, cx| {
        let focus = workspace.read(cx).focus_handle.clone();
        focus.focus(window, cx);
    });
    redraw(cx);
    cx.simulate_keystrokes("escape");
    redraw(cx);
    assert!(cx.debug_bounds("kickoff-dialog").is_none());
    cx.update(|window, cx| assert!(workspace.read(cx).focus_handle.is_focused(window)));
    crate::ui::test_support::click(cx, "plan:spec:docs/specs/ridgeline-design.md");
    workspace.read_with(cx, |w, cx| {
        let pane = w.pane_group.read(cx).panes()[0].clone();
        assert_eq!(
            pane.read(cx).active_item(),
            Some(&TabItem::Spec(DESIGN.into()))
        );
    });
    assert!(cx.debug_bounds(REVIEW).is_some());
}

// Review (screens minor 3): showing the collapsed git screen twice keeps
// the Changes section folded, whatever state it starts in.
#[gpui::test]
fn the_collapsed_git_screen_folds_changes_each_time(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx, Screen::GitCollapsed);
    workspace.update(cx, |w, cx| w.show_screen(Screen::GitCollapsed, cx));
    redraw(cx);
    assert!(cx.debug_bounds("git-change:src/main.rs").is_none());
    workspace.update(cx, |w, cx| w.show_screen(Screen::Git, cx));
    redraw(cx);
    assert!(cx.debug_bounds("git-change:src/main.rs").is_some());
}

// Review (screens minor 2): every screen that names the mockup's agent or
// its worktree has it in its fixture, so the screen never falls back to
// something else silently.
#[test]
fn screens_find_the_mockup_agent_and_worktree_in_their_fixture() {
    let mut checked = 0;
    for screen in Screen::ALL {
        let state = WorkspaceState::for_screen(screen);
        let snapshot = FixtureBackend::for_screen(screen).unwrap().snapshot();
        if state.right == Some(RightDock::Agent) || state.center == CenterTab::Agent {
            assert!(
                super::super::mockup_agent(&snapshot).is_some(),
                "{screen:?} lacks its agent"
            );
            checked += 1;
        }
        if matches!(state.center, CenterTab::File | CenterTab::Diff) {
            let handle = super::super::mockup_handle(&snapshot);
            assert!(
                snapshot.worktrees.iter().any(|wt| wt.handle == handle),
                "{screen:?} lacks {handle}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 9, "checked {checked}");
}
