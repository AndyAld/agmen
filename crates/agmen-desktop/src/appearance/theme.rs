//! Zed theme JSON (schema v0.2.0) to the colors agmen paints with.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use gpui::{Hsla, Rgba};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::fallback::gruvbox_material_dark_hard;
use super::zed::read_jsonc;
use super::{AppearanceError, Skipped};

/// How many theme files a search reads at most.
const MAX_THEME_FILES: usize = 2_000;

/// The colors agmen paints with, from one Zed theme.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Theme {
    pub(crate) background: Hsla,
    pub(crate) surface: Hsla,
    pub(crate) elevated_surface: Hsla,
    pub(crate) element_selected: Hsla,
    pub(crate) element_hover: Hsla,
    pub(crate) border: Hsla,
    pub(crate) border_variant: Hsla,
    pub(crate) text: Hsla,
    pub(crate) text_muted: Hsla,
    pub(crate) text_placeholder: Hsla,
    pub(crate) text_accent: Hsla,
    pub(crate) icon_muted: Hsla,
    pub(crate) editor_background: Hsla,
    pub(crate) line_number: Hsla,
    pub(crate) error: Hsla,
    pub(crate) warning: Hsla,
    pub(crate) success: Hsla,
    pub(crate) info: Hsla,
    pub(crate) created: Hsla,
    pub(crate) modified: Hsla,
    pub(crate) deleted: Hsla,
    /// `terminal.ansi.*`: black, red, green, yellow, blue, magenta, cyan,
    /// white, then the bright variants in the same order.
    pub(crate) ansi: [Hsla; 16],
    /// The `players` cursor colors.
    pub(crate) players: Vec<Hsla>,
    /// Fills for controls at rest (`element.background`).
    pub(crate) element: Hsla,
    /// Text selection (`players[0].selection`).
    pub(crate) selection: Hsla,
    /// Accent colors for things that need telling apart (`accents`).
    pub(crate) accents: Vec<Hsla>,
    /// Highlight colors by Zed's syntax names (`keyword`, `string`, ...).
    pub(crate) syntax: BTreeMap<String, Hsla>,
}

impl Theme {
    /// A role's color, from the theme's player palette so it stays readable
    /// in light and dark themes. Unknown roles get the muted text color.
    pub(crate) fn role_color(&self, role: &str) -> Hsla {
        let slot = match role {
            "planner" => 0,
            "coordinator" => 1,
            "reviewer" => 2,
            "engineer" => 3,
            "researcher" => 4,
            "tester" => 6,
            // The mockup's listener is between placeholder and body text
            // (#a89984 in Gruvbox); no theme key holds it, so mix the two.
            "listener" => return mix(self.text_placeholder, self.text, 0.3),
            _ => return self.text_muted,
        };
        self.players.get(slot).copied().unwrap_or(self.text_muted)
    }

    fn from_style(style: &Map<String, Value>, base: Self) -> Self {
        let pick = |keys: &[&str], default: Hsla| {
            keys.iter()
                .find_map(|key| style.get(*key).and_then(parse_color))
                .unwrap_or(default)
        };
        let ansi_names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let mut ansi = base.ansi;
        for (i, name) in ansi_names.iter().enumerate() {
            ansi[i] = pick(&[&format!("terminal.ansi.{name}")], base.ansi[i]);
            ansi[i + 8] = pick(&[&format!("terminal.ansi.bright_{name}")], base.ansi[i + 8]);
        }
        let players = match style.get("players").and_then(Value::as_array) {
            Some(list) if !list.is_empty() => {
                // A player without a cursor color keeps the fallback's color
                // for that slot, so later slots do not shift.
                let mut out: Vec<Hsla> = list
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        p.get("cursor")
                            .and_then(parse_color)
                            .or_else(|| base.players.get(i).copied())
                            .unwrap_or(base.text)
                    })
                    .collect();
                // Keep every slot role_color uses, even for short palettes.
                out.extend(base.players.iter().skip(out.len()).copied());
                out
            }
            _ => base.players.clone(),
        };
        let mut syntax = base.syntax.clone();
        if let Some(map) = style.get("syntax").and_then(Value::as_object) {
            for (key, entry) in map {
                if let Some(color) = entry.get("color").and_then(parse_color) {
                    syntax.insert(key.clone(), color);
                }
            }
        }
        let accents = style
            .get("accents")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(parse_color).collect::<Vec<_>>())
            .filter(|list| !list.is_empty())
            .unwrap_or_else(|| base.accents.clone());
        let selection = style
            .get("players")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .and_then(|p| p.get("selection"))
            .and_then(parse_color)
            .unwrap_or(base.selection);
        Self {
            element: pick(&["element.background"], base.element),
            selection,
            accents,
            syntax,
            background: pick(&["background"], base.background),
            surface: pick(&["surface.background"], base.surface),
            elevated_surface: pick(&["elevated_surface.background"], base.elevated_surface),
            element_selected: pick(&["element.selected"], base.element_selected),
            element_hover: pick(&["element.hover"], base.element_hover),
            border: pick(&["border"], base.border),
            border_variant: pick(&["border.variant"], base.border_variant),
            text: pick(&["text"], base.text),
            text_muted: pick(&["text.muted"], base.text_muted),
            text_placeholder: pick(&["text.placeholder"], base.text_placeholder),
            text_accent: pick(&["text.accent"], base.text_accent),
            icon_muted: pick(&["icon.muted"], base.icon_muted),
            editor_background: pick(&["editor.background"], base.editor_background),
            line_number: pick(&["editor.line_number"], base.line_number),
            error: pick(&["error"], base.error),
            warning: pick(&["warning"], base.warning),
            success: pick(&["success"], base.success),
            info: pick(&["info"], base.info),
            created: pick(&["version_control.added", "created"], base.created),
            modified: pick(&["version_control.modified", "modified"], base.modified),
            deleted: pick(&["version_control.deleted", "deleted"], base.deleted),
            ansi,
            players,
        }
    }
}

