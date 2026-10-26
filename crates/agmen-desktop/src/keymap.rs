//! Every key binding in the app, and the command palette's entries.
//!
//! Bindings are Zed's wherever agmen has the same action (finding V86, from
//! Zed's `assets/keymaps/default-macos.json` at 08cbb88). Agents and Roles
//! have no Zed counterpart, so the palette and a click reach them; ⌥O is
//! agmen's own. The palette lists [`commands`]; [`table`] is the one list
//! of bindings, which `bindings_do_not_collide` checks.

use gpui::{Action, App, KeyBinding, SharedString, actions};

#[path = "keymap_vim.rs"]
mod vim;
pub(crate) use vim::toggle_vim_mode;

actions!(
    workspace,
    [
        /// Shows or hides the left dock.
        ToggleLeftDock,
        /// Shows or hides the right dock (the Listener panel).
        ToggleRightDock,
        /// Quits the app.
        Quit,
        /// Shows the Project panel, or hides the dock when it already shows
        /// it (Zed's `project_panel::ToggleFocus`).
        ToggleProjectPanel,
        /// Shows the Git panel, or hides the dock when it already shows it
        /// (Zed's `git_panel::ToggleFocus`).
        ToggleGitPanel,
        /// Shows the Agents panel, or hides the dock when it already shows
        /// it. No Zed counterpart, so no key.
        ToggleAgentsPanel,
        /// Shows the Roles panel, or hides the dock when it already shows
        /// it. No Zed counterpart, so no key.
        ToggleRolesPanel,
        /// Opens the Search tab in the active pane (Zed's
        /// `pane::DeploySearch`).
        OpenSearch,
        /// Opens the Settings tab in the active pane (Zed's
        /// `zed::OpenSettings`).
        OpenSettings,
        /// Closes the active pane's active tab (Zed's `pane::CloseActiveItem`).
        CloseActiveTab,
        /// Splits the active pane, opening its active tab on the right
        /// (Zed's `pane::SplitRight`).
        SplitRight,
        /// Hides or shows the take-over terminal (Zed's
        /// `terminal_panel::Toggle`).
        ToggleTerminal,
        /// Takes over the agent of the newest DONE that offers "Open in
        /// terminal" (agmen's ⌥O); in an agent view, that view's agent.
        OpenInTerminal,
        /// Opens the palette on "spawn worker with ", to pick the agent.
        SpawnWorker,
        /// Records vim mode on or off in agmen's settings (Zed's
        /// `workspace::ToggleVimMode`).
        ToggleVimMode,
    ]
);

actions!(
    command_palette,
    [
        /// Opens the command palette, or closes it when open.
        ToggleCommandPalette,
    ]
);

// Task 13: Escape ends a reply in the composer.
actions!(
    composer,
    [
        /// Ends the composer's reply, so the next message goes to the
        /// recipient again.
        CancelReply,
    ]
);

actions!(
    menu,
    [
        /// Selects the row above, wrapping to the last.
        SelectPrevious,
        /// Selects the row below, wrapping to the first.
        SelectNext,
        /// Runs the selected row.
        Confirm,
        /// Closes the menu.
        Cancel,
    ]
);

/// Activates the pane at this index, counted from 0; past the last pane it
/// does nothing (Zed's `workspace::ActivatePane`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct ActivatePane(pub(crate) usize);

/// Takes over this agent's session in the chat: the agent view's Open in
/// terminal (Task 14). It names its agent, so no key or palette row runs it.
#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct TakeOverAgent(pub(crate) agmen_proto::IdentityId);

/// Asks the listener for a worker on `agent`, one palette row per
/// installed agent.
#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = agmen, no_json)]
pub(crate) struct SpawnWorkerWith {
    pub(crate) agent: SharedString,
}

/// The key context the workspace's root sets.
pub(crate) const WORKSPACE_CONTEXT: &str = "Workspace";
/// The key context a text input sets.
pub(crate) const TEXT_INPUT_CONTEXT: &str = "TextInput";
/// The key context the command palette sets.
pub(crate) const PALETTE_CONTEXT: &str = "CommandPalette";
/// The key context the chat's composer sets.
pub(crate) const COMPOSER_CONTEXT: &str = "Composer";
/// The workspace outside any text input, for keys that type a character on
/// a Mac layout. gpui on macOS runs a matched binding before text input, so
/// a binding there would keep the character from ever being typed.
pub(crate) const WORKSPACE_OUTSIDE_INPUT_CONTEXT: &str = "Workspace && !TextInput";
/// The palette's query input: deeper than [`TEXT_INPUT_CONTEXT`] alone.
pub(crate) const PALETTE_INPUT_CONTEXT: &str = "CommandPalette > TextInput";

/// The command palette's keys, which the status bar's hint shows.
pub(crate) const TOGGLE_PALETTE_KEYS: &str = "cmd-shift-p";
/// Take-over's keys, which the "Open in terminal" chip shows.
pub(crate) const OPEN_IN_TERMINAL_KEYS: &str = "alt-o";

