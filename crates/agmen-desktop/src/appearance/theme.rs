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
pub(super) struct Family {
    #[serde(default)]
    pub(super) author: Option<String>,
    pub(super) themes: Vec<FamilyTheme>,
}

#[derive(Deserialize)]
pub(super) struct FamilyTheme {
    pub(super) name: String,
    /// `"light"` or `"dark"`.
    #[serde(default)]
    pub(super) appearance: Option<String>,
    #[serde(default)]
    pub(super) style: Map<String, Value>,
}

/// The folders theme files are searched in, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThemeDirs {
    /// `~/.config/zed`: themes in its `themes/` folder.
    pub(crate) zed: PathBuf,
    /// Zed's `extensions/installed`: `*/themes/*.json`, and the index
    /// beside it.
    pub(crate) extensions: PathBuf,
    /// agmen's own installs, `~/.config/agmen/themes`: `*/themes/*.json`.
    pub(crate) agmen: PathBuf,
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
    let dirs = ThemeDirs {
        zed: zed_dir.to_owned(),
        extensions: extensions_dir.to_owned(),
        agmen: zed_dir.join("no-agmen-themes"),
    };
    load_theme_with(&dirs, name, &Map::new(), &mut Vec::new())
}

/// Loads the theme named `name`, as `load_theme` does, with Zed's `theme_overrides` merged over the theme's own
/// style keys first, searching `dirs` in order (agmen's own installs last).
/// The built-in themes, Marked's palette and the Gruvbox fallback, answer to
/// their own names.
///
/// # Errors
///
/// When no readable theme file defines `name`.
pub(crate) fn load_theme_with(
    dirs: &ThemeDirs,
    name: &str,
    overrides: &Map<String, Value>,
    warnings: &mut Vec<String>,
) -> Result<Theme, AppearanceError> {
    if name == super::marked::MARKED_THEME {
        let marked = super::marked::with_wallpaper(warnings);
        // Overrides apply to the built-in theme too (review C125).
        return Ok(Theme::from_style(overrides, marked));
    }
    if name == super::fallback::BUILTIN_THEME {
        return Ok(Theme::from_style(overrides, gruvbox_material_dark_hard()));
    }
    let (files, mut unreadable) = theme_files(dirs);
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
            merge_style(&mut style, overrides);
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
/// reported. Zed's `extensions/index.json` adds theme files an extension
/// keeps outside its `themes/` folder.
pub(super) fn theme_files(dirs: &ThemeDirs) -> (Vec<PathBuf>, Vec<Skipped>) {
    let mut skipped = Vec::new();
    let mut theme_dirs = vec![dirs.zed.join("themes")];
    // Only folders are extensions; a .DS_Store beside them is not (C109).
    for root in [&dirs.extensions, &dirs.agmen] {
        theme_dirs.extend(
            list_dir(root, &mut skipped)
                .into_iter()
                .filter(|ext| ext.is_dir())
                .map(|ext| ext.join("themes")),
        );
    }
    let mut files = Vec::new();
    for (ix, dir) in theme_dirs.iter().enumerate() {
        files.extend(
            list_dir(dir, &mut skipped)
                .into_iter()
                .filter(|p| p.extension().is_some_and(|ext| ext == "json")),
        );
        // The index's extra files go after Zed's own scan, before agmen's.
        if ix == 0 {
            files.extend(indexed_theme_files(&dirs.extensions));
        }
        if files.len() >= MAX_THEME_FILES {
            files.truncate(MAX_THEME_FILES);
            break;
        }
    }
    // The index lists files the folder scan finds again; the first wins.
    let mut seen = std::collections::HashSet::new();
    files.retain(|path| seen.insert(path.clone()));
    (files, skipped)
}

/// The sorted paths in `dir`; a missing folder is empty, and an unreadable
/// one is recorded in `skipped`.
fn list_dir(dir: &Path, skipped: &mut Vec<Skipped>) -> Vec<PathBuf> {
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
}

#[derive(Deserialize)]
struct ExtensionIndex {
    #[serde(default)]
    themes: BTreeMap<String, IndexedTheme>,
}

#[derive(Deserialize)]
struct IndexedTheme {
    extension: String,
    path: PathBuf,
}

/// Theme files that Zed's `extensions/index.json` (beside `installed/`)
/// lists, as Zed resolves them: `installed/{extension}/{path}`. A missing
/// or unreadable index gives none; the folder scan still finds the rest.
fn indexed_theme_files(extensions: &Path) -> Vec<PathBuf> {
    let Some(index) = extensions.parent().map(|dir| dir.join("index.json")) else {
        return Vec::new();
    };
    let Ok(index) = read_jsonc::<ExtensionIndex>(&index) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = index
        .themes
        .into_values()
        .filter(|entry| safe_relative(Path::new(&entry.extension)) && safe_relative(&entry.path))
        .map(|entry| extensions.join(entry.extension).join(entry.path))
        .filter(|path| path.is_file())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Whether `path` stays inside the folder it is joined to: relative, with
/// no `..`, so an index entry cannot point outside Zed's extensions.
pub(super) fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
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

/// Merges `overrides` into a theme `style` the way Zed's `modify_theme`
/// does: `syntax` per highlight name (and per field within one), `players`
/// per index (and per field within one), every other key replaced.
pub(crate) fn merge_style(style: &mut Map<String, Value>, overrides: &Map<String, Value>) {
    fn merge_fields(into: &mut Value, from: &Value) {
        match (into.as_object_mut(), from.as_object()) {
            (Some(into), Some(from)) => {
                for (k, v) in from {
                    if !unusable(k, into.get(k), v) {
                        into.insert(k.clone(), v.clone());
                    }
                }
            }
            _ => *into = from.clone(),
        }
    }
    for (key, value) in overrides {
        match (key.as_str(), style.get_mut(key)) {
            ("syntax", Some(Value::Object(names))) => {
                if let Some(over) = value.as_object() {
                    for (name, entry) in over {
                        match names.get_mut(name) {
                            Some(existing) => merge_fields(existing, entry),
                            None => {
                                names.insert(name.clone(), entry.clone());
                            }
                        }
                    }
                }
            }
            ("players", Some(Value::Array(players))) => {
                if let Some(over) = value.as_array() {
                    for (i, player) in over.iter().enumerate() {
                        match players.get_mut(i) {
                            Some(existing) => merge_fields(existing, player),
                            None => players.push(player.clone()),
                        }
                    }
                }
            }
            _ => {
                if !unusable(key, style.get(key), value) {
                    style.insert(key.clone(), value.clone());
                }
            }
        }
    }
}

/// Whether an override for `key` keeps the theme's `existing` value: Zed
/// does so for a null, for a string that is not a color where the theme has
/// a color, and for an empty accents list (C142, C148).
fn unusable(key: &str, existing: Option<&Value>, value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(_) => {
            existing.and_then(parse_color).is_some() && parse_color(value).is_none()
        }
        Value::Array(list) => key == "accents" && list.is_empty(),
        _ => false,
    }
}
