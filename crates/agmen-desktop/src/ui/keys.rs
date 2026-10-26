//! The workspace's actions and their key bindings, in one place.
//!
//! Bindings are Zed's wherever agmen has the same action (finding V86, from
//! Zed's `assets/keymaps/default-macos.json` at 08cbb88). Task 18's
//! `keymap.rs` absorbs this list when it lists every binding for the
//! palette. The palette (cmd-shift-p), search (cmd-shift-f) and the
//! terminal (ctrl-backtick) belong to Tasks 18 and 14 and are not bound
//! here.

use gpui::{Action, App, KeyBinding, actions};

use super::left_dock::LeftDockTab;
use super::workspace::{ToggleLeftDock, ToggleRightDock};

actions!(
    workspace,
    [
        /// Shows the Project panel, or hides the dock when it already shows
        /// it (Zed's `project_panel::ToggleFocus`).
        ToggleProjectPanel,
        /// Shows the Git panel, or hides the dock when it already shows it
        /// (Zed's `git_panel::ToggleFocus`).
        ToggleGitPanel,
        /// Opens the Search tab in the active pane.
        OpenSearch,
        /// Opens the Settings tab in the active pane (Zed's
        /// `zed::OpenSettings`).
        OpenSettings,
        /// Closes the active pane's active tab (Zed's `pane::CloseActiveItem`).
        CloseActiveTab,
        /// Splits the active pane, opening its active tab on the right
        /// (Zed's `pane::SplitRight`).
        SplitRight,
    ]
);

/// Activates the pane at this index, counted from 0; past the last pane it
/// does nothing (Zed's `workspace::ActivatePane`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct ActivatePane(pub(crate) usize);

/// Shows a left dock tab, opening the dock: a click on the tab strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct ShowLeftDockTab(pub(crate) LeftDockTab);

/// The key context the workspace's root sets.
const CONTEXT: &str = "Workspace";

/// The workspace's key bindings.
pub(crate) fn bindings() -> Vec<KeyBinding> {
    let ctx = Some(CONTEXT);
    vec![
        KeyBinding::new("cmd-b", ToggleLeftDock, ctx),
        KeyBinding::new("cmd-r", ToggleRightDock, ctx),
        KeyBinding::new("cmd-alt-b", ToggleRightDock, ctx),
        KeyBinding::new("cmd-shift-e", ToggleProjectPanel, ctx),
        KeyBinding::new("ctrl-shift-g", ToggleGitPanel, ctx),
        KeyBinding::new("cmd-,", OpenSettings, ctx),
        KeyBinding::new("cmd-1", ActivatePane(0), ctx),
        KeyBinding::new("cmd-2", ActivatePane(1), ctx),
        KeyBinding::new("cmd-3", ActivatePane(2), ctx),
        KeyBinding::new("cmd-4", ActivatePane(3), ctx),
        KeyBinding::new("cmd-5", ActivatePane(4), ctx),
        KeyBinding::new("cmd-6", ActivatePane(5), ctx),
        KeyBinding::new("cmd-7", ActivatePane(6), ctx),
        KeyBinding::new("cmd-8", ActivatePane(7), ctx),
        KeyBinding::new("cmd-9", ActivatePane(8), ctx),
        KeyBinding::new("cmd-w", CloseActiveTab, ctx),
        KeyBinding::new("cmd-\\", SplitRight, ctx),
    ]
}

/// Registers [`bindings`] with the app.
pub(crate) fn bind_workspace_keys(cx: &mut App) {
    cx.bind_keys(bindings());
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui::Keystroke;

    use super::*;

    #[test]
    fn bindings_do_not_collide() {
        let bindings = bindings();
        let keys: HashSet<_> = bindings.iter().map(|b| b.keystrokes().to_vec()).collect();
        assert_eq!(keys.len(), bindings.len(), "a key is bound twice");
    }

    #[test]
    fn other_tasks_keys_stay_unbound() {
        let bindings = bindings();
        for key in ["cmd-shift-p", "cmd-shift-f", "ctrl-`", "cmd-q"] {
            let typed = [Keystroke::parse(key).unwrap()];
            assert!(
                bindings
                    .iter()
                    .all(|b| b.match_keystrokes(&typed).is_none()),
                "{key} is bound here"
            );
        }
    }

    #[test]
    fn cmd_comma_opens_settings() {
        // V86: Zed binds cmd-comma to zed::OpenSettings.
        let typed = [Keystroke::parse("cmd-,").unwrap()];
        let bound: Vec<_> = bindings()
            .into_iter()
            .filter(|b| b.match_keystrokes(&typed) == Some(false))
            .collect();
        assert_eq!(bound.len(), 1);
        assert!(bound[0].action().partial_eq(&OpenSettings));
    }
}
