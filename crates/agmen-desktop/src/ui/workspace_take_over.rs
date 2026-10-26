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

    /// Takes the chat's agents, take-overs and roster, which the Agent
    /// panel and the left dock's Agents tab show (a live switch moves an
    /// agent's harness): the chat hears the backend's updates; the
    /// workspace does not.
    pub(super) fn follow_chat(&mut self, cx: &mut Context<Self>) {
        let chat = self.chat.read(cx).snapshot();
        let roster = (chat.roster != self.snapshot.roster).then(|| chat.roster.clone());
        let panel = (chat.take_overs != self.snapshot.take_overs
            || chat.agents != self.snapshot.agents)
            .then(|| (chat.take_overs.clone(), chat.agents.clone()));
        if let Some(roster) = roster {
            // The workspace's own copy: make_mut clones it while shared.
            Rc::make_mut(&mut self.snapshot).roster.clone_from(&roster);
            self.agents_panel
                .update(cx, |panel, cx| panel.set_roster(roster, cx));
            cx.notify();
        }
        if let Some((take_overs, agents)) = panel {
            let snapshot = Rc::make_mut(&mut self.snapshot);
            snapshot.take_overs = take_overs;
            snapshot.agents = agents;
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
