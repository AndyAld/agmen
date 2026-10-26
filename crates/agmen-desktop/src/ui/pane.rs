//! The center: panes with a tab bar and a breadcrumb bar (frame "Desktop ·
//! chat", nodes 115 and 130), and a group that splits them side by side.

use std::collections::BTreeMap;
use std::rc::Rc;

use agmen_proto::IdentityId;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
};

use super::chat::ChatView;
use super::components::{Button, ButtonStyle, Color, Label, LabelSize, Tab};
use super::ref_chip::RefEvent;
use super::tokens::{BORDER_WIDTH, BREADCRUMB_HEIGHT, SPACE_8, SPACE_14, TAB_BAR_HEIGHT};
use crate::backend::Snapshot;
use crate::icons::UiIcon;
use crate::refs::RefTarget;

/// The most tabs one pane holds; opening another closes the leftmost
/// inactive tab.
pub(crate) const MAX_TABS: usize = 32;

/// The most panes the center splits into.
pub(crate) const MAX_PANES: usize = 4;

/// A Settings page. Task 17 builds Appearance; the other pages follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum SettingsPage {
    #[default]
    Appearance,
}

/// What a tab shows. Items whose view is not built yet show a placeholder
/// until their task (Tasks 10 to 12, 17 and 19).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Tasks 10 to 19 open file, diff, role, plan, spec and settings tabs"
    )
)]
pub(crate) enum TabItem {
    Chat,
    /// An agent's session view.
    Agent(IdentityId),
    File {
        worktree: String,
        path: String,
    },
    Diff {
        worktree: String,
        path: String,
    },
    /// A role's XML file, by role name.
    RoleFile(String),
    Plan,
    /// A spec file, by path.
    Spec(String),
    Settings(SettingsPage),
    /// A reference chip's target whose view does not exist yet.
    // shortcut: a placeholder tab; Task 12 routes each kind to its home
    // (spec 9.3) and removes this variant.
    Ref(RefTarget),
}

/// An agent's handle and role, for its tab.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentName {
    handle: String,
    role: String,
}

/// What tabs and breadcrumbs name, from the snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PaneNames {
    project: String,
    agents: BTreeMap<IdentityId, AgentName>,
}

