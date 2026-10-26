//! A Zed icon theme (schema v0.3.0), read from an installed extension
//! through its own mapping of file names, suffixes and folders. agmen bundles
//! none of its icons.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::appearance::{AppearanceError, read_jsonc};

#[derive(Debug, Clone, Default, Deserialize)]
struct FolderIcons {
    collapsed: Option<String>,
    expanded: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FileIconEntry {
    path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ThemeEntry {
    name: String,
    #[serde(default)]
    directory_icons: FolderIcons,
    #[serde(default)]
    named_directory_icons: HashMap<String, FolderIcons>,
    #[serde(default)]
    file_stems: HashMap<String, String>,
    #[serde(default)]
    file_suffixes: HashMap<String, String>,
    #[serde(default)]
    file_icons: HashMap<String, FileIconEntry>,
}

#[derive(Deserialize)]
struct Family {
    themes: Vec<ThemeEntry>,
}

/// One icon theme from one installed extension.
#[derive(Debug, Clone)]
pub(crate) struct IconTheme {
    root: PathBuf,
    theme: ThemeEntry,
}

impl IconTheme {
    /// Finds the icon theme `name` in `extensions_dir/*/icon_themes/*.json`.
    ///
    /// # Errors
    ///
    /// When no installed extension defines it; the error names the folders
    /// and files that could not be read on the way.
    pub(crate) fn load(extensions_dir: &Path, name: &str) -> Result<Self, AppearanceError> {
        let mut unreadable = Vec::new();
        let mut roots = match fs::read_dir(extensions_dir) {
            Ok(dir) => entries(dir, extensions_dir, &mut unreadable),
            Err(err) => {
                return Err(AppearanceError::Io {
                    path: extensions_dir.to_owned(),
                    source: err,
                });
            }
        };
        roots.sort();
        for root in roots {
            let themes_dir = root.join("icon_themes");
            let Ok(dir) = fs::read_dir(&themes_dir) else {
                // Most extensions ship no icon themes; that is not an error.
                continue;
            };
            let mut files = entries(dir, &themes_dir, &mut unreadable);
            files.sort();
            for file in files
                .iter()
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
            {
                let Ok(family) = read_jsonc::<Family>(file) else {
                    unreadable.push(file.clone());
                    continue;
                };
                if let Some(theme) = family.themes.into_iter().find(|t| t.name == name) {
                    let root = root.canonicalize().map_err(|source| AppearanceError::Io {
                        path: root.clone(),
                        source,
                    })?;
                    return Ok(Self { root, theme });
                }
            }
        }
        if unreadable.is_empty() {
            Err(AppearanceError::ThemeNotFound(name.to_owned()))
        } else {
            Err(AppearanceError::ThemeNotFoundSkipped {
                name: name.to_owned(),
                skipped: unreadable,
            })
        }
    }

    /// The icon for a file named `name`, in Zed's order: the whole name, then
    /// each suffix after a dot (longest first), each looked up in the stems
    /// and then the suffixes; then the part before the first dot; then the
    /// theme's `default`. A mapping whose SVG is missing, or lies outside the
    /// extension, counts as no match, so the search goes on.
    pub(crate) fn file(&self, name: &str) -> Option<PathBuf> {
        let lookup = |key: &str| {
            self.theme
                .file_stems
                .get(key)
                .or_else(|| self.theme.file_suffixes.get(key))
                .and_then(|typ| self.icon(typ))
        };
        let suffixes = name.match_indices('.').map(|(i, _)| &name[i + 1..]);
        let first_prefix = name.split_once('.').map(|(prefix, _)| prefix);
        std::iter::once(name)
            .chain(suffixes)
            .chain(first_prefix)
            .filter(|key| !key.is_empty())
            .find_map(lookup)
            .or_else(|| self.icon("default"))
    }

    /// The icon for a folder named `name`, open or closed.
    pub(crate) fn folder(&self, name: &str, open: bool) -> Option<PathBuf> {
        let pick = |icons: &FolderIcons| {
            if open {
                icons.expanded.clone()
            } else {
                icons.collapsed.clone()
            }
        };
        self.theme
            .named_directory_icons
            .get(name)
            .and_then(pick)
            .and_then(|rel| self.resolve(&rel))
            .or_else(|| pick(&self.theme.directory_icons).and_then(|rel| self.resolve(&rel)))
    }

    fn icon(&self, key: &str) -> Option<PathBuf> {
        self.theme
            .file_icons
            .get(key)
            .and_then(|entry| self.resolve(&entry.path))
    }

    /// The SVG at `rel` under the extension, if it exists there; a path that
    /// climbs out of the extension (`../`) or a link that lands outside is
    /// refused.
    fn resolve(&self, rel: &str) -> Option<PathBuf> {
        let root = self.root.canonicalize().ok()?;
        let path = root
            .join(rel.trim_start_matches("./"))
            .canonicalize()
            .ok()?;
        (path.starts_with(&root) && path.is_file()).then_some(path)
    }
}

/// The paths in `dir`, recording entries that cannot be read under `at`.
fn entries(dir: fs::ReadDir, at: &Path, unreadable: &mut Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in dir {
        match entry {
            Ok(entry) => out.push(entry.path()),
            Err(_) => unreadable.push(at.to_owned()),
        }
    }
    out
}

#[cfg(test)]
#[path = "zed_theme_tests.rs"]
mod tests;
