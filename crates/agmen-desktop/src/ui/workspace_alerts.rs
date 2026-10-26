//! Task 13 in the workspace: the in-app toast floats over the window's top
//! right (frame "Desktop · needs you", node 6004), and clicking it opens the
//! chat tab at the alert, as the status bar's alert count does (node 456).
//! A child of `workspace`, so it reaches the workspace's fields.

use agmen_proto::MessageId;
use gpui::{
    AnyElement, Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _,
    Styled as _, actions, div,
};

use super::Workspace;
use crate::ui::pane::TabItem;
use crate::ui::toast::AlertToast;
use crate::ui::tokens::{SPACE_16, TOAST_TOP};

actions!(
    workspace,
    [
        /// Opens the chat tab at the oldest alert that needs me, else the
        /// oldest open one: a click on the status bar's alert count.
        OpenAlerts,
    ]
);

impl Workspace {
    /// Registers the alert actions on the workspace's root.
    pub(super) fn alert_actions(root: Div, cx: &Context<Self>) -> Div {
        root.on_action(cx.listener(|this, _: &OpenAlerts, _, cx| {
            if let Some(alert) = this.chat.read(cx).oldest_alert() {
                this.open_alert(&alert, cx);
            }
        }))
    }

    /// The chat's toast, placed over the window.
    pub(super) fn alert_toast(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast = self.chat.read(cx).toast()?.clone();
        let workspace = cx.entity().downgrade();
        Some(
            div()
                .absolute()
                .top(TOAST_TOP)
                .right(SPACE_16)
                .child(AlertToast::new(toast).on_click(move |alert, _, cx| {
                    // A toast outlives its workspace only while the window closes.
                    workspace
                        .update(cx, |workspace, cx| workspace.open_alert(&alert, cx))
                        .unwrap_or_default();
                }))
                .into_any_element(),
        )
    }

    /// Shows the chat tab and scrolls it to `alert`; the toast goes.
    fn open_alert(&mut self, alert: &MessageId, cx: &mut Context<Self>) {
        self.pane_group
            .update(cx, |group, cx| group.open(TabItem::Chat, cx));
        self.chat.update(cx, |chat, cx| {
            chat.dismiss_toast(cx);
            chat.jump_to_alert(alert, cx);
        });
    }
}

#[cfg(test)]
#[path = "workspace_alerts_tests.rs"]
mod tests;
