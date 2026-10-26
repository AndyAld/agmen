//! The workspace's command palette: opening it over the center, filling it
//! with what the focused view can run, running the chosen action where the
//! focus was, and the palette-only actions. A child of `workspace`, so the
//! handlers reach its state.

use gpui::{
    AnyElement, AppContext as _, Context, Div, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    Subscription, Window, div,
};

use super::Workspace;
use crate::keymap::{
    self, Scope, SpawnWorker, SpawnWorkerWith, ToggleAgentsPanel, ToggleCommandPalette,
    ToggleRolesPanel, ToggleVimMode,
};
use crate::ui::left_dock::LeftDockTab;
use crate::ui::palette::{CommandPalette, PaletteEntry, PaletteEvent};
use crate::ui::pane::TabItem;
use crate::ui::settings_view::SettingsSources;
use crate::ui::switch_menu;
use crate::ui::tokens::PALETTE_TOP;

/// The query ⌘N and "spawn worker…" open the palette on.
const SPAWN_QUERY: &str = "spawn worker with ";

/// The palette while it is open, and where the focus goes back to.
#[derive(Default)]
pub(crate) struct PaletteState {
    open: Option<(Entity<CommandPalette>, Subscription)>,
    return_focus: Option<FocusHandle>,
    /// A query to open the palette on once the window has drawn, for the
    /// palette screen: which actions are available is read from a drawn
    /// frame.
    pending: Option<&'static str>,
}

