//! Draws the file icons a window opens on in its first frame.
//!
//! `img()` loads a theme SVG on the background executor and repaints the
//! view when it arrives. macOS stops a window's display link while AppKit
//! reports the window occluded (covered, on another Space, or behind the
//! lock screen), so a window that opens there keeps its first frames and
//! icons that load afterwards never draw: `--screen` shots taken that way
//! had empty icon slots in the trees and tabs. Rendering the snapshot's
//! icons before the window opens puts them in the first frame; any other
//! icon still loads in the background.

use std::collections::BTreeSet;
use std::path::Path;

use gpui::{App, SvgRenderer};

use super::FileIcons;
use crate::backend::Snapshot;
use crate::ui::ICON_PREVIEW_NAMES;

/// The most theme SVGs rendered ahead: about 0.4 ms each in a release
/// build and 10 ms in a debug build (Material icons, 2026-10-26).
const MAX_PRELOADED: usize = 128;

/// The most names [`snapshot_entries`] collects.
const MAX_ENTRIES: usize = 2048;

/// The settings file, which Settings opens in a tab.
const SETTINGS_FILE: &str = "settings.toml";

impl FileIcons {
    /// Renders now the theme SVG of each entry, a file or folder name and
    /// whether it is a folder (folders both open and closed), so views draw
    /// them without waiting. Does nothing without a Zed icon theme.
    pub(crate) fn preload<'a>(
        &self,
        entries: impl IntoIterator<Item = (&'a str, bool)>,
        renderer: &SvgRenderer,
    ) {
        self.preload_up_to(entries, renderer, MAX_PRELOADED);
    }

    pub(super) fn preload_up_to<'a>(
        &self,
        entries: impl IntoIterator<Item = (&'a str, bool)>,
        renderer: &SvgRenderer,
        limit: usize,
    ) {
        let Some(theme) = &self.theme else {
            return;
        };
        let mut rendered = self.rendered.borrow_mut();
        for (name, is_dir) in entries {
            let svgs = if is_dir {
                [theme.folder(name, false), theme.folder(name, true)]
            } else {
                [theme.file(name), None]
            };
            for svg in svgs.into_iter().flatten() {
                if rendered.len() >= limit {
                    return;
                }
                if rendered.contains_key(&svg) {
                    continue;
                }
                // An SVG that cannot be read or drawn here fails the same
                // way in `img()`, which draws nothing; leave it to that path.
                let image = std::fs::read(&svg)
                    .ok()
                    .and_then(|bytes| renderer.render_single_frame(&bytes, 1.0).ok());
                if let Some(image) = image {
                    rendered.insert(svg, image);
                }
            }
        }
    }
}

/// Renders the icons of every name in `snapshot` ahead, through the
/// [`FileIcons`] global; call it before the window opens.
pub(crate) fn preload_snapshot(snapshot: &Snapshot, cx: &App) {
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
pub(crate) fn snapshot_entries(snapshot: &Snapshot) -> Vec<(String, bool)> {
    let mut names = BTreeSet::new();
    let mut add = |name: &str, is_dir: bool| {
        if names.len() < MAX_ENTRIES {
            names.insert((name.to_owned(), is_dir));
        }
    };
    add("", true);
    add("", false);
    add(SETTINGS_FILE, false);
    for (name, is_dir) in ICON_PREVIEW_NAMES {
        add(name, is_dir);
    }
    for role in &snapshot.roles {
        add(&format!("{}.xml", role.name), false);
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
#[path = "preload_tests.rs"]
mod tests;
