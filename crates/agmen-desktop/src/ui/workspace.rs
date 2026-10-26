//! The window's root view: title bar, docks, center and status bar.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
    actions, div,
};

use super::agents_panel::{AgentsPanel, AgentsPanelEvent};
use super::chat::ChatView;
use super::dock::{Dock, DockSide, LeftDockTabs};
use super::pane::{PaneGroup, TabItem};
use super::status_bar::StatusBar;
use super::title_bar::TitleBar;
use super::tokens::{BORDER_WIDTH, LEFT_DOCK_WIDTH, RIGHT_DOCK_WIDTH};
use crate::appearance::Appearance;
use agmen_proto::IdentityId;

use crate::backend::{AgentInfo, Snapshot};
use crate::icons::FileIcons;

#[path = "workspace_dock.rs"]
mod dock_wiring;
use dock_wiring::LeftDockState;
#[path = "workspace_screen.rs"]
mod screen_state;

actions!(workspace, [ToggleLeftDock, ToggleRightDock, Quit]);

/// The window's root view.
pub(crate) struct Workspace {
    focus_handle: FocusHandle,
    snapshot: Snapshot,
    left_dock: Dock,
    right_dock: Dock,
    agents_panel: Entity<AgentsPanel>,
    /// The center: the chat, and the tabs and splits its chips open.
    ///
    /// The plan's `Workspace::split(direction)` lives here as
    /// `PaneGroup::split(direction, item, cx)`. The palette (Task 18) reaches
    /// it by dispatching the `SplitRight` action (`keys.rs`), which
    /// `workspace_dock.rs` routes to `PaneGroup::split_active_right`; a split
    /// down gets its own action the same way.
    pane_group: Entity<PaneGroup>,
    /// The agent the right dock shows; `None` shows the Listener panel.
    selected_agent: Option<IdentityId>,
    left: LeftDockState,
    _agents_events: Subscription,
}

impl Workspace {
    pub(crate) fn new(snapshot: Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let agents_panel = cx.new(|_| AgentsPanel::new(snapshot.roster.clone()));
        let agents_events = cx.subscribe(&agents_panel, |this, _, event, cx| match event {
            AgentsPanelEvent::Selected(id) => this.select_agent(id, cx),
        });
        let chat = cx.new(|cx| ChatView::new(snapshot.clone(), window, cx));
        let tabs = starting_tabs(&snapshot);
        let pane_group = cx.new(|cx| PaneGroup::new(&snapshot, Some(chat), tabs, cx));
        let left = LeftDockState::new(&snapshot, cx);
        Self {
            focus_handle: cx.focus_handle(),
            snapshot,
            pane_group,
            left_dock: Dock::new(LEFT_DOCK_WIDTH),
            right_dock: Dock::new(RIGHT_DOCK_WIDTH),
            agents_panel,
            selected_agent: None,
            left,
            _agents_events: agents_events,
        }
    }

    pub(crate) const fn is_dock_open(&self, side: DockSide) -> bool {
        match side {
            DockSide::Left => self.left_dock.is_open(),
            DockSide::Right => self.right_dock.is_open(),
        }
    }

    pub(crate) fn toggle_dock(&mut self, side: DockSide, cx: &mut Context<Self>) {
        match side {
            DockSide::Left => self.left_dock.toggle(),
            DockSide::Right => {
                self.right_dock.toggle();
                // Closing the dock drops the selection, so the status bar's
                // Listener toggle reopens it on the Listener panel.
                if !self.right_dock.is_open() {
                    self.set_selection(None, cx);
                }
            }
        }
        cx.notify();
    }

    /// The one place the selection changes; the Agents dock highlights it.
    fn set_selection(&mut self, id: Option<IdentityId>, cx: &mut Context<Self>) {
        self.selected_agent = id.clone();
        self.left.shown_role = None;
        self.agents_panel
            .update(cx, |panel, cx| panel.set_selected(id, cx));
    }

    /// Shows the agent's panel in the right dock, opening the dock. An agent
    /// without details (the listener, other people's agents) shows the
    /// Listener panel and no highlighted row.
    fn select_agent(&mut self, id: &IdentityId, cx: &mut Context<Self>) {
        let shown = self.snapshot.agents.contains_key(id).then(|| id.clone());
        self.set_selection(shown, cx);
        self.right_dock.open();
        cx.notify();
    }

