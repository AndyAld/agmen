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
    // shortcut: mode is stored but not applied; Task 17's theme picker uses it to pick light or dark.
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
        let Some(mut table) = read_table(path)? else {
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Settings saves with it in Task 17 of the desktop plan"
        )
    )]
    pub(crate) fn save(&self, path: &Path) -> Result<(), AppearanceError> {
        let io = |source| AppearanceError::Io {
            path: path.to_owned(),
            source,
        };
        let mut table = read_table(path)?.unwrap_or_default();
        let value = toml::Value::try_from(self).map_err(|err| AppearanceError::Serialize {
            path: path.to_owned(),
            message: err.to_string(),
        })?;
        table.insert(TABLE.to_owned(), value);
        let text = toml::to_string_pretty(&table).map_err(|err| AppearanceError::Serialize {
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
}

/// The whole file as a table, `None` when it does not exist.
fn read_table(path: &Path) -> Result<Option<toml::Table>, AppearanceError> {
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
