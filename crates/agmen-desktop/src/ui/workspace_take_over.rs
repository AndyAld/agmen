//! Task 14's workspace wiring: ⌥O, ctrl-` and the agent view's Open in
//! terminal reach the chat from anywhere in the window, and the Agent
//! panel follows the chat's take-overs. A child of `workspace`, so it
//! sets the workspace's state directly.

use std::rc::Rc;

use gpui::{Context, Div, InteractiveElement as _};

use super::Workspace;
use crate::keymap::{OpenInTerminal, TakeOverAgent, ToggleTerminal};
use crate::ui::chat::TerminalPopup;
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

    /// Takes the chat's snapshot whole: the chat hears the backend's
    /// updates and the workspace does not. The Agent panel shows its agents
    /// and take-overs (a live switch moves an agent's harness), the Agents
    /// tab its roster, and reference chips route against its worktrees and
    /// roles, so a new worktree's chips open and a removed one's do not
    /// (review C294).
    // shortcut: the Git and Project panels, the center's TabViews and the
    // plan room keep the snapshot they were built with, so after an update
    // a new worktree's file chip opens a placeholder tab and its branch chip
    // a Git panel on another worktree (C314). Latent while nothing sends
    // such updates; they follow too once the live backend (ADR-0011) sends
    // worktree changes.
    pub(super) fn follow_chat(&mut self, cx: &mut Context<Self>) {
        let chat = Rc::clone(self.chat.read(cx).snapshot());
        if Rc::ptr_eq(&chat, &self.snapshot) {
            return;
        }
        if chat.roster != self.snapshot.roster {
            let roster = chat.roster.clone();
            self.agents_panel
                .update(cx, |panel, cx| panel.set_roster(roster, cx));
        }
        self.agents_panel.update(cx, |panel, cx| {
            panel.set_managed(chat.managed_agents.clone(), chat.agent_error.clone(), cx)
        });
        self.snapshot = chat;
        cx.notify();
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
