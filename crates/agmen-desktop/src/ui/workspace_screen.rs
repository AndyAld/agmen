//! What each mockup screen opens beyond its fixture data: which docks are
//! open, the left dock's tab, the active center tab and the right dock's
//! panel, read from the screen's frame `Desktop · <screen>` in
//! `docs/mockups/agmen-console.figcraft`. A child of `workspace`, so it sets
//! the workspace's state directly.

use gpui::Context;

use super::{MOCKUP_ROLE, Workspace, mockup_agent, mockup_handle};
use crate::backend::Screen;
use crate::ui::git_panel::GitSection;
use crate::ui::left_dock::LeftDockTab;
use crate::ui::pane::{SettingsPage, TabItem};
use crate::ui::{add_agent_dialog::AgentDialog, chat::TerminalPopup};

/// The file the Project and Git frames open from the mockup's worktree.
const MAIN_RS: &str = "src/main.rs";

/// The left dock's tab, and how its panel unfolds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeftDock {
    Agents,
    /// main.rs selected, `src/ui` open and `tests` folded.
    Project,
    /// As `Project`, with `tests` open.
    ProjectTestsOpen,
    /// main.rs's change selected.
    Git,
    /// As `Git`, with the Changes section folded.
    GitChangesFolded,
    Roles,
}

impl LeftDock {
    const fn tab(self) -> LeftDockTab {
        match self {
            Self::Agents => LeftDockTab::Agents,
            Self::Project | Self::ProjectTestsOpen => LeftDockTab::Project,
            Self::Git | Self::GitChangesFolded => LeftDockTab::Git,
            Self::Roles => LeftDockTab::Roles,
        }
    }
}

/// The active center tab. The chat, andy/fx1 and reviewer.xml tabs are
/// open on every screen; a file, diff or Settings tab opens after them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CenterTab {
    Chat,
    /// andy/fx1's session.
    Agent,
    /// reviewer.xml.
    RoleFile,
    /// fx1's main.rs.
    File,
    /// fx1's main.rs diff.
    Diff,
    Settings,
    /// Task 19: the plan's design spec (frame "Desktop · spec").
    Spec,
}

/// The right dock's panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RightDock {
    Listener,
    /// andy/fx1's Agent panel.
    Agent,
    /// reviewer's Role panel.
    Role,
    /// Task 19: the plan panel, or the Review panel beside a spec tab.
    Plan,
}

/// What a screen opens over the take-over terminal (Task 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Popup {
    /// The switch agent menu (frame "Desktop · switch agent").
    SwitchMenu,
    /// The add agent dialog for grok over the menu ("Desktop · add agent").
    AddMissingAgent,
}

/// The agent the add agent frame adds: the one the switch menu lists as
/// missing.
const MOCKUP_MISSING_AGENT: &str = "grok";

/// What a screen shows beyond its fixture data. Every screen opens the
/// left dock; a `right` of `None` is a closed right dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkspaceState {
    left: LeftDock,
    center: CenterTab,
    right: Option<RightDock>,
    /// Task 18: the query the command palette opens on, when open.
    palette: Option<&'static str>,
    /// Task 13: the alert moment the frame shows.
    alert: AlertMoment,
    /// Task 14: the take-over terminal's menu or dialog the frame opens.
    popup: Option<Popup>,
    /// Task 15: whether the frame shows the composer's model picker open.
    model_picker: bool,
    /// Task 19: whether the frame shows the kick-off dialog.
    kickoff: bool,
}

/// Task 13: what a screen shows of an alert beyond its history, silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlertMoment {
    None,
    /// The oldest alert that needs me as just arrived: its toast and
    /// "chime played" (the needs-you frame).
    Arrives,
    /// The last answered alert as just answered: the Answered banner (the
    /// needs-you-answered frame).
    Answered,
}

impl WorkspaceState {
    /// Each screen's state, as its frame shows it. Screens whose views are
    /// not built yet keep the chat with their frame's docks.
    pub(super) const fn for_screen(screen: Screen) -> Self {
        let (left, center, right) = match screen {
            Screen::Chat
            | Screen::Ask
            | Screen::NeedsYou
            | Screen::NeedsYouAnswered
            | Screen::SettingsFromChat
            | Screen::SettingsApplied => {
                (LeftDock::Agents, CenterTab::Chat, Some(RightDock::Listener))
            }
            // Task 15: the composer's model picker opens over the chat.
            Screen::ModelPicker => (LeftDock::Agents, CenterTab::Chat, Some(RightDock::Listener)),
            // Task 18: the palette opens over the chat with "spawn" typed.
            Screen::Palette => {
                return Self {
                    left: LeftDock::Agents,
                    center: CenterTab::Chat,
                    right: Some(RightDock::Listener),
                    palette: Some("spawn"),
                    alert: AlertMoment::None,
                    popup: None,
                    model_picker: false,
                    kickoff: false,
                };
            }
            // Task 14: the fixture takes fx1 over; the popups open below.
            Screen::ChatTerminal | Screen::ChatCodex | Screen::SwitchAgent | Screen::AddAgent => {
                (LeftDock::Agents, CenterTab::Chat, Some(RightDock::Agent))
            }
            Screen::ListenerCollapsed => (LeftDock::Agents, CenterTab::Chat, None),
            Screen::Agent => (LeftDock::Agents, CenterTab::Agent, Some(RightDock::Agent)),
            Screen::RoleFile => (LeftDock::Roles, CenterTab::RoleFile, Some(RightDock::Role)),
            Screen::Project => (LeftDock::Project, CenterTab::File, None),
            Screen::ProjectTestsOpen => (LeftDock::ProjectTestsOpen, CenterTab::File, None),
            Screen::Git => (LeftDock::Git, CenterTab::Diff, None),
            Screen::GitCollapsed => (LeftDock::GitChangesFolded, CenterTab::Diff, None),
            Screen::Settings => (LeftDock::Agents, CenterTab::Settings, None),
            // Task 19: the chat is the plan tab; KickOff adds the dialog.
            Screen::Plan | Screen::PlanReady | Screen::KickOff | Screen::Running => {
                (LeftDock::Agents, CenterTab::Chat, Some(RightDock::Plan))
            }
            Screen::Spec => (LeftDock::Agents, CenterTab::Spec, Some(RightDock::Plan)),
        };
        let popup = match screen {
            Screen::SwitchAgent => Some(Popup::SwitchMenu),
            Screen::AddAgent => Some(Popup::AddMissingAgent),
            _ => None,
        };
        Self {
            left,
            center,
            right,
            palette: None,
            alert: match screen {
                Screen::NeedsYou => AlertMoment::Arrives,
                Screen::NeedsYouAnswered => AlertMoment::Answered,
                _ => AlertMoment::None,
            },
            popup,
            model_picker: matches!(screen, Screen::ModelPicker),
            kickoff: matches!(screen, Screen::KickOff),
        }
    }
}

