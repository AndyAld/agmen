//! The workspace's left dock: its tabs and their panels, the status bar
//! buttons that open them, the role panel a Roles click shows, and the pane
//! keys. A child of `workspace`, so the handlers reach its state.

use gpui::{
    AnyElement, AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled as _, Subscription, div,
};

use super::Workspace;
use crate::backend::{RoleFile, Snapshot};
use crate::ui::agent_panel::AgentPanel;
use crate::ui::components::{Color, Label};
use crate::ui::keys::{
    ActivatePane, CloseActiveTab, OpenSearch, ShowLeftDockTab, SplitRight, ToggleGitPanel,
    ToggleProjectPanel,
};
use crate::ui::left_dock::{LeftDockTab, PanelButtons};
use crate::ui::listener_panel::ListenerPanel;
use crate::ui::pane::TabItem;
use crate::ui::project_panel::{ProjectPanel, ProjectPanelEvent};
use crate::ui::roles_panel::{RolePanel, RolesPanel, RolesPanelEvent};
use crate::ui::scope_chip::ScopeOption;

/// The left dock's tab and the panels it hosts beside the Agents panel.
pub(crate) struct LeftDockState {
    pub(super) tab: LeftDockTab,
    project: Entity<ProjectPanel>,
    roles: Entity<RolesPanel>,
    /// The role whose panel the right dock shows, set by a Roles click and
    /// cleared when the selection changes.
    pub(super) shown_role: Option<RoleFile>,
    _events: [Subscription; 2],
}

impl LeftDockState {
    pub(crate) fn new(snapshot: &Snapshot, cx: &mut Context<Workspace>) -> Self {
        let options = ScopeOption::for_worktrees(&snapshot.worktrees, &snapshot.agents);
        let worktrees = snapshot.worktrees.clone();
        let project = cx.new(|cx| ProjectPanel::new(worktrees, options, cx));
        let roles = cx.new(|_| RolesPanel::new(snapshot.roles.clone()));
        let project_events = cx.subscribe(&project, |this, _, event, cx| {
            let ProjectPanelEvent::OpenFile { worktree, path } = event;
            this.open_file(worktree, path, cx);
        });
        let roles_events = cx.subscribe(&roles, |this, _, event, cx| {
            let RolesPanelEvent::Open(name) = event;
            this.open_role(name, cx);
        });
        Self {
            tab: LeftDockTab::Agents,
            project,
            roles,
            shown_role: None,
            _events: [project_events, roles_events],
        }
    }
}

impl Workspace {
    /// The tab the left dock shows, or `None` while it is hidden.
    pub(crate) const fn shown_left_tab(&self) -> Option<LeftDockTab> {
        if self.left_dock.is_open() {
            Some(self.left.tab)
        } else {
            None
        }
    }

    /// Shows `tab`, opening the dock.
    pub(crate) fn show_left_tab(&mut self, tab: LeftDockTab, cx: &mut Context<Self>) {
        self.left.tab = tab;
        self.left_dock.open();
        cx.notify();
    }

    /// Shows `tab`, or hides the dock when it already shows `tab` (Zed's
    /// `ToggleFocus`).
    pub(crate) fn toggle_left_tab(&mut self, tab: LeftDockTab, cx: &mut Context<Self>) {
        if self.shown_left_tab() == Some(tab) {
            self.left_dock.toggle();
            cx.notify();
        } else {
            self.show_left_tab(tab, cx);
        }
    }

    /// Opens a Project panel file in a center tab.
    fn open_file(&mut self, owner: &agmen_proto::IdentityId, path: &str, cx: &mut Context<Self>) {
        let Some(wt) = self.snapshot.worktrees.iter().find(|wt| wt.owner == *owner) else {
            return;
        };
        let item = TabItem::File {
            worktree: wt.handle.clone(),
            path: path.to_owned(),
        };
        self.pane_group.update(cx, |group, cx| group.open(item, cx));
    }

    /// Opens a role's file in a center tab and its Role panel in the right
    /// dock; base has no Role panel, so the dock shows none.
    fn open_role(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(role) = self.snapshot.roles.iter().find(|r| r.name == name).cloned() else {
            return;
        };
        self.set_selection(None, cx);
        self.left.shown_role = Some(role);
        self.right_dock.open();
        let item = TabItem::RoleFile(name.to_owned());
        self.pane_group.update(cx, |group, cx| group.open(item, cx));
        cx.notify();
    }

    /// The left dock's panel for its tab.
    pub(super) fn left_dock_content(&self) -> AnyElement {
        match self.left.tab {
            LeftDockTab::Agents => self.agents_panel.clone().into_any_element(),
            LeftDockTab::Project => self.left.project.clone().into_any_element(),
            // shortcut: a placeholder until Task 12's GitPanel replaces this
            // arm.
            LeftDockTab::Git => div()
                .debug_selector(|| "git-placeholder".into())
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(Label::new("Git").color(Color::Placeholder))
                .into_any_element(),
            LeftDockTab::Roles => self.left.roles.clone().into_any_element(),
        }
    }

    /// The right dock's panel: a role's, an agent's or the Listener panel;
    /// `None` for base, which has no Role panel.
    pub(super) fn right_panel(&self) -> Option<AnyElement> {
        if let Some(role) = &self.left.shown_role {
            return role
                .info
                .clone()
                .map(|info| RolePanel::new(role.name.clone(), info).into_any_element());
        }
        Some(match self.shown_agent() {
            Some(info) => AgentPanel::new(info.clone()).into_any_element(),
            None => ListenerPanel::new(self.snapshot.listener.clone()).into_any_element(),
        })
    }

    /// The status bar's files, git and search buttons.
    pub(super) fn panel_buttons(&self, cx: &gpui::App) -> PanelButtons {
        let search = self.pane_group.read(cx).active_item(cx) == Some(TabItem::Search);
        PanelButtons::new(self.shown_left_tab(), search)
    }

    /// Registers the dock, panel and pane actions on the workspace's root.
    pub(super) fn dock_actions(root: Div, cx: &Context<Self>) -> Div {
        root.on_action(cx.listener(|this, action: &ShowLeftDockTab, _, cx| {
            this.show_left_tab(action.0, cx);
        }))
        .on_action(cx.listener(|this, _: &ToggleProjectPanel, _, cx| {
            this.toggle_left_tab(LeftDockTab::Project, cx);
        }))
        .on_action(cx.listener(|this, _: &ToggleGitPanel, _, cx| {
            this.toggle_left_tab(LeftDockTab::Git, cx);
        }))
        .on_action(cx.listener(|this, _: &OpenSearch, _, cx| {
            this.pane_group
                .update(cx, |group, cx| group.open(TabItem::Search, cx));
        }))
        .on_action(cx.listener(|this, action: &ActivatePane, _, cx| {
            this.pane_group
                .update(cx, |group, cx| group.activate_pane(action.0, cx));
        }))
        .on_action(cx.listener(|this, _: &CloseActiveTab, _, cx| {
            this.pane_group
                .update(cx, |group, cx| group.close_active_tab(cx));
        }))
        .on_action(cx.listener(|this, _: &SplitRight, _, cx| {
            this.pane_group
                .update(cx, |group, cx| group.split_active_right(cx));
        }))
    }
}

#[cfg(test)]
#[path = "workspace_dock_tests.rs"]
mod tests;
