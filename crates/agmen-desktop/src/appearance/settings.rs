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

/// The `[appearance]` keys of agmen's settings file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AgmenSettings {
    /// Read theme, icon theme and fonts from Zed's settings.
    pub(crate) follow_zed: bool,
    pub(crate) theme: Option<String>,
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

impl AgmenSettings {
    /// Reads `path`; a missing file gives the defaults.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read or is not valid settings.
    pub(crate) fn load(path: &Path) -> Result<Self, AppearanceError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(AppearanceError::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|err| AppearanceError::Parse {
            path: path.to_owned(),
            message: err.to_string(),
        })
    }

    /// Writes `path`, creating its folder.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Settings saves with it in Task 17 of the desktop plan"
        )
    )]
    ///
    /// # Errors
    ///
    /// When the folder or file cannot be written.
    pub(crate) fn save(&self, path: &Path) -> Result<(), AppearanceError> {
        let io = |source| AppearanceError::Io {
            path: path.to_owned(),
            source,
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io)?;
        }
        let text = toml::to_string_pretty(self).map_err(|err| AppearanceError::Parse {
            path: path.to_owned(),
            message: err.to_string(),
        })?;
        fs::write(path, text).map_err(io)
    }
}