    /// What the status bar shows in the listener's place: "settings" while
    /// Settings is the active tab (frame "Desktop · settings", node 8240),
    /// `None` for the listener.
    pub(crate) fn status_context(&self, cx: &App) -> Option<SharedString> {
        matches!(
            self.pane_group.read(cx).active_item(cx),
            Some(TabItem::Settings(_))
        )
        .then(|| "settings".into())
    }

    /// The agent the right dock shows, or `None` for the Listener panel.
    /// Agents without details (the listener itself, other people's agents
    /// the fixture has no data for) show the Listener panel.
    pub(crate) fn shown_agent(&self) -> Option<&AgentInfo> {
        self.selected_agent
            .as_ref()
            .and_then(|id| self.snapshot.agents.get(id))
    }
}

/// The tabs the center opens behind the chat: the mockup's agent tab and
/// role file tab (console.py `tab_bar`: andy/fx1 and reviewer.xml), when the
/// roster has that agent and role.
// shortcut: the mockup's tabs stand in for the tabs the person left open.
// No task in the desktop plan restores tabs yet; the change that restores
// them once agmend keeps them deletes this function and its call in
// Workspace::new, so production windows stop opening the mockup's tabs.
fn starting_tabs(snapshot: &Snapshot) -> Vec<TabItem> {
    let mut tabs: Vec<TabItem> = mockup_agent(snapshot)
        .map(TabItem::Agent)
        .into_iter()
        .collect();
    let mut agents = snapshot.roster.iter().flat_map(|group| &group.agents);
    if agents.any(|agent| agent.role == MOCKUP_ROLE) {
        tabs.push(TabItem::RoleFile(MOCKUP_ROLE.into()));
    }
    tabs
}

/// The role whose file the mockup opens: reviewer.xml.
const MOCKUP_ROLE: &str = "reviewer";

/// The handle of the agent the mockup opens: your fx1, such as `andy/fx1`.
fn mockup_handle(snapshot: &Snapshot) -> String {
    format!("{}/fx1", snapshot.me)
}

/// The mockup's agent, when the roster has it.
fn mockup_agent(snapshot: &Snapshot) -> Option<IdentityId> {
    let fx1 = mockup_handle(snapshot);
    snapshot
        .roster
        .iter()
        .flat_map(|group| &group.agents)
        .find(|agent| agent.handle == fx1)
        .map(|agent| agent.id.clone())
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Fetch the OS file icons the last frame asked for, off the render path.
        cx.defer(FileIcons::fetch_wanted);
        let appearance = cx.global::<Appearance>();
        let theme = &appearance.theme;
        let fonts = &appearance.fonts;
        // Text sizes are rems, so they follow the person's UI font size.
        window.set_rem_size(appearance.fonts.ui_size);

        let mut body = div().flex().flex_row().flex_1().min_h_0();
        if self.is_dock_open(DockSide::Left) {
            body = body.child(
                div()
                    .debug_selector(|| "left-dock".into())
                    .w(self.left_dock.width())
                    .flex_none()
                    .h_full()
                    .flex()
                    .border_r(BORDER_WIDTH)
                    .border_color(theme.border_variant)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .child(LeftDockTabs(self.left.tab))
                            .child(self.left_dock_content()),
                    ),
            );
        }
        body = body.child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .child(self.pane_group.clone()),
        );
        if self.is_dock_open(DockSide::Right) {
            let panel = self.right_panel();
            body = body.child(
                div()
                    .w(self.right_dock.width())
                    .flex_none()
                    .h_full()
                    .flex()
                    .border_l(BORDER_WIDTH)
                    .border_color(theme.border_variant)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            .children(panel),
                    ),
            );
        }

        Self::dock_actions(div(), cx)
            .track_focus(&self.focus_handle)
            .key_context("Workspace")
            .on_action(cx.listener(|this, _: &ToggleLeftDock, _, cx| {
                this.toggle_dock(DockSide::Left, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRightDock, _, cx| {
                this.toggle_dock(DockSide::Right, cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(fonts.ui_family.clone())
            .child(TitleBar::new(&self.snapshot))
            .child(body)
            .child(
                StatusBar::new(&self.snapshot, self.is_dock_open(DockSide::Right))
                    .panels(self.panel_buttons(cx))
                    .when_some(self.status_context(cx), StatusBar::context),
            )
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