/// The last part of a slash separated path.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl PaneNames {
    pub(crate) fn from_snapshot(snapshot: &Snapshot) -> Self {
        let agents = snapshot
            .roster
            .iter()
            .flat_map(|group| &group.agents)
            .map(|agent| {
                (
                    agent.id.clone(),
                    AgentName {
                        handle: agent.handle.clone(),
                        role: agent.role.clone(),
                    },
                )
            })
            .collect();
        Self {
            project: snapshot.project.clone(),
            agents,
        }
    }

    /// An agent's handle, or its id when the roster does not have it.
    fn handle(&self, id: &IdentityId) -> String {
        self.agents
            .get(id)
            .map_or_else(|| id.to_string(), |agent| agent.handle.clone())
    }

    /// The tab a reference chip opens: an agent in the roster as its agent
    /// tab, anything else as a placeholder until Task 12 routes it.
    pub(crate) fn tab_for_ref(&self, target: &RefTarget) -> TabItem {
        if let RefTarget::Agent(handle) = target
            && let Some((id, _)) = self.agents.iter().find(|(_, a)| a.handle == *handle)
        {
            return TabItem::Agent(id.clone());
        }
        TabItem::Ref(target.clone())
    }

    /// The tab's label, as the mockup's tab bar (console.py `tab_bar`).
    pub(crate) fn tab_label(&self, item: &TabItem) -> String {
        match item {
            TabItem::Chat => "chat".into(),
            TabItem::Agent(id) => self.handle(id),
            TabItem::File { path, .. } => file_name(path).into(),
            TabItem::Diff { path, .. } => format!("{} (diff)", file_name(path)),
            TabItem::RoleFile(role) => format!("{role}.xml"),
            TabItem::Plan => "plan".into(),
            TabItem::Spec(path) => file_name(path).into(),
            TabItem::Settings(_) => "settings".into(),
            TabItem::Ref(target) => ref_label(target),
        }
    }

    /// The breadcrumb's parts, outermost first.
    pub(crate) fn breadcrumb(&self, item: &TabItem) -> Vec<String> {
        match item {
            TabItem::File { worktree, path } | TabItem::Diff { worktree, path } => {
                vec![worktree.clone(), path.clone()]
            }
            TabItem::RoleFile(_) => vec!["roles".into(), self.tab_label(item)],
            TabItem::Settings(page) => vec!["settings".into(), page.label().into()],
            TabItem::Spec(path) => vec![self.project.clone(), path.clone()],
            TabItem::Chat | TabItem::Agent(_) | TabItem::Plan | TabItem::Ref(_) => {
                vec![self.project.clone(), self.tab_label(item)]
            }
        }
    }

    /// The tab's icon and its color (console.py `tab_bar` and `plan_tabs`).
    fn tab_icon(&self, item: &TabItem) -> (UiIcon, Color) {
        match item {
            TabItem::Chat | TabItem::Plan => (UiIcon::Chat, Color::Accent),
            TabItem::Agent(id) => (
                UiIcon::Sparkle,
                self.agents.get(id).map_or(Color::Placeholder, |agent| {
                    Color::Role(agent.role.clone().into())
                }),
            ),
            // shortcut: Lucide's file icon; Task 11 shows the icon theme's
            // file icons here once the Project panel reads them.
            TabItem::File { .. } => (UiIcon::File, Color::Muted),
            TabItem::Diff { .. } => (UiIcon::Diff, Color::Warning),
            TabItem::RoleFile(role) => (UiIcon::File, Color::Role(role.clone().into())),
            TabItem::Spec(_) => (UiIcon::File, Color::Warning),
            TabItem::Settings(_) => (UiIcon::Settings, Color::Placeholder),
            TabItem::Ref(RefTarget::File { .. }) => (UiIcon::File, Color::Accent),
            TabItem::Ref(RefTarget::Commit(_) | RefTarget::Branch { .. }) => {
                (UiIcon::Git, Color::Success)
            }
            TabItem::Ref(_) => (UiIcon::File, Color::Placeholder),
        }
    }
}

/// A reference as its chip reads: `src/main.rs:2-8`, `#9`.
fn ref_label(target: &RefTarget) -> String {
    match target {
        RefTarget::Agent(name)
        | RefTarget::Person(name)
        | RefTarget::Role(name)
        | RefTarget::Commit(name)
        | RefTarget::Item(name)
        | RefTarget::Lane(name) => name.clone(),
        RefTarget::Branch { name, at: None } => name.clone(),
        RefTarget::Branch { name, at: Some(at) } => format!("{name}@{at}"),
        RefTarget::File { path, lines: None } => file_name(path).into(),
        RefTarget::File {
            path,
            lines: Some((from, to)),
        } => format!("{}:{from}-{to}", file_name(path)),
        RefTarget::Pr(number) => format!("#{number}"),
    }
}

impl SettingsPage {
    const fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
        }
    }
}

/// Labels in the UI font; the rest are names people type, in the code font.
const fn label_is_code(item: &TabItem) -> bool {
    !matches!(item, TabItem::Chat | TabItem::Plan | TabItem::Settings(_))
}

/// What a pane tells its group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneEvent {
    /// The breadcrumb's "Show ledger" was switched on or off.
    ShowLedger(bool),
    /// The person clicked in the pane.
    Focused,
    /// The last tab closed.
    Emptied,
}

/// One pane: a tab bar, the active tab's breadcrumb, and its content. Tests
/// find the pane as `"pane-{n}"`, its tabs as `"pane-{n}:tab-{i}"` and the
/// toggle as `"pane-{n}:show-ledger"`.
#[derive(Debug)]
pub(crate) struct Pane {
    /// The pane's number in its group, for element ids.
    number: usize,
    names: Rc<PaneNames>,
    tabs: Vec<TabItem>,
    active: usize,
    show_ledger: bool,
    /// The chat this pane shows on its Chat tab, if it hosts it.
    chat: Option<Entity<ChatView>>,
    /// Redraws the tab bar when the chat's badge changes.
    _chat_changes: Option<Subscription>,
}

