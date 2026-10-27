//! The window's root view: title bar, docks, center and status bar.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Window, div,
};

use super::agents_panel::{AgentsPanel, AgentsPanelEvent};
use super::chat::ChatView;
use super::dock::{Dock, DockSide, LeftDockTabs};
use super::pane::{PaneGroup, PaneGroupEvent, TabItem};
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
// agmen-c7 fix-hosting: the status bar's context text and Listener toggle.
#[path = "workspace_status.rs"]
mod status;
// Task 18: the command palette.
#[path = "workspace_palette.rs"]
mod palette;
// Task 13: the toast over the window and the alert count's action.
#[path = "workspace_alerts.rs"]
mod alert_wiring;
pub(crate) use alert_wiring::OpenAlerts;
// Task 14: take-over keys and the Agent panel following the chat.
#[path = "workspace_take_over.rs"]
mod take_over;
// Task 19: the planning room, plan panel and kick-off dialog.
#[path = "workspace_plan.rs"]
mod plan_wiring;
// Task 12 Step 1b: where a reference chip opens.
#[path = "workspace_refs.rs"]
mod ref_routes;
pub(super) use ref_routes::{RefRoute, route};

// Task 18: the actions moved to src/keymap.rs with their bindings.
pub(crate) use crate::keymap::{Quit, ToggleLeftDock, ToggleRightDock};

/// The window's root view.
pub(crate) struct Workspace {
    focus_handle: FocusHandle,
    snapshot: Rc<Snapshot>,
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
    /// Task 18: the command palette, while open.
    palette: palette::PaletteState,
    /// Task 19: the planning room's views, when the project has a plan.
    plan: plan_wiring::PlanWiring,
    _agents_events: Subscription,
    /// Closes the Role panel with its role file's tab.
    _pane_events: Subscription,
    /// Task 12 Step 1b: the chat's reference chips, which open a tab or a
    /// panel (`workspace_refs.rs`).
    _chat_refs: Subscription,
    // Task 13: the status bar's counts and the toast come from the chat;
    // Task 14: it also holds the take-over terminal.
    chat: Entity<ChatView>,
    _chat_changes: Subscription,
}

impl Workspace {
    pub(crate) fn new(snapshot: Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // agmen-c7 fix-hosting: the first frame draws the snapshot's file
        // icons, even in a window macOS will not draw again, and the center
        // shares the snapshot.
        super::icon_preload::preload_snapshot(&snapshot, cx);
        let snapshot = Rc::new(snapshot);
        let agents_panel = cx.new(|_| AgentsPanel::new(snapshot.roster.clone()));
        let agents_events = cx.subscribe(&agents_panel, |this, _, event, cx| match event {
            AgentsPanelEvent::Selected(id) => this.select_agent(id, cx),
        });
        let chat = cx.new(|cx| ChatView::new(Rc::clone(&snapshot), window, cx));
        // Task 13: the status bar's counts and the toast redraw on any change;
        // Task 14: the roster and the Agent panel follow the chat's snapshot.
        let chat_changes = cx.observe(&chat, |this, _, cx| {
            this.follow_chat(cx);
            cx.notify();
        });
        let tabs = starting_tabs(&snapshot);
        let pane_group = cx.new(|cx| PaneGroup::new(&snapshot, Some(chat.clone()), tabs, cx));
        let pane_events = cx.subscribe(&pane_group, |this, _, event, cx| match event {
            PaneGroupEvent::TabClosed(item) => this.tab_closed(item, cx),
        });
        let chat_refs = cx.subscribe(&chat, |this, _, event, cx| this.open_ref(event, cx));
        let left = LeftDockState::new(&snapshot, cx);
        let plan = plan_wiring::PlanWiring::new(&snapshot, cx);
        Self {
            focus_handle: cx.focus_handle(),
            snapshot,
            pane_group,
            left_dock: Dock::new(LEFT_DOCK_WIDTH),
            right_dock: Dock::new(RIGHT_DOCK_WIDTH),
            agents_panel,
            selected_agent: None,
            left,
            palette: palette::PaletteState::default(),
            plan,
            _agents_events: agents_events,
            _pane_events: pane_events,
            _chat_refs: chat_refs,
            chat,
            _chat_changes: chat_changes,
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
    // Task 19: the plan's spec beside the plan tab (frame 8640, tab 9174).
    tabs.extend(plan_wiring::mockup_spec(snapshot).map(TabItem::Spec));
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
        // Task 18: the palette over everything.
        let palette = self.palette_overlay(window, cx);
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
            let panel = self.right_panel(cx);
            // Task 19: the plan panel is wider than the other panels.
            let width = self.plan_dock_width(cx).unwrap_or(self.right_dock.width());
            body = body.child(
                div()
                    .w(width)
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

        // Task 19: the kick-off dialog over everything.
        let overlay = self.plan_overlay(cx);
        // Tasks 18, 13, 14 and 19: the palette's, alerts', take-over's and
        // plan's actions.
        Self::plan_actions(
            Self::take_over_actions(
                Self::alert_actions(Self::palette_actions(Self::dock_actions(div(), cx), cx), cx),
                cx,
            ),
            cx,
        )
        .relative()
        .track_focus(&self.focus_handle)
        .key_context(crate::keymap::WORKSPACE_CONTEXT)
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
            StatusBar::new(&self.snapshot, self.shows_listener_toggle(cx))
                .panels(self.panel_buttons(cx))
                .alert_counts(self.chat.read(cx).alert_counts())
                .when_some(self.status_context(cx), StatusBar::context)
                .palette_open(self.is_palette_open()),
        )
        .children(palette)
        .children(self.alert_toast(cx))
        .children(overlay)
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
