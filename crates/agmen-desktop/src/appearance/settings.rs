//! agmen's own appearance settings in `~/.config/agmen/settings.toml`, which
//! the terminal UI reads too (spec 9.4).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::zed::Mode;
use super::{AppearanceError, FontPrefs};

/// Where file and folder icons come from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "name")]
pub(crate) enum IconThemeChoice {
    /// An icon theme installed in Zed, by name.
    Zed(String),
    /// agmen's own Lucide icons.
    Lucide,
    /// The operating system's file icons.
    #[default]
    System,
}

/// Spacing between chat messages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Density {
    #[default]
    Comfortable,
    Compact,
}

/// The `[appearance]` table of agmen's settings file. Other tables (the
/// terminal UI's, say) and keys this version does not know are kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AgmenSettings {
    /// Read theme, icon theme and fonts from Zed's settings.
    pub(crate) follow_zed: bool,
    pub(crate) theme: Option<String>,
    // shortcut: mode picks which half (light or dark) the theme picker lists; it does not switch
    // the theme when the OS changes mode. Add light and dark theme keys if people ask for that.
    pub(crate) mode: Mode,
    pub(crate) icon_theme: IconThemeChoice,
    pub(crate) fonts: FontPrefs,
    pub(crate) density: Density,
}

impl Default for AgmenSettings {
    fn default() -> Self {
        Self {
            follow_zed: true,
            theme: None,
            mode: Mode::System,
            icon_theme: IconThemeChoice::System,
            fonts: FontPrefs::default(),
            density: Density::Comfortable,
        }
    }
}

/// The table that holds these settings.
const TABLE: &str = "appearance";
/// The `[appearance]` keys this version writes.
pub(super) const OWNED_KEYS: &[&str] = &[
    "follow_zed",
    "theme",
    "mode",
    "icon_theme",
    "fonts",
    "density",
];
/// The `[appearance.fonts]` keys this version writes.
pub(super) const OWNED_FONT_KEYS: &[&str] = &[
    "ui_family",
    "ui_size",
    "code_family",
    "code_size",
    "code_ligatures",
    "terminal_family",
    "terminal_size",
];
/// A settings file is small; anything larger is refused.
const MAX_SETTINGS_BYTES: u64 = 4 * 1024 * 1024;

impl AgmenSettings {
    /// Reads the `[appearance]` table of `path`; a missing file or table
    /// gives the defaults.
    ///
    /// # Errors
    ///
    /// When the file exists but is too large, unreadable, or not valid.
    pub(crate) fn load(path: &Path) -> Result<Self, AppearanceError> {
        let Some(mut table) = read_settings_table(path)? else {
            return Ok(Self::default());
        };
        let Some(value) = table.remove(TABLE) else {
            return Ok(Self::default());
        };
        value
            .try_into()
            .map_err(|err: toml::de::Error| AppearanceError::Parse {
                path: path.to_owned(),
                message: err.to_string(),
            })
    }

    /// Replaces the `[appearance]` table in `path`, keeping everything else,
    /// through a temporary file and a rename so a crash never leaves half a
    /// file.
    ///
    /// # Errors
    ///
    /// When the existing file cannot be parsed (it is left alone), or the
    /// folder or file cannot be written.
    pub(crate) fn save(&self, path: &Path) -> Result<(), AppearanceError> {
        let mut table = read_settings_table(path)?.unwrap_or_default();
        let value = toml::Value::try_from(self).map_err(|err| AppearanceError::Serialize {
            path: path.to_owned(),
            message: err.to_string(),
        })?;
        // Merge into the table already there: keys this version does not
        // know stay; keys it owns are replaced, or dropped when now unset.
        let mut merged = match table.remove(TABLE) {
            Some(toml::Value::Table(existing)) => existing,
            _ => toml::Table::new(),
        };
        // Font keys a newer version adds survive too (C110).
        let mut fonts = match merged.remove("fonts") {
            Some(toml::Value::Table(existing)) => existing,
            _ => toml::Table::new(),
        };
        for key in OWNED_KEYS {
            merged.remove(*key);
        }
        for key in OWNED_FONT_KEYS {
            fonts.remove(*key);
        }
        if let toml::Value::Table(mut ours) = value {
            if let Some(toml::Value::Table(our_fonts)) = ours.remove("fonts") {
                fonts.extend(our_fonts);
            }
            merged.extend(ours);
        }
        if !fonts.is_empty() {
            merged.insert("fonts".to_owned(), toml::Value::Table(fonts));
        }
        table.insert(TABLE.to_owned(), toml::Value::Table(merged));
        write_settings_table(path, &table)
    }
}

/// Writes `table` as the whole settings file at `path`, creating its
/// folder, through a temporary file and a rename so a crash never leaves
/// half a file.
///
/// # Errors
///
/// When the table cannot be written as TOML, or the folder or file cannot
/// be written.
pub(crate) fn write_settings_table(
    path: &Path,
    table: &toml::Table,
) -> Result<(), AppearanceError> {
    let io = |source| AppearanceError::Io {
        path: path.to_owned(),
        source,
    };
    let text = toml::to_string_pretty(table).map_err(|err| AppearanceError::Serialize {
        path: path.to_owned(),
        message: err.to_string(),
    })?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).map_err(io)?;
    fs::rename(&tmp, path).map_err(io)
}

/// The whole settings file at `path` as a table, `None` when it does not
/// exist.
///
/// # Errors
///
/// When the file is too large, unreadable, or not valid TOML.
pub(crate) fn read_settings_table(path: &Path) -> Result<Option<toml::Table>, AppearanceError> {
    let io = |source| AppearanceError::Io {
        path: path.to_owned(),
        source,
    };
    let len = match fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io(err)),
    };
    if len > MAX_SETTINGS_BYTES {
        return Err(AppearanceError::TooLarge {
            path: path.to_owned(),
            len,
        });
    }
    let text = fs::read_to_string(path).map_err(io)?;
    text.parse::<toml::Table>()
        .map(Some)
        .map_err(|err| AppearanceError::Parse {
            path: path.to_owned(),
            message: err.to_string(),
        })
}