/// The status bar's ⌘⇧P, without building the binding table.
#[must_use]
pub(crate) fn palette_hint() -> String {
    glyphs(TOGGLE_PALETTE_KEYS)
}

/// The "Open in terminal" chip's ⌥O, without building the binding table.
#[must_use]
pub(crate) fn open_in_terminal_hint() -> String {
    glyphs(OPEN_IN_TERMINAL_KEYS)
}

/// One binding: the keys as Zed writes them and the context they apply in.
#[derive(Clone)]
pub(crate) struct Binding {
    pub(crate) keys: &'static str,
    pub(crate) context: Option<&'static str>,
    pub(crate) key_binding: KeyBinding,
}

impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding")
            .field("keys", &self.keys)
            .field("context", &self.context)
            .field("action", &self.key_binding.action().name())
            .finish()
    }
}

/// Where a command's handler lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// On a view in the window: listed when the focused view can run it.
    Window,
    /// On the app, so it runs from any window.
    App,
}

/// A palette entry for a bound or palette-only action.
pub(crate) struct Command {
    pub(crate) label: SharedString,
    pub(crate) action: Box<dyn Action>,
    pub(crate) scope: Scope,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("label", &self.label)
            .field("action", &self.action.name())
            .field("scope", &self.scope)
            .finish()
    }
}

fn bind<A: Action>(keys: &'static str, action: A, context: Option<&'static str>) -> Binding {
    Binding {
        keys,
        context,
        key_binding: KeyBinding::new(keys, action, context),
    }
}

/// Every binding, in the order they are registered. The text input's come
/// first: when two contexts match equally deep, the binding added later
/// wins, and the palette's input overrides the plain input's arrows and
/// enter.
#[must_use]
pub(crate) fn table() -> Vec<Binding> {
    use crate::ui::text_input_actions as input;
    let text = Some(TEXT_INPUT_CONTEXT);
    let ws = Some(WORKSPACE_CONTEXT);
    let mut table = vec![
        bind("backspace", input::Backspace, text),
        bind("delete", input::Delete, text),
        bind("left", input::Left, text),
        bind("right", input::Right, text),
        bind("up", input::Up, text),
        bind("down", input::Down, text),
        bind("shift-left", input::SelectLeft, text),
        bind("shift-right", input::SelectRight, text),
        bind("cmd-a", input::SelectAll, text),
        bind("home", input::Home, text),
        bind("cmd-left", input::Home, text),
        bind("end", input::End, text),
        bind("cmd-right", input::End, text),
        bind("enter", input::Submit, text),
        bind("shift-enter", input::Newline, text),
        bind("cmd-v", input::Paste, text),
        bind("cmd-x", input::Cut, text),
        bind("cmd-c", input::Copy, text),
        // Quit works in every window, the dev views too.
        bind("cmd-q", Quit, None),
        bind(TOGGLE_PALETTE_KEYS, ToggleCommandPalette, ws),
        bind("cmd-b", ToggleLeftDock, ws),
        // Zed shows ⌘R for the right dock; the hint takes the first binding.
        bind("cmd-r", ToggleRightDock, ws),
        bind("cmd-alt-b", ToggleRightDock, ws),
        bind("cmd-shift-e", ToggleProjectPanel, ws),
        bind("ctrl-shift-g", ToggleGitPanel, ws),
        bind("cmd-,", OpenSettings, ws),
        bind("cmd-shift-f", OpenSearch, ws),
        bind("cmd-w", CloseActiveTab, ws),
        bind("cmd-\\", SplitRight, ws),
        bind("ctrl-`", ToggleTerminal, ws),
        // On a Mac layout ⌥O types "ø", and gpui_macos never lets a text
        // input take the character first (prefer_character_input is always
        // false), so ⌥O stays the input's there.
        bind(
            OPEN_IN_TERMINAL_KEYS,
            OpenInTerminal,
            Some(WORKSPACE_OUTSIDE_INPUT_CONTEXT),
        ),
        // The mockup's hint on "spawn worker…"; Zed's cmd-n (new file)
        // has no agmen counterpart.
        bind("cmd-n", SpawnWorker, ws),
    ];
    let pane_keys = [
        "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
    ];
    table.extend(
        pane_keys
            .into_iter()
            .enumerate()
            .map(|(ix, keys)| bind(keys, ActivatePane(ix), ws)),
    );
    // Zed's "Picker > Editor" keys: home, end and tab stay the input's.
    let input = Some(PALETTE_INPUT_CONTEXT);
    table.extend([
        bind("up", SelectPrevious, input),
        bind("down", SelectNext, input),
        bind("enter", Confirm, input),
        bind("escape", Cancel, Some(PALETTE_CONTEXT)),
        // Task 13: the composer's reply; its input binds no escape.
        bind("escape", CancelReply, Some(COMPOSER_CONTEXT)),
    ]);
    table
}