impl EventEmitter<PaneEvent> for Pane {}

impl Pane {
    pub(crate) fn new(number: usize, names: Rc<PaneNames>) -> Self {
        Self {
            number,
            names,
            tabs: Vec::new(),
            active: 0,
            show_ledger: false,
            chat: None,
            _chat_changes: None,
        }
    }

    /// Shows `chat` on this pane's Chat tab.
    pub(crate) fn set_chat(&mut self, chat: Entity<ChatView>, cx: &mut Context<Self>) {
        self._chat_changes = Some(cx.observe(&chat, |_, _, cx| cx.notify()));
        self.chat = Some(chat);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) const fn chat(&self) -> Option<&Entity<ChatView>> {
        self.chat.as_ref()
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the palette (Task 18) lists open tabs")
    )]
    pub(crate) fn tabs(&self) -> &[TabItem] {
        &self.tabs
    }

    pub(crate) fn active_item(&self) -> Option<&TabItem> {
        self.tabs.get(self.active)
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "only tests read it so far"))]
    pub(crate) const fn show_ledger(&self) -> bool {
        self.show_ledger
    }

    /// Activates `item`'s tab, opening one after the active tab when there
    /// is none.
    pub(crate) fn open(&mut self, item: TabItem, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.iter().position(|tab| *tab == item) {
            self.activate(index, cx);
            return;
        }
        if self.tabs.len() >= MAX_TABS {
            let active = self.active;
            if let Some(oldest) = (0..self.tabs.len()).find(|i| *i != active) {
                self.tabs.remove(oldest);
                if oldest < self.active {
                    self.active -= 1;
                }
            }
        }
        let at = if self.tabs.is_empty() {
            0
        } else {
            self.active + 1
        };
        self.tabs.insert(at, item);
        self.active = at;
        cx.notify();
    }

    /// Activates the tab at `index`; an index past the last tab does nothing.
    pub(crate) fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() && index != self.active {
            self.active = index;
            cx.notify();
        }
    }

    /// Closes the tab at `index`; the tab to its right (or left, at the end)
    /// becomes active when it was.
    pub(crate) fn close(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        cx.notify();
        if self.tabs.is_empty() {
            cx.emit(PaneEvent::Emptied);
        }
    }

    fn set_show_ledger(&mut self, on: bool, cx: &mut Context<Self>) {
        self.show_ledger = on;
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_show_ledger(on, cx));
        }
        cx.emit(PaneEvent::ShowLedger(on));
        cx.notify();
    }

    fn render_tab(&self, index: usize, item: &TabItem, cx: &mut Context<Self>) -> Tab {
        let (icon, icon_color) = self.names.tab_icon(item);
        let tab = Tab::new(
            SharedString::from(format!("pane-{}:tab-{index}", self.number)),
            icon,
            self.names.tab_label(item),
        )
        .pane()
        .icon_color(icon_color)
        .selected(index == self.active)
        .on_click(cx.listener(move |this, _, _, cx| this.activate(index, cx)))
        .on_close(cx.listener(move |this, _, _, cx| this.close(index, cx)));
        let tab = match (item, &self.chat) {
            (TabItem::Chat, Some(chat)) => tab.badge(chat.read(cx).badge()),
            _ => tab,
        };
        if label_is_code(item) { tab.code() } else { tab }
    }

    fn render_breadcrumb(&self, item: &TabItem, cx: &mut Context<Self>) -> AnyElement {
        let parts = self.names.breadcrumb(item);
        let last = parts.len().saturating_sub(1);
        let mut bar = div()
            .h(BREADCRUMB_HEIGHT)
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_8)
            .px(SPACE_14)
            .border_b(BORDER_WIDTH)
            .border_color(super::rule_color(cx));
        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 {
                bar = bar.child(Label::new("›").size(LabelSize::Small).color(Color::Muted));
            }
            let color = if i == last {
                Color::Default
            } else {
                Color::Placeholder
            };
            bar = bar.child(Label::new(part).code().color(color));
        }
        let on = self.show_ledger;
        bar.child(div().flex_1())
            .when(*item == TabItem::Chat, |this| {
                this.child(
                    Button::new(
                        SharedString::from(format!("pane-{}:show-ledger", self.number)),
                        "Show ledger",
                    )
                    .style(ButtonStyle::Subtle)
                    .selected(on)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_show_ledger(!on, cx))),
                )
            })
            .into_any_element()
    }
}

