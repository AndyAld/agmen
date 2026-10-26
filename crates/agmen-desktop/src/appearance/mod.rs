//! Theme, fonts and icon theme: read from Zed, or from agmen settings.

mod fallback;
mod settings;
mod theme;
mod zed;

use std::path::{Path, PathBuf};

use gpui::{Global, Pixels, SharedString, px};
use serde::{Deserialize, Serialize};

pub(crate) use settings::{AgmenSettings, Density, IconThemeChoice};
pub(crate) use theme::Theme;

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
    #[error("no installed Zed theme is named \"{0}\"")]
    ThemeNotFound(String),
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
#[serde(default, deny_unknown_fields)]
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
        let code_size = size(prefs.code_size, 13.0);
        Self {
            ui_family: prefs
                .ui_family
                .clone()
                .unwrap_or_else(|| ".SystemUIFont".into())
                .into(),
            ui_size: size(prefs.ui_size, 15.0),
            code_family: prefs
                .code_family
                .clone()
                .unwrap_or_else(|| "Menlo".into())
                .into(),
            code_size,
            code_ligatures: prefs.code_ligatures.unwrap_or(true),
            terminal_family: prefs.terminal_family.clone().map(Into::into),
            terminal_size: prefs
                .terminal_size
                .map_or(code_size, |s| size(Some(s), 13.0)),
        }
    }

    fn overlay(base: &FontPrefs, top: &FontPrefs) -> FontPrefs {
        FontPrefs {
            ui_family: top.ui_family.clone().or_else(|| base.ui_family.clone()),
            ui_size: top.ui_size.or(base.ui_size),
            code_family: top.code_family.clone().or_else(|| base.code_family.clone()),
            code_size: top.code_size.or(base.code_size),
            code_ligatures: top.code_ligatures.or(base.code_ligatures),
            terminal_family: top
                .terminal_family
                .clone()
                .or_else(|| base.terminal_family.clone()),
            terminal_size: top.terminal_size.or(base.terminal_size),
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
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "file icons read it in Task 5 of the desktop plan")
    )]
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

/// Resolves the appearance for a person whose home folder is `home`.
#[must_use]
pub(crate) fn resolve(home: &Path, system: SystemMode) -> Appearance {
    resolve_paths(
        &home.join(".config"),
        &home.join("Library/Application Support/Zed/extensions/installed"),
        system,
    )
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
            Err(AppearanceError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                None
            }
            Err(err) => {
                warnings.push(err.to_string());
                None
            }
        }
    } else {
        None
    };

    let (theme_name, prefs, icon_theme, mut source) = match &zed {
        Some(z) => (
            z.theme.as_ref().map(|t| t.name_for(system).to_owned()),
            z.fonts.clone(),
            z.icon_theme
                .clone()
                .map_or(IconThemeChoice::System, IconThemeChoice::Zed),
            AppearanceSource::Zed,
        ),
        None if agmen.follow_zed => (
            None,
            agmen.fonts.clone(),
            IconThemeChoice::System,
            AppearanceSource::Builtin,
        ),
        None => (
            agmen.theme.clone(),
            Fonts::overlay(&FontPrefs::default(), &agmen.fonts),
            agmen.icon_theme.clone(),
            AppearanceSource::Agmen,
        ),
    };

    let theme = match theme_name {
        Some(name) => theme::load_theme(&zed_dir, extensions, &name).unwrap_or_else(|err| {
            warnings.push(format!("{err}; using the built-in Gruvbox theme"));
            source = AppearanceSource::Builtin;
            fallback::gruvbox_material_dark_hard()
        }),
        None => {
            if source != AppearanceSource::Builtin {
                source = AppearanceSource::Builtin;
            }
            fallback::gruvbox_material_dark_hard()
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
