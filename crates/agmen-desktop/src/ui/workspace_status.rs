//! What the status bar says about the active tab, and when it offers the
//! Listener toggle, as the mockup's frames draw them (console.py
//! `statusbar`): the listener's state on the chat, "andy/fx1 · working" on
//! an agent tab (frame 479), "reviewer.xml · XML" on a role file (11514),
//! "andy/fx1 · fix/c015-main-conflict" on a file or diff (4507, 5057) and
//! "settings" on Settings (7908). A child of `workspace`, so it reads the
//! workspace's state directly.

use std::path::Path;

use gpui::{
    App, AppContext as _, Context, InteractiveElement as _, ParentElement as _, SharedString,
    Styled as _, div,
};

use super::Workspace;
use crate::backend::Snapshot;
use crate::ui::components::{Button, Color, DialogFooter, Label, LabelSize, Modal};
use crate::ui::dock::DockSide;
use crate::ui::listener_dialog::{Closed, ListenerDialog};
use crate::ui::pane::TabItem;
use crate::ui::scope_chip::branch_label;
use crate::ui::status_bar::link_label;
use crate::ui::syntax::Language;

impl Workspace {
    /// What the status bar shows in the listener's place for the active
    /// tab, else the plan's state in a project with a plan (Task 19), else
    /// "andy/fx1 · taken over" while a session is taken over (Task 14, node
    /// 1265), or `None` for the listener's state.
    pub(crate) fn status_context(&self, cx: &App) -> Option<SharedString> {
        self.pane_group
            .read(cx)
            .active_tab(cx)
            .and_then(|(item, label)| context_for(&item, &label, &self.snapshot))
            .map(Into::into)
            // Task 19: "ridgeline · planning" in a project with a plan.
            .or_else(|| self.plan_status_context(cx))
            .or_else(|| self.chat.read(cx).take_over_status(cx))
    }

    /// Task 7: what the status bar says about the link to agmend and the
    /// hub, from the chat's last `Update::Link`; `None` for the fixture,
    /// which sends none.
    pub(crate) fn status_link(&self, cx: &App) -> Option<(SharedString, Color)> {
        let chat = self.chat.read(cx);
        chat.link().map(|link| link_label(link, chat.was_live()))
    }

    /// The listener chip's click: the Listener panel when the person has a
    /// listener, and otherwise the dialog that adds one.
    pub(super) fn open_listener(&mut self, cx: &mut Context<Self>) {
        if self.snapshot.listener.is_some() {
            if !self.is_dock_open(DockSide::Right) {
                self.toggle_dock(DockSide::Right, cx);
            }
            return;
        }
        let dialog = cx.new(|_| ListenerDialog::default());
        self.listener_dialog_events = Some(cx.subscribe(&dialog, |this, _, _: &Closed, cx| {
            this.listener_dialog = None;
            this.listener_dialog_events = None;
            cx.notify();
        }));
        self.listener_dialog = Some(dialog);
        cx.notify();
    }

    /// Who the status bar's online count is, while its list is open. Tests
    /// find each row as `online:<handle>`.
    pub(super) fn online_list(&self, cx: &mut Context<Self>) -> Option<Modal> {
        if !self.online_open {
            return None;
        }
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.online_open = false;
            cx.notify();
        };
        let status = &self.snapshot.status;
        let rows = status.online_who.iter().map(|who| {
            let selector = format!("online:{}", who.handle);
            div()
                .debug_selector(move || selector)
                .flex()
                .justify_between()
                .child(Label::new(who.handle.clone()).code())
                .child(
                    Label::new(who.kind)
                        .size(LabelSize::Small)
                        .color(Color::Placeholder),
                )
        });
        // The count without names: a backend that does not say who.
        let note = match (status.online, status.online_who.is_empty()) {
            (0, _) => Some("No one is online."),
            (_, true) => Some("This project does not say who is online."),
            _ => None,
        };
        Some(
            Modal::small("online")
                .on_dismiss(cx.listener(move |this, _, _, cx| close(this, cx)))
                .title(Label::title("Online"))
                .children(rows)
                .children(note.map(|note| Label::new(note).color(Color::Placeholder)))
                .footer(DialogFooter::close(
                    Button::new("online:close", "Close")
                        .on_click(cx.listener(move |this, _, _, cx| close(this, cx))),
                )),
        )
    }

    /// Whether the status bar offers the Listener toggle.
    pub(crate) fn shows_listener_toggle(&self, cx: &App) -> bool {
        let item = self.pane_group.read(cx).active_item(cx);
        offers_listener_toggle(self.is_dock_open(DockSide::Right), item.as_ref())
    }
}

/// The context text for `item`, whose tab reads `label`, so the status bar
/// names an agent or role file as its tab does. Items the snapshot does not
/// know, and tabs no frame draws (the settings file, search, plan and
/// spec), give `None`, which keeps the listener's state.
fn context_for(item: &TabItem, label: &str, snapshot: &Snapshot) -> Option<String> {
    match item {
        TabItem::Agent(id) => snapshot
            .roster
            .iter()
            .flat_map(|group| &group.agents)
            .find(|agent| agent.id == *id)
            .map(|agent| format!("{label} · {}", agent.status.label())),
        TabItem::RoleFile(_) => {
            let language = Language::for_path(Path::new(label)).name();
            Some(format!("{label} · {language}"))
        }
        TabItem::File { worktree, .. } | TabItem::Diff { worktree, .. } => snapshot
            .worktrees
            .iter()
            .find(|wt| wt.handle == *worktree)
            .map(|wt| format!("{} · {}", wt.handle, branch_label(wt))),
        TabItem::Settings(_) => Some("settings".to_owned()),
        TabItem::Chat
        | TabItem::Plan
        | TabItem::Spec(_)
        | TabItem::SettingsFile(_)
        | TabItem::Search
        | TabItem::Recents => None,
    }
}

/// The toggle reopens the closed right dock. Settings draws none (frame
/// 7908), though its dock is closed too.
const fn offers_listener_toggle(right_dock_open: bool, item: Option<&TabItem>) -> bool {
    !right_dock_open && !matches!(item, Some(TabItem::Settings(_)))
}

#[cfg(test)]
#[path = "workspace_status_tests.rs"]
mod tests;
