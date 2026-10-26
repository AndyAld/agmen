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
    /// When no installed extension defines it.
    pub(crate) fn load(extensions_dir: &Path, name: &str) -> Result<Self, AppearanceError> {
        let mut roots: Vec<PathBuf> = fs::read_dir(extensions_dir)
            .map(|dir| dir.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        roots.sort();
        for root in roots {
            let Ok(files) = fs::read_dir(root.join("icon_themes")) else {
                continue;
            };
            let mut files: Vec<PathBuf> = files.flatten().map(|e| e.path()).collect();
            files.sort();
            for file in files
                .iter()
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
            {
                // A theme file that does not parse is skipped, not fatal.
                let Ok(family) = read_jsonc::<Family>(file) else {
                    continue;
                };
                if let Some(theme) = family.themes.into_iter().find(|t| t.name == name) {
                    return Ok(Self { root, theme });
                }
            }
        }
        Err(AppearanceError::ThemeNotFound(name.to_owned()))
    }

    /// The icon for a file named `name`: the exact name, then the longest
    /// suffix (`tar.gz` before `gz`), then the theme's default, the order
    /// Zed uses. `None` when the mapped SVG is missing or outside the theme.
    pub(crate) fn file(&self, name: &str) -> Option<PathBuf> {
        let by_stem = self.theme.file_stems.get(name);
        let by_suffix = || {
            name.match_indices('.')
                .map(|(i, _)| &name[i + 1..])
                .find_map(|suffix| self.theme.file_suffixes.get(suffix))
        };
        let key = by_stem.or_else(by_suffix);
        key.and_then(|k| self.icon(k))
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
