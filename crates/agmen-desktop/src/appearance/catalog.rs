//! The themes the picker lists: built in, installed in Zed, installed by
//! agmen from Zed's extensions, then what the API offers.

use std::path::Path;

use super::fallback::BUILTIN_THEME;
use super::marked::MARKED_THEME;
use super::marketplace::{InstalledTheme, ThemeExtension, read_installed};
use super::theme::{Family, ThemeDirs, theme_files};
use super::zed::read_jsonc;
use super::{Skipped, SystemMode};

/// Where an installed theme comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThemeOrigin {
    /// Built into agmen: the Gruvbox fallback and Marked's palette.
    BuiltIn,
    /// The person's own `~/.config/zed/themes/`.
    ZedUser,
    /// A Zed extension, by id.
    ZedExtension(String),
    /// Installed by agmen from Zed's extensions.
    Agmen(InstalledTheme),
}

/// One theme the picker can choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThemeEntry {
    pub(crate) name: String,
    /// `None` when the theme file does not say.
    pub(crate) appearance: Option<SystemMode>,
    /// The theme family's `author`.
    pub(crate) author: Option<String>,
    pub(crate) origin: ThemeOrigin,
}

/// Every installed theme, built-ins first, then in the order the loader
/// searches, each name once (the first wins, as when loading). The second
/// list names files and folders that could not be read.
#[must_use]
pub(crate) fn installed_themes(dirs: &ThemeDirs) -> (Vec<ThemeEntry>, Vec<Skipped>) {
    let mut out = vec![
        ThemeEntry {
            name: BUILTIN_THEME.to_owned(),
            appearance: Some(SystemMode::Dark),
            author: None,
            origin: ThemeOrigin::BuiltIn,
        },
        ThemeEntry {
            name: MARKED_THEME.to_owned(),
            appearance: Some(SystemMode::Dark),
            author: None,
            origin: ThemeOrigin::BuiltIn,
        },
    ];
    let (files, mut skipped) = theme_files(dirs);
    for path in files {
        let family = match read_jsonc::<Family>(&path) {
            Ok(family) => family,
            Err(err) => {
                skipped.push(Skipped::new(&path, err));
                continue;
            }
        };
        let origin = origin_of(&path, dirs);
        for theme in family.themes {
            if out.iter().any(|entry| entry.name == theme.name) {
                continue;
            }
            out.push(ThemeEntry {
                appearance: match theme.appearance.as_deref() {
                    Some("light") => Some(SystemMode::Light),
                    Some("dark") => Some(SystemMode::Dark),
                    _ => None,
                },
                name: theme.name,
                author: family.author.clone(),
                origin: origin.clone(),
            });
        }
    }
    (out, skipped)
}

/// Which folder a theme file was found in.
fn origin_of(path: &Path, dirs: &ThemeDirs) -> ThemeOrigin {
    let top = |root: &Path| {
        path.strip_prefix(root)
            .ok()
            .and_then(|rest| rest.components().next())
            .map(|c| c.as_os_str().to_owned())
    };
    if let Some(id) = top(&dirs.agmen)
        && let Some(installed) = read_installed(&dirs.agmen.join(&id))
    {
        return ThemeOrigin::Agmen(installed);
    }
    if let Some(id) = top(&dirs.extensions) {
        return ThemeOrigin::ZedExtension(id.to_string_lossy().into_owned());
    }
    ThemeOrigin::ZedUser
}

/// One row of the "Browse Zed themes" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PickerEntry {
    Installed(ThemeEntry),
    Marketplace(ThemeExtension),
}

/// The browse list: every installed theme first, then the API's themes that
/// are not installed already (in Zed or by agmen), in the API's order.
#[must_use]
pub(crate) fn picker_entries(
    installed: &[ThemeEntry],
    marketplace: &[ThemeExtension],
) -> Vec<PickerEntry> {
    let installed_id = |id: &str| {
        installed.iter().any(|entry| match &entry.origin {
            ThemeOrigin::ZedExtension(ext) => ext == id,
            ThemeOrigin::Agmen(theme) => theme.id == id,
            ThemeOrigin::BuiltIn | ThemeOrigin::ZedUser => false,
        })
    };
    installed
        .iter()
        .cloned()
        .map(PickerEntry::Installed)
        .chain(
            marketplace
                .iter()
                .filter(|ext| !installed_id(&ext.id))
                .cloned()
                .map(PickerEntry::Marketplace),
        )
        .collect()
}
