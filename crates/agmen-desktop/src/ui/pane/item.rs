//! What a center tab shows.

use std::path::PathBuf;

use agmen_proto::IdentityId;

/// A Settings page. Task 17 builds Appearance; the other pages follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum SettingsPage {
    #[default]
    Appearance,
}

/// What a tab shows. Items whose view is not built yet show a placeholder
/// until their task (Tasks 10 to 12, 17 and 19).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Tasks 10 to 19 open file, diff, role, plan, spec and settings tabs"
    )
)]
pub(crate) enum TabItem {
    Chat,
    /// An agent's session view.
    Agent(IdentityId),
    File {
        worktree: String,
        path: String,
    },
    Diff {
        worktree: String,
        path: String,
    },
    /// A role's XML file, by role name.
    RoleFile(String),
    Plan,
    /// A spec file, by path.
    Spec(String),
    Settings(SettingsPage),
    /// The settings file Settings writes, shown read only.
    SettingsFile(PathBuf),
    // shortcut: a placeholder tab the status bar's search button opens;
    // Task 18 builds search (cmd-shift-f) and its view.
    Search,
}

impl SettingsPage {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
        }
    }
}
