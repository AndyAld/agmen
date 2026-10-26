//! The file icons a workspace's first frame draws, rendered ahead so the
//! frame has them (`icons/preload.rs` says why).

use std::collections::BTreeSet;
use std::path::Path;

use gpui::App;

use super::roles_panel::role_file_name;
use super::settings_view::ICON_PREVIEW_NAMES;
use crate::appearance::SETTINGS_FILE_NAME;
use crate::backend::Snapshot;
use crate::icons::FileIcons;

/// The most names [`snapshot_entries`] collects.
const MAX_ENTRIES: usize = 2048;

/// Renders the icons of every name in `snapshot` ahead, through the
/// [`FileIcons`] global. `Workspace::new` calls it, before the window's
/// first frame.
pub(super) fn preload_snapshot(snapshot: &Snapshot, cx: &App) {
    let Some(icons) = cx.try_global::<FileIcons>() else {
        return;
    };
    let entries = snapshot_entries(snapshot);
    let renderer = cx.svg_renderer();
    icons.preload(
        entries
            .iter()
            .map(|(name, is_dir)| (name.as_str(), *is_dir)),
        &renderer,
    );
}

/// Every file and folder name the snapshot's views ask icons for, each once:
/// the worktrees' files and folders (the Project tree and the Git panel's
/// changes), role files (the Roles tab and role file tabs), the settings
/// file, Settings' icon preview, and the theme's default file and folder
/// (an empty name), which unmapped names such as a tree's root fall to.
fn snapshot_entries(snapshot: &Snapshot) -> Vec<(String, bool)> {
    let mut names = BTreeSet::new();
    let mut add = |name: &str, is_dir: bool| {
        if names.len() < MAX_ENTRIES {
            names.insert((name.to_owned(), is_dir));
        }
    };
    add("", true);
    add("", false);
    add(SETTINGS_FILE_NAME, false);
    for (name, is_dir) in ICON_PREVIEW_NAMES {
        add(name, is_dir);
    }
    for role in &snapshot.roles {
        add(&role_file_name(&role.name), false);
    }
    for worktree in &snapshot.worktrees {
        let changed = worktree.changes.iter().map(|change| &change.path);
        for path in worktree.files.iter().chain(changed) {
            let path = Path::new(path);
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                add(name, false);
            }
            for folder in path.ancestors().skip(1) {
                if let Some(name) = folder.file_name().and_then(|n| n.to_str()) {
                    add(name, true);
                }
            }
        }
    }
    names.into_iter().collect()
}

#[cfg(test)]
#[path = "icon_preload_tests.rs"]
mod tests;