impl Render for Pane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs: Vec<Tab> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, item)| self.render_tab(index, item, cx))
            .collect();
        let active = self.active_item().cloned();
        let selector = format!("pane-{}", self.number);
        div()
            .debug_selector(|| selector)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(PaneEvent::Focused)),
            )
            .child(
                div()
                    .id("tab-bar")
                    .h(TAB_BAR_HEIGHT)
                    .w_full()
                    .flex()
                    .flex_none()
                    .overflow_x_scroll()
                    .border_b(BORDER_WIDTH)
                    .border_color(super::rule_color(cx))
                    .children(tabs),
            )
            .when_some(active, |this, item| {
                let content = match (&item, &self.chat) {
                    (TabItem::Chat, Some(chat)) => div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .child(chat.clone())
                        .into_any_element(),
                    // shortcut: every other item shows its name until its
                    // view is built (agent and role file in Task 10, file
                    // and diff in Tasks 11 and 12, settings in 17, plan and
                    // spec in 19). A second Chat tab has no chat of its own
                    // yet, so it shows its name too.
                    _ => div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Label::new(self.names.tab_label(&item)).color(Color::Placeholder))
                        .into_any_element(),
                };
                this.child(self.render_breadcrumb(&item, cx)).child(content)
            })
    }
}

/// Which way a split opens the new pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SplitDirection {
    Right,
    Down,
}

/// Why the center did not split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SplitError {
    #[error("the center holds at most {MAX_PANES} panes")]
    Full,
    #[error("the center is already split the other way")]
    OtherDirection,
}

/// What the center tells its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneGroupEvent {
    /// A pane's "Show ledger" was switched on or off.
    ShowLedger(bool),
}

/// A pane and the subscription to its events, dropped together.
#[derive(Debug)]
struct Member {
    pane: Entity<Pane>,
    _events: Subscription,
}

/// The center: one pane, or up to [`MAX_PANES`] side by side or stacked.
#[derive(Debug)]
pub(crate) struct PaneGroup {
    names: Rc<PaneNames>,
    members: Vec<Member>,
    /// `None` until the first split.
    direction: Option<SplitDirection>,
    active: usize,
    /// The next pane's number; numbers are not reused.
    next_number: usize,
    _chat_refs: Option<Subscription>,
}

impl EventEmitter<PaneGroupEvent> for PaneGroup {}

impl PaneGroup {
    /// One pane with the chat open, showing `chat` when given. A click on
    /// one of its reference chips opens a tab, a ⌘-click a split.
    pub(crate) fn new(
        snapshot: &Snapshot,
        chat: Option<Entity<ChatView>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let chat_refs = chat
            .as_ref()
            .map(|chat| cx.subscribe(chat, |this, _, event, cx| this.open_ref(event, cx)));
        let mut group = Self {
            names: Rc::new(PaneNames::from_snapshot(snapshot)),
            members: Vec::new(),
            direction: None,
            active: 0,
            next_number: 0,
            _chat_refs: chat_refs,
        };
        group.add_pane(TabItem::Chat, chat, cx);
        group
    }

    /// Opens `item` in the active pane.
    pub(crate) fn open(&mut self, item: TabItem, cx: &mut Context<Self>) {
        if let Some(member) = self.members.get(self.active) {
            member.pane.update(cx, |pane, cx| pane.open(item, cx));
        }
    }

