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
/// Task 8: with the project comes the checkout root when the folder named
/// it, which agmend reads worktrees and git facts from; a project from the
/// settings has none.
pub(crate) fn choose_project(
    cwd: &Path,
    settings: Option<&Path>,
) -> (ProjectChoice, Option<PathBuf>) {
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
    (from_settings(settings), None)
}

fn from_settings(settings: Option<&Path>) -> ProjectChoice {
    let Some(settings) = settings else {
        return ProjectChoice::None { why: None };
    };
    match DesktopSettings::load(settings) {
        Ok(DesktopSettings {
            last_project: Some(project),
            ..
        }) => ProjectChoice::Project(project),
        Ok(DesktopSettings {
            last_project: None, ..
        }) => ProjectChoice::None { why: None },
        Err(err) => ProjectChoice::None {
            why: Some(err.to_string()),
        },
    }
}

#[cfg(test)]
#[path = "project_tests.rs"]
mod tests;
