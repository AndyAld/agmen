//! The center's panes, side by side or stacked.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div,
};

use super::item::TabItem;
use super::names::PaneNames;
use super::tabs::{Pane, PaneEvent};
use crate::backend::Snapshot;
use crate::ui::chat::ChatView;
use crate::ui::ref_chip::RefEvent;
use crate::ui::tab_views::{TabSources, TabViews};
use crate::ui::tokens::BORDER_WIDTH;

/// The most panes the center splits into.
pub(super) const MAX_PANES: usize = 4;

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
    /// The center's one chat. The group holds it, so closing its tab or
    /// its pane keeps its composer text, alerts and backend updates, and
    /// any pane can open it again.
    chat: Option<Entity<ChatView>>,
    _chat_refs: Option<Subscription>,
    /// What the tabs' views read.
    sources: Rc<TabSources>,
}

impl PaneGroup {
    /// One pane with the chat's tab active and `tabs` open after it,
    /// showing `chat` when given. A click on one of the chat's reference
    /// chips opens a tab, a ⌘-click a split.
    pub(crate) fn new(
        snapshot: &Rc<Snapshot>,
        chat: Option<Entity<ChatView>>,
        tabs: Vec<TabItem>,
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
            chat,
            _chat_refs: chat_refs,
            sources: Rc::new(TabSources::from_snapshot(snapshot)),
        };
        let pane = group.add_pane(TabItem::Chat, cx);
        pane.update(cx, |pane, cx| {
            for item in tabs {
                pane.open(item, cx);
            }
            pane.activate(0, cx);
        });
        group
    }

    /// The active pane's active tab and its label, as its tab shows it.
    pub(crate) fn active_tab(&self, cx: &App) -> Option<(TabItem, String)> {
        let item = self.active_item(cx)?;
        let label = self.names.tab_label(&item);
        Some((item, label))
    }

    /// The center's chat (Task 18: the palette sends spawn requests in it).
    pub(crate) fn chat(&self) -> Option<&Entity<ChatView>> {
        self.chat.as_ref()
    }

    /// The pane with a Chat tab, if one has it.
    fn chat_pane(&self, cx: &App) -> Option<usize> {
        self.members
            .iter()
            .position(|m| m.pane.read(cx).tabs().contains(&TabItem::Chat))
    }

    /// Opens `item` in the active pane. The chat has one tab in the
    /// center: when a pane has it, that pane and tab become active.
    pub(crate) fn open(&mut self, item: TabItem, cx: &mut Context<Self>) {
        if item == TabItem::Chat
            && let Some(index) = self.chat_pane(cx)
        {
            self.active = index;
        }
        if let Some(member) = self.members.get(self.active) {
            member.pane.update(cx, |pane, cx| pane.open(item, cx));
        }
        cx.notify();
    }

    /// Activates `item`'s tab in the active pane, opening one after the
    /// pane's last tab when there is none: where a mockup screen shows the
    /// tab it opened.
    pub(crate) fn open_last(&mut self, item: TabItem, cx: &mut Context<Self>) {
        if let Some(member) = self.members.get(self.active) {
            member.pane.update(cx, |pane, cx| {
                pane.activate(pane.tabs().len().saturating_sub(1), cx);
                pane.open(item, cx);
            });
        }
        cx.notify();
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
    /// The chat has one tab: when a pane has it, that pane becomes active
    /// instead and is returned.
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
        if item == TabItem::Chat
            && let Some(index) = self.chat_pane(cx)
            && let Some(member) = self.members.get(index)
        {
            let pane = member.pane.clone();
            self.open(item, cx);
            return Ok(pane);
        }
        if self.members.len() >= MAX_PANES {
            return Err(SplitError::Full);
        }
        if self.direction.is_some_and(|current| current != direction) {
            return Err(SplitError::OtherDirection);
        }
        // shortcut: one direction per center, so the group is a flat list;
        // nested splits need a tree of groups if people ask for them.
        self.direction = Some(direction);
        let pane = self.add_pane(item, cx);
        cx.notify();
        Ok(pane)
    }

    /// Makes a pane holding `item` and puts it after the active pane.
    fn add_pane(&mut self, item: TabItem, cx: &mut Context<Self>) -> Entity<Pane> {
        let number = self.next_number;
        self.next_number += 1;
        let names = self.names.clone();
        let chat = self.chat.clone();
        let views = TabViews::new(self.sources.clone());
        let pane = cx.new(|cx| {
            let mut pane = Pane::new(number, names, views);
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
            PaneEvent::Focused => self.active = index,
            PaneEvent::Emptied => {
                // The last pane stays.
                if self.members.len() > 1 {
                    self.members.remove(index);
                    if index < self.active || self.active >= self.members.len() {
                        self.active = self.active.saturating_sub(1);
                    }
                    if self.members.len() == 1 {
                        self.direction = None;
                    }
                }
                self.keep_chat_open(cx);
                cx.notify();
            }
        }
    }

    /// Opens the chat in the active pane when no pane has it, so the center
    /// is never left with nothing to reopen it. The chat's tab does not
    /// close, so this only acts if a pane that had it went away.
    fn keep_chat_open(&mut self, cx: &mut Context<Self>) {
        if self.chat_pane(cx).is_none()
            && let Some(member) = self.members.get(self.active)
        {
            member
                .pane
                .update(cx, |pane, cx| pane.open(TabItem::Chat, cx));
        }
    }
}

#[path = "actions.rs"]
mod actions;

impl Render for PaneGroup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let down = self.direction == Some(SplitDirection::Down);
        let rule = crate::ui::rule_color(cx);
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