impl Workspace {
    /// Opens the docks, tabs and panels `screen`'s frame shows.
    pub(crate) fn show_screen(&mut self, screen: Screen, cx: &mut Context<Self>) {
        let state = WorkspaceState::for_screen(screen);
        self.show_left(state.left, cx);
        self.show_right(state.right, cx);
        // After the right dock: opening a role opens its file tab too.
        if let Some(item) = self.center_item(state.center) {
            self.pane_group
                .update(cx, |group, cx| group.open_last(item, cx));
        }
        if let Some(query) = state.palette {
            self.open_palette_after_draw(query);
        }
        match state.alert {
            AlertMoment::None => {}
            AlertMoment::Arrives => self.chat.update(cx, |chat, cx| chat.show_arrival(cx)),
            AlertMoment::Answered => self.chat.update(cx, |chat, cx| chat.show_answered(cx)),
        }
        match state.popup {
            Some(Popup::SwitchMenu) => self.show_terminal_popup(TerminalPopup::SwitchMenu, cx),
            Some(Popup::AddMissingAgent) => {
                let dialog = AgentDialog::Add(MOCKUP_MISSING_AGENT.into());
                self.show_terminal_popup(TerminalPopup::Dialog(dialog), cx);
            }
            None => {}
        }
        // Task 15: the frame shows the composer's model picker open.
        if state.model_picker {
            self.chat.update(cx, |chat, cx| chat.open_model_picker(cx));
        }
        // Task 19: the kick-off frame shows the confirm dialog.
        if state.kickoff {
            self.open_kickoff(cx);
        }
        cx.notify();
    }

    fn show_left(&mut self, left: LeftDock, cx: &mut Context<Self>) {
        self.left.tab = left.tab();
        self.left_dock.open();
        match left {
            LeftDock::Agents | LeftDock::Roles => {}
            LeftDock::Project | LeftDock::ProjectTestsOpen => {
                self.left.project.update(cx, |panel, cx| {
                    panel.reveal(MAIN_RS);
                    panel.open_folder("src/ui");
                    if left == LeftDock::ProjectTestsOpen {
                        panel.open_folder("tests");
                    }
                    cx.notify();
                });
            }
            LeftDock::Git | LeftDock::GitChangesFolded => {
                self.left.git.update(cx, |panel, cx| {
                    panel.set_selected(MAIN_RS);
                    let folded = left == LeftDock::GitChangesFolded;
                    panel.set_folded(GitSection::Changes, folded, cx);
                    cx.notify();
                });
            }
        }
    }

    fn show_right(&mut self, right: Option<RightDock>, cx: &mut Context<Self>) {
        match right {
            None => {
                self.right_dock.close();
                self.set_selection(None, cx);
            }
            // With nothing selected, a project with a plan shows its plan.
            Some(RightDock::Listener | RightDock::Plan) => {
                self.set_selection(None, cx);
                self.right_dock.open();
            }
            Some(RightDock::Agent) => {
                if let Some(id) = mockup_agent(&self.snapshot) {
                    self.select_agent(&id, cx);
                }
            }
            Some(RightDock::Role) => {
                self.left
                    .roles
                    .update(cx, |panel, _| panel.set_selected(MOCKUP_ROLE));
                self.open_role(MOCKUP_ROLE, cx);
            }
        }
    }

    /// The tab `center` names, or `None` when the fixture lacks its agent.
    fn center_item(&self, center: CenterTab) -> Option<TabItem> {
        let worktree = mockup_handle(&self.snapshot);
        Some(match center {
            CenterTab::Chat => TabItem::Chat,
            CenterTab::Agent => TabItem::Agent(mockup_agent(&self.snapshot)?),
            CenterTab::RoleFile => TabItem::RoleFile(MOCKUP_ROLE.into()),
            CenterTab::File => TabItem::File {
                worktree,
                path: MAIN_RS.into(),
            },
            CenterTab::Diff => TabItem::Diff {
                worktree,
                path: MAIN_RS.into(),
            },
            CenterTab::Settings => TabItem::Settings(SettingsPage::default()),
            CenterTab::Spec => TabItem::Spec(super::plan_wiring::mockup_spec(&self.snapshot)?),
        })
    }
}

#[cfg(test)]
#[path = "workspace_screen_tests.rs"]
mod tests;