    /// Opens a reference chip's target: in the active pane on a click, in
    /// a split to the right on a ⌘-click (Zed's split). A center that
    /// cannot split further opens it in the active pane.
    pub(crate) fn open_ref(&mut self, event: &RefEvent, cx: &mut Context<Self>) {
        match event {
            RefEvent::OpenRef(target) => self.open(self.names.tab_for_ref(target), cx),
            RefEvent::OpenRefInSplit(target) => {
                let item = self.names.tab_for_ref(target);
                if self.split(SplitDirection::Right, item.clone(), cx).is_err() {
                    self.open(item, cx);
                }
            }
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the palette (Task 18) moves between panes")
    )]
    pub(crate) fn panes(&self) -> Vec<Entity<Pane>> {
        self.members.iter().map(|m| m.pane.clone()).collect()
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the palette (Task 18) splits the other way")
    )]
    pub(crate) const fn direction(&self) -> Option<SplitDirection> {
        self.direction
    }

    /// Opens `item` in a new pane after the active one, in `direction`.
    ///
    /// # Errors
    ///
    /// [`SplitError::Full`] at [`MAX_PANES`]; [`SplitError::OtherDirection`]
    /// when the center is split the other way.
    pub(crate) fn split(
        &mut self,
        direction: SplitDirection,
        item: TabItem,
        cx: &mut Context<Self>,
    ) -> Result<Entity<Pane>, SplitError> {
        if self.members.len() >= MAX_PANES {
            return Err(SplitError::Full);
        }
        if self.direction.is_some_and(|current| current != direction) {
            return Err(SplitError::OtherDirection);
        }
        // shortcut: one direction per center, so the group is a flat list;
        // nested splits need a tree of groups if people ask for them.
        self.direction = Some(direction);
        let pane = self.add_pane(item, None, cx);
        cx.notify();
        Ok(pane)
    }

    /// Makes a pane holding `item` and puts it after the active pane.
    fn add_pane(
        &mut self,
        item: TabItem,
        chat: Option<Entity<ChatView>>,
        cx: &mut Context<Self>,
    ) -> Entity<Pane> {
        let number = self.next_number;
        self.next_number += 1;
        let names = self.names.clone();
        let pane = cx.new(|cx| {
            let mut pane = Pane::new(number, names);
            if let Some(chat) = chat {
                pane.set_chat(chat, cx);
            }
            pane.open(item, cx);
            pane
        });
        let events = cx.subscribe(&pane, Self::on_pane_event);
        let at = if self.members.is_empty() {
            0
        } else {
            self.active + 1
        };
        self.members.insert(
            at,
            Member {
                pane: pane.clone(),
                _events: events,
            },
        );
        self.active = at;
        pane
    }

    fn on_pane_event(&mut self, pane: Entity<Pane>, event: &PaneEvent, cx: &mut Context<Self>) {
        let Some(index) = self.members.iter().position(|m| m.pane == pane) else {
            return;
        };
        match *event {
            PaneEvent::ShowLedger(on) => cx.emit(PaneGroupEvent::ShowLedger(on)),
            PaneEvent::Focused => self.active = index,
            // The last pane stays, empty, so the center always has one.
            PaneEvent::Emptied if self.members.len() > 1 => {
                self.members.remove(index);
                if index < self.active || self.active >= self.members.len() {
                    self.active = self.active.saturating_sub(1);
                }
                if self.members.len() == 1 {
                    self.direction = None;
                }
                cx.notify();
            }
            PaneEvent::Emptied => {}
        }
    }
}

impl Render for PaneGroup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let down = self.direction == Some(SplitDirection::Down);
        let rule = super::rule_color(cx);
        let panes = self.members.iter().enumerate().map(|(i, member)| {
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .when(i > 0, |this| {
                    let this = if down {
                        this.border_t(BORDER_WIDTH)
                    } else {
                        this.border_l(BORDER_WIDTH)
                    };
                    this.border_color(rule)
                })
                .child(member.pane.clone())
        });
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .when(down, |this| this.flex_col())
            .children(panes)
    }
}

#[cfg(test)]
#[path = "pane_tests.rs"]
mod tests;
