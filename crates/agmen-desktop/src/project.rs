//! Which project a live launch opens (Task 7).

use std::path::{Path, PathBuf};

use agmen_proto::ProjectId;

use crate::appearance::DesktopSettings;

/// The project a live window opens, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectChoice {
    Project(ProjectId),
    /// Neither the folder nor the settings name one; `why` is set when
    /// one could not be read.
    None {
        why: Option<String>,
    },
}

/// The project named by `cwd`'s `.agmen/identity.json` (the trusted file
/// the CLI uses, agmen-client's `find_identity`), else the last project in
/// `settings`, the desktop's settings file. An identity file agmen may not
/// use gives `None` with its reason rather than some other project, so the
/// person sees why their folder did not open.
///
/// The checkout root lets agmend read worktrees and git facts. Saved
/// projects recover it from their matching recent folder when possible.
pub(crate) fn choose_project(
    cwd: &Path,
    settings: Option<&Path>,
) -> (ProjectChoice, Option<PathBuf>) {
    let choice = choose_folder(cwd);
    if choice.0 == (ProjectChoice::None { why: None }) {
        choose_saved_project(settings)
    } else {
        choice
    }
}

/// An explicit folder never falls back to a different project from settings.
/// The CLI's finder resolves its checkout root and checks its joined identity.
pub(crate) fn choose_folder(cwd: &Path) -> (ProjectChoice, Option<PathBuf>) {
    match agmen_client::find_identity(cwd) {
        Ok(Some(joined)) => {
            return (
                ProjectChoice::Project(joined.identity.project),
                Some(joined.root),
            );
        }
        Ok(None) => {}
        Err(err) => {
            return (
                ProjectChoice::None {
                    why: Some(err.to_string()),
                },
                None,
            );
        }
    }
    (ProjectChoice::None { why: None }, None)
}

/// Restores the last project and its checkout, else the newest joined recent.
pub(crate) fn choose_saved_project(settings: Option<&Path>) -> (ProjectChoice, Option<PathBuf>) {
    let Some(settings) = settings else {
        return (ProjectChoice::None { why: None }, None);
    };
    let stored = match DesktopSettings::load(settings) {
        Ok(stored) => stored,
        Err(err) => {
            return (
                ProjectChoice::None {
                    why: Some(err.to_string()),
                },
                None,
            );
        }
    };
    for folder in &stored.recent_projects {
        let (choice, root) = choose_project(folder, None);
        if let ProjectChoice::Project(project) = &choice
            && stored
                .last_project
                .as_ref()
                .is_none_or(|last| last == project)
        {
            return (choice, root);
        }
    }
    (
        stored
            .last_project
            .map_or(ProjectChoice::None { why: None }, ProjectChoice::Project),
        None,
    )
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
