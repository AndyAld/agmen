//! Theme, fonts and icon theme: read from Zed, or from agmen settings.

mod fallback;
mod marked;
mod settings;
mod theme;
mod zed;

use std::path::{Path, PathBuf};

use gpui::{Global, Pixels, SharedString, px};
use serde::{Deserialize, Serialize};

pub(crate) use settings::{AgmenSettings, Density, IconThemeChoice};
pub(crate) use theme::{Theme, window_background};
pub(crate) use zed::read_jsonc;

/// What can go wrong reading appearance files.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AppearanceError {
    #[error("cannot read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} is {len} bytes, too large for a settings file", path.display())]
    TooLarge { path: PathBuf, len: u64 },
    #[error("cannot parse {}: {message}", path.display())]
    Parse { path: PathBuf, message: String },
    #[error("cannot write {}: {message}", path.display())]
    Serialize { path: PathBuf, message: String },
    #[error("no installed Zed theme is named \"{0}\"")]
    ThemeNotFound(String),
    #[error(
        "no installed Zed theme is named \"{name}\" ({} theme files could not be read: {})",
        skipped.len(),
        skipped.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
    )]
    ThemeNotFoundSkipped { name: String, skipped: Vec<Skipped> },
}

/// A theme file or folder passed over while searching, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Skipped {
    pub(crate) path: PathBuf,
    pub(crate) reason: String,
}

impl std::fmt::Display for Skipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.path.display(), self.reason)
    }
}

impl Skipped {
    pub(crate) fn new(path: &Path, reason: impl std::fmt::Display) -> Self {
        Self {
            path: path.to_owned(),
            reason: reason.to_string(),
        }
    }
}

/// Whether the OS is in light or dark mode right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SystemMode {
    Light,
    Dark,
}

/// Font choices where every key is optional; Zed's settings and agmen's
/// settings both fill one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct FontPrefs {
    pub(crate) ui_family: Option<String>,
    pub(crate) ui_size: Option<f32>,
    pub(crate) code_family: Option<String>,
    pub(crate) code_size: Option<f32>,
    pub(crate) code_ligatures: Option<bool>,
    pub(crate) terminal_family: Option<String>,
    pub(crate) terminal_size: Option<f32>,
}

/// Font sizes outside this range are clamped.
const FONT_SIZES: std::ops::RangeInclusive<f32> = 6.0..=72.0;

/// The fonts agmen paints with.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fonts {
    pub(crate) ui_family: SharedString,
    pub(crate) ui_size: Pixels,
    pub(crate) code_family: SharedString,
    pub(crate) code_size: Pixels,
    pub(crate) code_ligatures: bool,
    /// `None` means the code font.
    pub(crate) terminal_family: Option<SharedString>,
    pub(crate) terminal_size: Pixels,
}

impl Fonts {
    fn from_prefs(prefs: &FontPrefs) -> Self {
        let size = |v: Option<f32>, default: f32| {
            px(v.filter(|v| v.is_finite())
                .unwrap_or(default)
                .clamp(*FONT_SIZES.start(), *FONT_SIZES.end()))
        };
        // Zed's own defaults (assets/settings/default.json) for unset keys.
        let code_size = size(prefs.code_size, 15.0);
        Self {
            ui_family: prefs
                .ui_family
                .clone()
                .unwrap_or_else(|| ".ZedSans".into())
                .into(),
            ui_size: size(prefs.ui_size, 16.0),
            code_family: prefs
                .code_family
                .clone()
                .unwrap_or_else(|| ".ZedMono".into())
                .into(),
            code_size,
            code_ligatures: prefs.code_ligatures.unwrap_or(true),
            terminal_family: prefs.terminal_family.clone().map(Into::into),
            terminal_size: prefs
                .terminal_size
                .map_or(code_size, |s| size(Some(s), 15.0)),
        }
    }
}

/// Where the theme in use came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AppearanceSource {
    /// Zed's settings (Follow Zed).
    Zed,
    /// agmen's own settings.
    Agmen,
    /// The built-in Gruvbox theme, because nothing else loaded.
    Builtin,
}