/// The palette's entries, in the order an empty query lists them. Labels
/// follow Zed's "namespace: action" form; "agmen:" stands in for "zed:".
#[must_use]
pub(crate) fn commands() -> Vec<Command> {
    let window = |label: &'static str, action: Box<dyn Action>| Command {
        label: label.into(),
        action,
        scope: Scope::Window,
    };
    let mut commands = vec![
        window("agmen: spawn worker\u{2026}", Box::new(SpawnWorker)),
        window(
            "agmen: open agent in terminal here",
            Box::new(OpenInTerminal),
        ),
        window("terminal panel: toggle", Box::new(ToggleTerminal)),
        window("workspace: toggle left dock", Box::new(ToggleLeftDock)),
        window("workspace: toggle right dock", Box::new(ToggleRightDock)),
        window("agents panel: toggle focus", Box::new(ToggleAgentsPanel)),
        window("project panel: toggle focus", Box::new(ToggleProjectPanel)),
        window("git panel: toggle focus", Box::new(ToggleGitPanel)),
        window("roles panel: toggle focus", Box::new(ToggleRolesPanel)),
        window("pane: deploy search", Box::new(OpenSearch)),
        window("pane: split right", Box::new(SplitRight)),
        window("pane: close active item", Box::new(CloseActiveTab)),
        window("composer: cancel reply", Box::new(CancelReply)),
    ];
    let panes = [
        "workspace: activate pane 1",
        "workspace: activate pane 2",
        "workspace: activate pane 3",
        "workspace: activate pane 4",
        "workspace: activate pane 5",
        "workspace: activate pane 6",
        "workspace: activate pane 7",
        "workspace: activate pane 8",
        "workspace: activate pane 9",
    ];
    commands.extend(
        panes
            .into_iter()
            .enumerate()
            .map(|(ix, label)| window(label, Box::new(ActivatePane(ix)))),
    );
    commands.extend([
        window("agmen: open settings", Box::new(OpenSettings)),
        // Zed's palette lists its own toggle too.
        window("command palette: toggle", Box::new(ToggleCommandPalette)),
        window("workspace: toggle vim mode", Box::new(ToggleVimMode)),
        Command {
            label: "agmen: quit".into(),
            action: Box::new(Quit),
            scope: Scope::App,
        },
    ]);
    commands
}

/// The glyphs a key hint shows for `keys` (written as Zed's keymap writes
/// them, such as `cmd-shift-p`), as Zed draws them on a Mac: modifiers in
/// the order ⌃ ⌥ ⌘ ⇧ with no separator, then the key, capitalized; chords
/// apart by a space. Arrows, enter, escape, tab and backspace are symbols.
#[must_use]
pub(crate) fn glyphs(keys: &str) -> String {
    keys.split(' ')
        .map(stroke_glyphs)
        .collect::<Vec<_>>()
        .join(" ")
}

fn stroke_glyphs(stroke: &str) -> String {
    // The key is the last part; "cmd--" would name the minus key.
    let (mods, key) = match stroke.strip_suffix("--") {
        Some(mods) => (mods, "-"),
        None => stroke.rsplit_once('-').unwrap_or(("", stroke)),
    };
    let has = |name: &str| mods.split('-').any(|m| m == name);
    let mut out = String::new();
    for (names, glyph) in [
        (&["ctrl"][..], '\u{2303}'),
        (&["alt"], '\u{2325}'),
        (&["cmd", "super", "win"], '\u{2318}'),
        (&["shift"], '\u{21e7}'),
    ] {
        if names.iter().any(|n| has(n)) {
            out.push(glyph);
        }
    }
    let symbol = match key {
        "up" => Some('\u{2191}'),
        "down" => Some('\u{2193}'),
        "left" => Some('\u{2190}'),
        "right" => Some('\u{2192}'),
        "enter" => Some('\u{23ce}'),
        "escape" => Some('\u{238b}'),
        "tab" => Some('\u{21e5}'),
        "backspace" => Some('\u{232b}'),
        _ => None,
    };
    match symbol {
        Some(c) => out.push(c),
        None => {
            let mut chars = key.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        }
    }
    out
}

/// The hint for `action`: the glyphs of its first binding in `table`.
#[must_use]
pub(crate) fn hint(table: &[Binding], action: &dyn Action) -> Option<String> {
    table
        .iter()
        .find(|b| b.key_binding.action().partial_eq(action))
        .map(|b| glyphs(b.keys))
}

/// The text input's bindings alone, for tests of views without a palette.
#[cfg(test)]
#[must_use]
pub(crate) fn text_input_bindings() -> Vec<KeyBinding> {
    bindings_in(TEXT_INPUT_CONTEXT)
}

/// The bindings for one key context, for tests of a view without the rest.
#[cfg(test)]
#[must_use]
pub(crate) fn bindings_in(context: &str) -> Vec<KeyBinding> {
    table()
        .into_iter()
        .filter(|b| b.context == Some(context))
        .map(|b| b.key_binding)
        .collect()
}

/// Registers every binding with the app.
pub(crate) fn bind_all(cx: &mut App) {
    cx.bind_keys(table().into_iter().map(|b| b.key_binding));
}

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod tests;