/// `a` moved `t` of the way toward `b`, in RGB.
fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    }
    .into()
}

fn parse_color(value: &Value) -> Option<Hsla> {
    value
        .as_str()
        .and_then(|s| Rgba::try_from(s).ok())
        .map(Hsla::from)
}

#[derive(Deserialize)]
struct Family {
    themes: Vec<FamilyTheme>,
}

#[derive(Deserialize)]
struct FamilyTheme {
    name: String,
    #[serde(default)]
    style: Map<String, Value>,
}

/// Loads the theme named `name` from `zed_dir/themes/*.json`, then from
/// `extensions_dir/*/themes/*.json`. Keys the theme lacks come from the
/// built-in theme.
///
/// # Errors
///
/// When no readable theme file defines `name`.
#[cfg(test)]
pub(crate) fn load_theme(
    zed_dir: &Path,
    extensions_dir: &Path,
    name: &str,
) -> Result<Theme, AppearanceError> {
    load_theme_with(zed_dir, extensions_dir, name, &Map::new())
}

/// Loads the theme named `name`, as `load_theme` does, with Zed's `theme_overrides` merged over the theme's own
/// style keys first. Marked's built-in palette answers to its own name.
///
/// # Errors
///
/// When no readable theme file defines `name`.
pub(crate) fn load_theme_with(
    zed_dir: &Path,
    extensions_dir: &Path,
    name: &str,
    overrides: &Map<String, Value>,
) -> Result<Theme, AppearanceError> {
    if name == super::marked::MARKED_THEME {
        return Ok(super::marked::with_wallpaper());
    }
    let (files, mut unreadable) = theme_files(zed_dir, extensions_dir);
    for path in files {
        // A file that does not parse is skipped, and named if nothing matches.
        let family = match read_jsonc::<Family>(&path) {
            Ok(family) => family,
            Err(err) => {
                unreadable.push(Skipped::new(&path, err));
                continue;
            }
        };
        if let Some(theme) = family.themes.into_iter().find(|t| t.name == name) {
            let mut style = theme.style;
            style.extend(overrides.clone());
            return Ok(Theme::from_style(&style, gruvbox_material_dark_hard()));
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

/// Theme files to search, and the folders that could not be read. A
/// missing folder is normal (most people have no `themes/` folder) and not
/// reported.
fn theme_files(zed_dir: &Path, extensions_dir: &Path) -> (Vec<PathBuf>, Vec<Skipped>) {
    let mut skipped = Vec::new();
    let list = |dir: &Path, skipped: &mut Vec<Skipped>| -> Vec<PathBuf> {
        match fs::read_dir(dir) {
            Ok(entries) => {
                let mut out = Vec::new();
                for entry in entries {
                    match entry {
                        Ok(entry) => out.push(entry.path()),
                        Err(err) => skipped.push(Skipped::new(dir, err)),
                    }
                }
                out.sort();
                out
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                skipped.push(Skipped::new(dir, err));
                Vec::new()
            }
        }
    };
    let mut dirs = vec![zed_dir.join("themes")];
    // Only folders are extensions; a .DS_Store beside them is not (C109).
    dirs.extend(
        list(extensions_dir, &mut skipped)
            .into_iter()
            .filter(|ext| ext.is_dir())
            .map(|ext| ext.join("themes")),
    );
    let mut files = Vec::new();
    for dir in dirs {
        files.extend(
            list(&dir, &mut skipped)
                .into_iter()
                .filter(|p| p.extension().is_some_and(|ext| ext == "json")),
        );
        if files.len() >= MAX_THEME_FILES {
            files.truncate(MAX_THEME_FILES);
            break;
        }
    }
    (files, skipped)
}

/// How the window paints behind the theme: blurred when the theme's
/// background lets the desktop show through (Marked's palette), as GPUI's own
/// window appearance does it, so no platform code of ours is needed.
pub(crate) fn window_background(theme: &Theme) -> gpui::WindowBackgroundAppearance {
    if theme.background.a < 1.0 {
        gpui::WindowBackgroundAppearance::Blurred
    } else {
        gpui::WindowBackgroundAppearance::Opaque
    }
}
