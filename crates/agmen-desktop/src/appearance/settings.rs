//! agmen's own appearance settings in `~/.config/agmen/settings.toml`, which
//! the terminal UI reads too (spec 9.4).

use std::fs;
use std::path::Path;

use agmen_proto::ProjectId;
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

// Task 7: what the desktop app remembers between launches.
/// The `[desktop]` table of agmen's settings file: what the app keeps
/// between launches, apart from how it looks. Keys this version does not
/// know are kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct DesktopSettings {
    /// The project the last live launch opened, which a launch outside a
    /// joined folder opens again.
    pub(crate) last_project: Option<ProjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) quit_keep_running: Option<bool>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) start_at_login: bool,
}

/// The table that holds [`DesktopSettings`].
const DESKTOP_TABLE: &str = "desktop";
/// The `[desktop]` keys this version writes.
const DESKTOP_KEYS: &[&str] = &["last_project", "quit_keep_running", "start_at_login"];

impl DesktopSettings {
    /// Reads the `[desktop]` table of `path`; a missing file or table gives
    /// the defaults.
    ///
    /// # Errors
    ///
    /// When the file exists but is too large, unreadable, or not valid,
    /// such as a `last_project` that is not a project id.
    pub(crate) fn load(path: &Path) -> Result<Self, AppearanceError> {
        let Some(mut table) = read_settings_table(path)? else {
            return Ok(Self::default());
        };
        let Some(value) = table.remove(DESKTOP_TABLE) else {
            return Ok(Self::default());
        };
        value
            .try_into()
            .map_err(|err: toml::de::Error| AppearanceError::Parse {
                path: path.to_owned(),
                message: err.to_string(),
            })
    }

    /// Replaces the `[desktop]` keys this version owns in `path`, keeping
    /// every other key and table. Settings equal to the file's leave it
    /// untouched; a change edits the file in place, so its comments and
    /// key order stay (final review fix 4).
    ///
    /// # Errors
    ///
    /// When the existing file cannot be parsed (it is left alone), or the
    /// folder or file cannot be written.
    pub(crate) fn save(&self, path: &Path) -> Result<(), AppearanceError> {
        // Checks the size cap and the TOML, and gives what the file holds.
        let existing = read_settings_table(path)?;
        let stored = match existing.and_then(|mut table| table.remove(DESKTOP_TABLE)) {
            Some(value) => value.try_into().ok(),
            None => Some(Self::default()),
        };
        if stored.as_ref() == Some(self) {
            return Ok(());
        }
        let parse = |message: String| AppearanceError::Parse {
            path: path.to_owned(),
            message,
        };
        let serialize = |message: String| AppearanceError::Serialize {
            path: path.to_owned(),
            message,
        };
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => {
                return Err(AppearanceError::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        let mut doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|err| parse(err.to_string()))?;
        let ours = match toml::Value::try_from(self).map_err(|err| serialize(err.to_string()))? {
            toml::Value::Table(ours) => ours,
            _ => toml::Table::new(),
        };
        // A `desktop` key that is not a table is replaced, as before.
        if !doc
            .get(DESKTOP_TABLE)
            .is_some_and(toml_edit::Item::is_table_like)
        {
            doc.insert(DESKTOP_TABLE, toml_edit::table());
        }
        let Some(table) = doc
            .get_mut(DESKTOP_TABLE)
            .and_then(toml_edit::Item::as_table_like_mut)
        else {
            return Err(serialize(format!(
                "the [{DESKTOP_TABLE}] table could not be made"
            )));
        };
        for key in DESKTOP_KEYS {
            match ours.get(*key) {
                Some(value) => {
                    // toml's text for one value is toml_edit's input.
                    let value = value
                        .to_string()
                        .parse::<toml_edit::Value>()
                        .map_err(|err| serialize(err.to_string()))?;
                    match table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
                        // Keeps the comments around the old value.
                        Some(old) => {
                            let decor = old.decor().clone();
                            *old = value;
                            *old.decor_mut() = decor;
                        }
                        None => {
                            table.insert(key, toml_edit::value(value));
                        }
                    }
                }
                None => {
                    table.remove(key);
                }
            }
        }
        write_settings_text(path, &doc.to_string())
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
    let text = toml::to_string_pretty(table).map_err(|err| AppearanceError::Serialize {
        path: path.to_owned(),
        message: err.to_string(),
    })?;
    write_settings_text(path, &text)
}

/// Writes `text` as the whole settings file at `path`, as
/// [`write_settings_table`] does.
fn write_settings_text(path: &Path, text: &str) -> Result<(), AppearanceError> {
    let io = |source| AppearanceError::Io {
        path: path.to_owned(),
        source,
    };
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
