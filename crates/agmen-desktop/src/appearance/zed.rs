//! Reads the appearance keys of Zed's `settings.json`, which is JSONC.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{AppearanceError, FontPrefs, SystemMode};

/// Zed's settings files are small; anything larger is not one.
const MAX_SETTINGS_BYTES: u64 = 4 * 1024 * 1024;

/// Light, dark, or whatever the OS uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Mode {
    Light,
    Dark,
    #[default]
    System,
}

/// Zed's `theme` setting: a name, or a light and dark pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(untagged)]
pub(crate) enum ThemeChoice {
    Named(String),
    BySystem {
        #[serde(default)]
        mode: Mode,
        light: String,
        dark: String,
    },
}

impl ThemeChoice {
    /// The theme name to use when the OS is in `system` mode.
    pub(crate) fn name_for(&self, system: SystemMode) -> &str {
        match self {
            Self::Named(name) => name,
            Self::BySystem { mode, light, dark } => match (mode, system) {
                (Mode::Light, _) | (Mode::System, SystemMode::Light) => light,
                (Mode::Dark, _) | (Mode::System, SystemMode::Dark) => dark,
            },
        }
    }
}

/// The keys agmen reads from Zed's settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ZedSettings {
    pub(crate) theme: Option<ThemeChoice>,
    pub(crate) icon_theme: Option<String>,
    pub(crate) fonts: FontPrefs,
}

#[derive(Deserialize)]
struct Raw {
    theme: Option<ThemeChoice>,
    icon_theme: Option<IconThemeSetting>,
    ui_font_family: Option<String>,
    ui_font_size: Option<f32>,
    buffer_font_family: Option<String>,
    buffer_font_size: Option<f32>,
    terminal: Option<RawTerminal>,
}

/// Zed's `icon_theme` is a name or a light and dark pair, like `theme`.
#[derive(Deserialize)]
#[serde(untagged)]
enum IconThemeSetting {
    Named(String),
    BySystem { dark: String },
}

#[derive(Deserialize)]
struct RawTerminal {
    font_family: Option<String>,
    font_size: Option<f32>,
}

/// Reads `dir/settings.json`.
///
/// # Errors
///
/// When the file is missing, too large, or not JSONC with the expected types.
pub(crate) fn read_zed_settings(dir: &Path) -> Result<ZedSettings, AppearanceError> {
    let path = dir.join("settings.json");
    let raw: Raw = read_jsonc(&path)?;
    let terminal = raw.terminal;
    Ok(ZedSettings {
        theme: raw.theme,
        icon_theme: raw.icon_theme.map(|t| match t {
            IconThemeSetting::Named(name) | IconThemeSetting::BySystem { dark: name } => name,
        }),
        fonts: FontPrefs {
            ui_family: raw.ui_font_family,
            ui_size: raw.ui_font_size,
            code_family: raw.buffer_font_family,
            code_size: raw.buffer_font_size,
            code_ligatures: None,
            terminal_family: terminal.as_ref().and_then(|t| t.font_family.clone()),
            terminal_size: terminal.and_then(|t| t.font_size),
        },
    })
}

/// Reads a JSONC file (comments and trailing commas allowed) of at most
/// [`MAX_SETTINGS_BYTES`]. jsonc-parser caps nesting at 512 levels.
pub(crate) fn read_jsonc<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<T, AppearanceError> {
    let io = |source| AppearanceError::Io {
        path: path.to_owned(),
        source,
    };
    let len = fs::metadata(path).map_err(io)?.len();
    if len > MAX_SETTINGS_BYTES {
        return Err(AppearanceError::TooLarge {
            path: path.to_owned(),
            len,
        });
    }
    let text = fs::read_to_string(path).map_err(io)?;
    let value: serde_json::Value =
        jsonc_parser::parse_to_serde_value(&text, &jsonc_parser::ParseOptions::default()).map_err(
            |err| AppearanceError::Parse {
                path: path.to_owned(),
                message: err.to_string(),
            },
        )?;
    serde_json::from_value(value).map_err(|err| AppearanceError::Parse {
        path: path.to_owned(),
        message: err.to_string(),
    })
}
