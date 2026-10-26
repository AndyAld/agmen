//! Take-over in the chat (Task 14, spec 5.4): the terminal of the session
//! the person took over sits between the messages and the composer. A
//! child of `chat`, so it reaches the chat's own state.

use std::collections::BTreeSet;
use std::rc::Rc;

use agmen_proto::{IdentityId, Payload};
use gpui::{
    App, AppContext as _, Context, Entity, Focusable as _, SharedString, Subscription, Window,
};

use super::ChatView;
use crate::backend::{Action, Snapshot};
use crate::ui::add_agent_dialog::AgentDialog;
use crate::ui::message::offers_take_over;
use crate::ui::terminal_view::{TerminalEvent, TerminalView};

/// What a screen opens over the take-over terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalPopup {
    /// The switch agent menu (frame "Desktop · switch agent").
    SwitchMenu,
    /// A dialog over the menu (frame "Desktop · add agent").
    Dialog(AgentDialog),
}

/// The chat's take-over terminal, when a session is taken over.
#[derive(Default)]
pub(super) struct TakeOverSlot {
    terminal: Option<Entity<TerminalView>>,
    /// Hidden with ctrl-`; the session stays taken over.
    hidden: bool,
    /// The sessions taken over at the last update, to spot a new one.
    seen: BTreeSet<IdentityId>,
    _events: Option<Subscription>,
}

impl ChatView {
    /// Follows the snapshot's take-overs: shows the newest one taken over,
    /// keeps the one shown while it lasts, and drops the terminal when the
    /// session is handed back.
    pub(super) fn sync_take_over(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.snapshot.clone();
        let taken: BTreeSet<IdentityId> = snapshot.take_overs.keys().cloned().collect();
        let fresh = taken.difference(&self.take_over.seen).next().cloned();
        let shown = self
            .take_over
            .terminal
            .as_ref()
            .map(|view| view.read(cx).worker().clone())
            .filter(|id| taken.contains(id));
        self.take_over.seen = taken;
        let Some(worker) = fresh
            .clone()
            .or(shown)
            .or_else(|| snapshot.take_overs.keys().next().cloned())
        else {
            self.take_over.terminal = None;
            self.take_over._events = None;
            self.take_over.hidden = false;
            return;
        };
        if fresh.is_some() {
            self.take_over.hidden = false;
        }
        self.show_terminal_of(worker, cx);
    }

    /// Shows `worker`'s take-over terminal, made fresh or with the
    /// snapshot's latest state; with `worker` not taken over it does
    /// nothing.
    fn show_terminal_of(&mut self, worker: IdentityId, cx: &mut Context<Self>) {
        let snapshot = self.snapshot.clone();
        let Some(take_over) = snapshot.take_overs.get(&worker).cloned() else {
            return;
        };
        let (agents, scans) = (snapshot.installed_agents.clone(), snapshot.agent_scans);
        let current = self
            .take_over
            .terminal
            .as_ref()
            .filter(|view| view.read(cx).worker() == &worker)
            .cloned();
        if let Some(view) = current {
            view.update(cx, |view, cx| view.set_state(take_over, agents, scans, cx));
            return;
        }
        let row = snapshot
            .roster
            .iter()
            .flat_map(|group| &group.agents)
            .find(|agent| agent.id == worker);
        let handle = row.map_or_else(|| worker.to_string(), |row| row.handle.clone());
        let role = row.map_or_else(String::new, |row| row.role.clone());
        let view =
            cx.new(|cx| TerminalView::new(worker, handle, role, take_over, agents, scans, cx));
        self.take_over._events = Some(cx.subscribe(&view, |chat, view, event, cx| {
            let TerminalEvent::Act(action) = event;
            chat.act(action.clone(), cx);
            // `act` keeps only this action's error, so a failed scan ends
            // the dialog's Check again with it.
            if let (Action::ScanAgents, Some(error)) = (action, chat.error.clone()) {
                view.update(cx, |view, cx| view.scan_failed(error, cx));
            }
        }));
        self.take_over.terminal = Some(view);
    }

    /// What the chat shows now, after the backend's latest update.
    pub(crate) const fn snapshot(&self) -> &Rc<Snapshot> {
        &self.snapshot
    }

    /// Takes over `agent`'s session: the agent view's Open in terminal.
    /// Already taken over, its terminal shows again, even after ctrl-` hid
    /// it, and takes the keyboard.
    pub(crate) fn take_over_agent(&mut self, agent: IdentityId, cx: &mut Context<Self>) {
        if !self.snapshot.take_overs.contains_key(&agent) {
            self.act(Action::TakeOver(agent), cx);
            return;
        }
        self.take_over.hidden = false;
        self.show_terminal_of(agent, cx);
        if let Some(view) = &self.take_over.terminal {
            view.update(cx, |view, _| view.request_focus());
        }
        cx.notify();
    }

    /// Opens the terminal's menu or a dialog over it; with nothing taken
    /// over it does nothing.
    pub(crate) fn show_terminal_popup(&mut self, popup: TerminalPopup, cx: &mut Context<Self>) {
        if let Some(view) = &self.take_over.terminal {
            view.update(cx, |view, cx| match popup {
                TerminalPopup::SwitchMenu => view.set_menu_open(true, cx),
                TerminalPopup::Dialog(dialog) => view.open_dialog(dialog, cx),
            });
        }
    }

    /// The take-over terminal, while a session is taken over.
    #[cfg(test)]
    pub(crate) fn terminal(&self) -> Option<&Entity<TerminalView>> {
        self.take_over.terminal.as_ref()
    }

    /// The terminal to draw under the messages, unless hidden.
    pub(super) fn shown_terminal(&self) -> Option<Entity<TerminalView>> {
        self.take_over
            .terminal
            .clone()
            .filter(|_| !self.take_over.hidden)
    }

    /// Hides or shows the take-over terminal (ctrl-`, Zed's
    /// `terminal_panel::Toggle`); with nothing taken over it does nothing.
    /// Shown, it takes the keyboard; hidden while it had it, the composer
    /// takes it, so the keys keep reaching the window.
    pub(crate) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.take_over.terminal.clone() else {
            return;
        };
        self.take_over.hidden = !self.take_over.hidden;
        if self.take_over.hidden {
            if view.focus_handle(cx).contains_focused(window, cx) {
                window.focus(&self.composer.focus_handle(cx), cx);
            }
        } else {
            view.update(cx, |view, _| view.request_focus());
        }
        cx.notify();
    }

    /// Takes over the agent of the newest DONE that offers "Open in
    /// terminal" (⌥O); without one it does nothing.
    pub(crate) fn open_in_terminal(&mut self, cx: &mut Context<Self>) {
        let context = self.stream.read(cx).context.clone();
        let agent = self
            .snapshot
            .messages
            .iter()
            .rev()
            .filter(|env| matches!(env.client.payload, Payload::Done { .. }))
            .find(|env| offers_take_over(env, &context))
            .map(|env| env.from.clone());
        if let Some(agent) = agent {
            self.act(Action::TakeOver(agent), cx);
        }
    }

    /// The status bar's context while a session is taken over: "andy/fx1 ·
    /// taken over" (node 1265), for the terminal's agent.
    pub(crate) fn take_over_status(&self, cx: &App) -> Option<SharedString> {
        let view = self.take_over.terminal.as_ref()?;
        Some(format!("{} · taken over", view.read(cx).handle()).into())
    }
}

#[cfg(test)]
#[path = "chat_take_over_tests.rs"]
mod tests;
