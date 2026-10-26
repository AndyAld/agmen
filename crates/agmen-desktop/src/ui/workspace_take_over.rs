//! Task 14's workspace wiring: ⌥O, ctrl-` and the agent view's Open in
//! terminal reach the chat from anywhere in the window, and the Agent
//! panel follows the chat's take-overs. A child of `workspace`, so it
//! sets the workspace's state directly.

use gpui::{Context, Div, InteractiveElement as _};

use super::Workspace;
use crate::ui::chat::TerminalPopup;
use crate::ui::keys::{OpenInTerminal, TakeOverAgent, ToggleTerminal};
use crate::ui::pane::TabItem;

impl Workspace {
    /// The take-over actions on the workspace's root, so they work wherever
    /// focus is.
    pub(super) fn take_over_actions(root: Div, cx: &Context<Self>) -> Div {
        root.on_action(cx.listener(|this, _: &OpenInTerminal, _, cx| {
            this.show_chat(cx);
            this.chat.update(cx, |chat, cx| chat.open_in_terminal(cx));
        }))
        .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
            this.chat
                .update(cx, |chat, cx| chat.toggle_terminal(window, cx));
        }))
        .on_action(cx.listener(|this, action: &TakeOverAgent, _, cx| {
            // The terminal opens in the chat (spec 5.4), so show the chat.
            this.show_chat(cx);
            let agent = action.0.clone();
            this.chat
                .update(cx, |chat, cx| chat.take_over_agent(agent, cx));
        }))
    }

    fn show_chat(&mut self, cx: &mut Context<Self>) {
        self.pane_group
            .update(cx, |group, cx| group.open(TabItem::Chat, cx));
    }

    /// Takes the chat's agents and take-overs, which the Agent panel shows:
    /// the chat hears the backend's updates; the workspace does not.
    pub(super) fn follow_chat(&mut self, cx: &mut Context<Self>) {
        let chat = self.chat.read(cx).snapshot();
        if chat.take_overs != self.snapshot.take_overs || chat.agents != self.snapshot.agents {
            self.snapshot.take_overs = chat.take_overs.clone();
            self.snapshot.agents = chat.agents.clone();
            cx.notify();
        }
    }

    /// Opens the take-over terminal's menu or dialog, as a screen's frame
    /// shows it.
    pub(super) fn show_terminal_popup(&mut self, popup: TerminalPopup, cx: &mut Context<Self>) {
        self.chat
            .update(cx, |chat, cx| chat.show_terminal_popup(popup, cx));
    }
}

#[cfg(test)]
#[path = "workspace_take_over_tests.rs"]
mod tests;
