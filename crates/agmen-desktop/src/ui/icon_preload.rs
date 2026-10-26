//! The file icons a workspace's first frame draws, rendered ahead so the
//! frame has them (`icons/preload.rs` says why).

use std::collections::HashSet;
use std::path::{Component, Path};

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
/// the theme's default file and folder (an empty name), which unmapped
/// names such as a tree's root fall to, the settings file, Settings' icon
/// preview, role files (the Roles tab and role file tabs), then the
/// worktrees' files and folders (the Project tree and the Git panel's
/// changes), shallowest first, so a preload limit leaves out the deepest.
fn snapshot_entries(snapshot: &Snapshot) -> Vec<(String, bool)> {
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    let mut add = |name: &str, is_dir: bool| {
        if names.len() < MAX_ENTRIES && seen.insert((name.to_owned(), is_dir)) {
            names.push((name.to_owned(), is_dir));
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
    // Each path's parts with their depth; a part before the last is a
    // folder.
    let mut parts: Vec<(usize, &str, bool)> = Vec::new();
    for worktree in &snapshot.worktrees {
        let changed = worktree.changes.iter().map(|change| &change.path);
        for path in worktree.files.iter().chain(changed) {
            let normal: Vec<&str> = Path::new(path)
                .components()
                .filter_map(|part| match part {
                    Component::Normal(name) => name.to_str(),
                    _ => None,
                })
                .collect();
            let last = normal.len().saturating_sub(1);
            parts.extend(
                normal
                    .into_iter()
                    .enumerate()
                    .map(|(depth, name)| (depth, name, depth < last)),
            );
        }
    }
    parts.sort_by_key(|(depth, _, _)| *depth);
    for (_, name, is_dir) in parts {
        add(name, is_dir);
    }
    names
}

#[cfg(test)]
#[path = "icon_preload_tests.rs"]
mod tests;