impl std::fmt::Debug for PaletteState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaletteState")
            .field("open", &self.open.is_some())
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// The open palette, if any.
    #[cfg(test)]
    pub(crate) fn palette_entity(&self) -> Option<&Entity<CommandPalette>> {
        self.palette.open.as_ref().map(|(palette, _)| palette)
    }

    /// Opens the palette with `query` typed after the next frame.
    pub(super) fn open_palette_after_draw(&mut self, query: &'static str) {
        self.palette.pending = Some(query);
    }

    /// The rows the focused view can run: every command whose action a
    /// view on the focus path handles or the app handles, with a spawn row
    /// per installed agent after "spawn worker…". Zed lists only available
    /// actions too, so take-over's keys show once its view handles them.
    fn palette_entries(&self, window: &Window, cx: &Context<Self>) -> Vec<PaletteEntry> {
        let table = keymap::table();
        let mut entries = Vec::new();
        for command in keymap::commands() {
            let runs =
                command.scope == Scope::App || window.is_action_available(&*command.action, cx);
            if !runs {
                continue;
            }
            let keys = keymap::hint(&table, &*command.action).map(SharedString::from);
            let is_spawn = command.action.partial_eq(&SpawnWorker);
            entries.push(PaletteEntry::new(command.label, command.action).keys(keys));
            if is_spawn {
                entries.extend(self.spawn_entries());
            }
        }
        entries
    }

    /// `agmen: spawn worker with <agent>` for each installed agent
    /// (desktop.md:380); a missing one has no row (fix 28). An agent that
    /// cannot run a worker yet, such as agy 1.1.7 (1.1.15 is needed for
    /// JSON), keeps its row with the switch menu's note as its hint, and
    /// the backend refuses its spawn with that text (C261). The listener's
    /// agent comes first, as the mockup's codex row (node 11489), then the
    /// others in the order the backend lists them.
    fn spawn_entries(&self) -> impl Iterator<Item = PaletteEntry> + '_ {
        let listener = self.snapshot.listener.agent.as_str();
        let installed = self
            .snapshot
            .installed_agents
            .iter()
            .filter(|agent| agent.is_installed());
        let (first, rest) = (installed.clone(), installed);
        first
            .filter(move |agent| agent.name == listener)
            .chain(rest.filter(move |agent| agent.name != listener))
            .map(|agent| {
                PaletteEntry::new(
                    format!("agmen: spawn worker with {}", agent.name),
                    Box::new(SpawnWorkerWith {
                        agent: agent.name.clone().into(),
                    }),
                )
                .note(switch_menu::note(agent))
            })
    }

    /// Opens the palette on `query`, or sets the query when it is open.
    fn open_palette(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((palette, _)) = &self.palette.open {
            palette.update(cx, |palette, cx| palette.set_query(query, window, cx));
            return;
        }
        self.palette.return_focus = window.focused(cx);
        let entries = self.palette_entries(window, cx);
        let palette = cx.new(|cx| CommandPalette::new(entries, window, cx));
        if !query.is_empty() {
            palette.update(cx, |palette, cx| palette.set_query(query, window, cx));
        }
        let events = cx.subscribe_in(&palette, window, |this, _, event, window, cx| {
            let action = match event {
                PaletteEvent::Dismissed => None,
                PaletteEvent::Confirmed(action) => Some(action.boxed_clone()),
            };
            this.close_palette(window, cx);
            // Zed runs the command where the focus was before the palette.
            if let Some(action) = action {
                window.dispatch_action(action, cx);
            }
        });
        self.palette.open = Some((palette, events));
        cx.notify();
    }

    /// Closes the palette and puts the focus back where it was, as Zed's
    /// modal layer does: only while the focus is still in the palette. A
    /// click that closed it keeps whatever the click focused.
    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((palette, _)) = self.palette.open.take() else {
            return;
        };
        let return_focus = self.palette.return_focus.take();
        if palette.focus_handle(cx).contains_focused(window, cx) {
            let focus = return_focus.unwrap_or_else(|| self.focus_handle.clone());
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.open.is_some() {
            self.close_palette(window, cx);
        } else {
            self.open_palette("", window, cx);
        }
    }

    /// Opens the fixture's DELEGATED flow for a worker on `agent` (andy's
    /// ask, the listener's ANSWER and DELEGATED, as at 15:31) and shows the
    /// chat.
    fn spawn_worker(&mut self, agent: &str, cx: &mut Context<Self>) {
        self.pane_group
            .update(cx, |group, cx| group.open(TabItem::Chat, cx));
        if let Some(chat) = self.pane_group.read(cx).chat().cloned() {
            chat.update(cx, |chat, cx| chat.spawn_worker(agent, cx));
        }
    }

    /// Records vim mode in agmen's settings file.
    fn toggle_vim_mode(cx: &Context<Self>) {
        // shortcut: the result goes to stderr only; show it in the window
        // once the status bar has a place for transient notes.
        let Some(sources) = cx.try_global::<SettingsSources>() else {
            eprintln!("agmen-desktop: vim mode not saved: no settings folder is known");
            return;
        };
        let path = crate::appearance::agmen_settings_path(&sources.config);
        match keymap::toggle_vim_mode(&path) {
            Ok(mode) => eprintln!(
                "agmen-desktop: vim mode {mode:?} saved to {}",
                path.display()
            ),
            Err(err) => eprintln!("agmen-desktop: vim mode not saved: {err}"),
        }
    }

    /// Registers the palette's actions and the palette-only actions.
    pub(super) fn palette_actions(root: Div, cx: &Context<Self>) -> Div {
        root.on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
            this.toggle_palette(window, cx);
        }))
        .on_action(cx.listener(|this, _: &SpawnWorker, window, cx| {
            this.open_palette(SPAWN_QUERY, window, cx);
        }))
        .on_action(cx.listener(|this, action: &SpawnWorkerWith, _, cx| {
            this.spawn_worker(&action.agent, cx);
        }))
        .on_action(cx.listener(|this, _: &ToggleAgentsPanel, _, cx| {
            this.toggle_left_tab(LeftDockTab::Agents, cx);
        }))
        .on_action(cx.listener(|this, _: &ToggleRolesPanel, _, cx| {
            this.toggle_left_tab(LeftDockTab::Roles, cx);
        }))
        .on_action(cx.listener(|_, _: &ToggleVimMode, _, cx| Self::toggle_vim_mode(cx)))
    }

    /// The open palette, centered under the title bar over everything.
    pub(super) fn palette_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(query) = self.palette.pending.take() {
            cx.on_next_frame(window, move |this, window, cx| {
                this.open_palette(query, window, cx);
            });
        }
        let (palette, _) = self.palette.open.as_ref()?;
        Some(
            div()
                .absolute()
                .top(PALETTE_TOP)
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .items_start()
                .child(palette.clone())
                .into_any_element(),
        )
    }

    /// Whether the palette is open, for the status bar's hint.
    pub(super) fn is_palette_open(&self) -> bool {
        self.palette.open.is_some()
    }
}

#[cfg(test)]
#[path = "workspace_palette_tests.rs"]
mod tests;