/// Everything the views paint with, held as a GPUI global.
#[derive(Debug, Clone)]
pub(crate) struct Appearance {
    pub(crate) theme: Theme,
    pub(crate) fonts: Fonts,
    pub(crate) icon_theme: IconThemeChoice,
    #[expect(
        dead_code,
        reason = "chat spacing reads it in Task 9 of the desktop plan"
    )]
    pub(crate) density: Density,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Settings shows it in Task 17 of the desktop plan")
    )]
    pub(crate) source: AppearanceSource,
    /// Why something fell back, in words for the Settings view.
    pub(crate) warnings: Vec<String>,
}

impl Global for Appearance {}

impl Appearance {
    /// The built-in theme and default fonts, with nothing read from disk.
    #[must_use]
    pub(crate) fn builtin() -> Self {
        Self {
            theme: fallback::gruvbox_material_dark_hard(),
            fonts: Fonts::from_prefs(&FontPrefs::default()),
            icon_theme: IconThemeChoice::System,
            density: Density::Comfortable,
            source: AppearanceSource::Builtin,
            warnings: Vec::new(),
        }
    }
}

/// Where Zed keeps installed extensions under a home folder.
pub(crate) fn zed_extensions_dir(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Zed/extensions/installed")
}

/// Resolves the appearance for a person whose home folder is `home`.
#[must_use]
pub(crate) fn resolve(home: &Path, system: SystemMode) -> Appearance {
    resolve_paths(&home.join(".config"), &zed_extensions_dir(home), system)
}

/// [`resolve`] with explicit folders: `config` holds `zed/` and `agmen/`.
/// Never fails: each fallback adds a warning instead.
#[must_use]
pub(crate) fn resolve_paths(config: &Path, extensions: &Path, system: SystemMode) -> Appearance {
    let zed_dir = config.join("zed");
    let agmen_path = config.join("agmen/settings.toml");
    let mut warnings = Vec::new();

    let agmen = AgmenSettings::load(&agmen_path).unwrap_or_else(|err| {
        warnings.push(format!("{err}; using the defaults"));
        AgmenSettings::default()
    });

    let zed = if agmen.follow_zed {
        match zed::read_zed_settings(&zed_dir) {
            Ok(zed) => Some(zed),
            Err(AppearanceError::Io { source, path })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                warnings.push(format!(
                    "Follow Zed is on, but there are no Zed settings at {}; using agmen's own",
                    path.display()
                ));
                None
            }
            Err(err) => {
                warnings.push(format!("{err}; using agmen's own settings"));
                None
            }
        }
    } else {
        None
    };

    // Zed's choices when following Zed, agmen's own otherwise.
    let (theme_name, prefs, icon_theme, wanted) = match &zed {
        Some(z) => (
            z.theme.as_ref().map(|t| t.name_for(system).to_owned()),
            z.fonts.clone(),
            z.icon_theme.as_ref().map_or(IconThemeChoice::System, |t| {
                IconThemeChoice::Zed(t.name_for(system).to_owned())
            }),
            AppearanceSource::Zed,
        ),
        None => (
            agmen.theme.clone(),
            agmen.fonts.clone(),
            agmen.icon_theme.clone(),
            AppearanceSource::Agmen,
        ),
    };

    let (theme, source) = match theme_name {
        Some(name) => match theme::load_theme_with(
            &zed_dir,
            extensions,
            &name,
            &zed.as_ref()
                .map(|z| z.overrides_for(&name))
                .unwrap_or_default(),
            &mut warnings,
        ) {
            Ok(theme) => (theme, wanted),
            Err(err) => {
                warnings.push(format!("{err}; using the built-in Gruvbox theme"));
                (
                    fallback::gruvbox_material_dark_hard(),
                    AppearanceSource::Builtin,
                )
            }
        },
        None => {
            let place = if wanted == AppearanceSource::Zed {
                "Zed's settings name"
            } else {
                "agmen's settings.toml names"
            };
            warnings.push(format!(
                "{place} no theme; using the built-in Gruvbox theme"
            ));
            (
                fallback::gruvbox_material_dark_hard(),
                AppearanceSource::Builtin,
            )
        }
    };

    Appearance {
        theme,
        fonts: Fonts::from_prefs(&prefs),
        icon_theme,
        density: agmen.density,
        source,
        warnings,
    }
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod tests;
