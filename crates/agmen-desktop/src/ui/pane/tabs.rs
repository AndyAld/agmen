//! One pane: its tab bar, the active tab's breadcrumb and its content.

use std::path::Path;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div,
};

use super::item::TabItem;
use super::names::{PaneNames, label_is_code};
use crate::icons::FileIcons;
use crate::ui::chat::ChatView;
use crate::ui::components::{Breadcrumb, Button, ButtonStyle, Color, Label, Tab};
use crate::ui::tab_views::TabViews;
use crate::ui::tokens::{BORDER_WIDTH, TAB_BAR_HEIGHT};

/// The most tabs one pane holds; opening another closes the leftmost
/// inactive tab other than the chat's.
pub(super) const MAX_TABS: usize = 32;

/// What a pane tells its group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneEvent {
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
    pub(super) active: usize,
    /// The center's chat, which this pane shows on a Chat tab. The group
    /// owns it too, so it outlives any one pane.
    chat: Option<Entity<ChatView>>,
    /// Redraws the tab bar when the chat's badge changes.
    _chat_changes: Option<Subscription>,
    /// The views of the tabs whose view is built.
    views: TabViews,
}

impl EventEmitter<PaneEvent> for Pane {}

impl Pane {
    pub(super) fn new(number: usize, names: Rc<PaneNames>, views: TabViews) -> Self {
        Self {
            number,
            names,
            tabs: Vec::new(),
            active: 0,
            chat: None,
            _chat_changes: None,
            views,
        }
    }

    /// Shows `chat` on a Chat tab in this pane.
    pub(crate) fn set_chat(&mut self, chat: Entity<ChatView>, cx: &mut Context<Self>) {
        self._chat_changes = Some(cx.observe(&chat, |_, _, cx| cx.notify()));
        self.chat = Some(chat);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) const fn chat(&self) -> Option<&Entity<ChatView>> {
        self.chat.as_ref()
    }

    #[cfg(test)]
    pub(crate) const fn number(&self) -> usize {
        self.number
    }

    pub(crate) fn tabs(&self) -> &[TabItem] {
        &self.tabs
    }

    pub(crate) fn active_item(&self) -> Option<&TabItem> {
        self.tabs.get(self.active)
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
            let oldest = self
                .tabs
                .iter()
                .enumerate()
                .position(|(i, tab)| i != active && *tab != TabItem::Chat);
            if let Some(oldest) = oldest {
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
    /// becomes active when it was. The chat's tab does not close, so the
    /// chat always has a pane.
    pub(crate) fn close(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tabs.get(index).is_none_or(|tab| *tab == TabItem::Chat) {
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
        .on_click(cx.listener(move |this, _, _, cx| this.activate(index, cx)));
        // The chat's tab draws no close button; its slot stays, so tabs keep
        // their width (mockup fix 40).
        let tab = if *item == TabItem::Chat {
            tab
        } else {
            tab.on_close(cx.listener(move |this, _, _, cx| this.close(index, cx)))
        };
        let tab = match (item, &self.chat) {
            (TabItem::Chat, Some(chat)) => tab.badge(chat.read(cx).badge()),
            (TabItem::RoleFile(_) | TabItem::SettingsFile(_) | TabItem::Spec(_), _) => {
                match cx.try_global::<FileIcons>() {
                    Some(icons) => {
                        let name = self.names.tab_label(item);
                        tab.file_icon(icons.icon(Path::new(&name), false, false))
                    }
                    None => tab,
                }
            }
            _ => tab,
        };
        if label_is_code(item) { tab.code() } else { tab }
    }

    fn render_breadcrumb(&self, item: &TabItem, cx: &mut Context<Self>) -> Breadcrumb {
        let parts = self
            .names
            .breadcrumb(item)
            .into_iter()
            .map(SharedString::from);
        let bar = Breadcrumb::new(parts);
        match (item, &self.chat) {
            (TabItem::Chat, Some(chat)) => {
                let on = chat.read(cx).show_ledger();
                let chat = chat.clone();
                bar.end(
                    Button::new(
                        SharedString::from(format!("pane-{}:show-ledger", self.number)),
                        "Show ledger",
                    )
                    .style(ButtonStyle::Subtle)
                    .selected(on)
                    .on_click(move |_, _, cx| {
                        chat.update(cx, |chat, cx| chat.set_show_ledger(!on, cx));
                    }),
                )
            }
            _ => bar,
        }
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
        let view = active
            .as_ref()
            .and_then(|item| self.views.view(item, &self.tabs, cx));
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
                    .border_color(crate::ui::rule_color(cx))
                    .children(tabs),
            )
            .when_some(active, |this, item| {
                if let Some(view) = view {
                    let this = if TabViews::draws_breadcrumb(&item) {
                        this
                    } else {
                        this.child(self.render_breadcrumb(&item, cx))
                    };
                    return this.child(div().flex_1().min_h_0().flex().child(view));
                }
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
                    // spec in 19).
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
