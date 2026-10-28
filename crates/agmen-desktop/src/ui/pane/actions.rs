//! What the pane keys do to the center: activate a pane, close the active
//! tab, split right. A child of `pane::group`, so it reads the group's
//! members.

use gpui::{App, Context};

use super::{PaneGroup, SplitDirection, SplitError, TabItem};

impl PaneGroup {
    /// Makes the pane at `index` active; past the last pane, nothing.
    pub(crate) fn activate_pane(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.members.len() && index != self.active {
            self.active = index;
            cx.notify();
        }
    }

    /// Closes the active pane's active tab.
    pub(crate) fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        if let Some(member) = self.members.get(self.active) {
            member
                .pane
                .update(cx, |pane, cx| pane.close(pane.active, cx));
        }
    }

    /// Opens the active pane's active tab in a new pane on its right, or
    /// the chat when the pane is empty.
    pub(crate) fn split_active_right(&mut self, cx: &mut Context<Self>) {
        let item = self.active_item(cx).unwrap_or(TabItem::Chat);
        // A center at MAX_PANES, or one split down, stays as it is: the key
        // does nothing, as Zed's does at its limit.
        match self.split(SplitDirection::Right, item, cx) {
            Ok(_) | Err(SplitError::Full | SplitError::OtherDirection) => {}
        }
    }

    /// The active pane's active tab.
    pub(crate) fn active_item(&self, cx: &App) -> Option<TabItem> {
        self.members
            .get(self.active)
            .and_then(|member| member.pane.read(cx).active_item().cloned())
    }

    /// The active pane's index.
    #[cfg(test)]
    pub(crate) const fn active_pane(&self) -> usize {
        self.active
    }
}
