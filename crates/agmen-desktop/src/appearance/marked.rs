//! Marked's palette, "Wallpaper Adaptive (Balanced)", from Andy's Marked
//! editor (MIT, its `src/theme.rs`), offered as a built-in theme next to the
//! Gruvbox fallback.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui::{Hsla, Rgba};
use serde_json::Value;

use super::AppearanceError;
use super::fallback::gruvbox_material_dark_hard;
use super::theme::Theme;
use super::zed::read_jsonc;

/// The built-in theme's name, as Marked shows it.
pub(crate) const MARKED_THEME: &str = "Wallpaper Adaptive (Balanced)";

fn color(hex: &str) -> Option<Hsla> {
    Rgba::try_from(hex).ok().map(Hsla::from)
}

/// Fills Zed's syntax names from Marked's 8-color ramp the way Marked's own
/// Zed-style syntax JSON does (Marked `src/theme.rs`, `build_highlight_theme`).
fn syntax_from_ramp(
    ramp: &[Hsla],
    accent: Hsla,
    text: Hsla,
    muted: Hsla,
) -> BTreeMap<String, Hsla> {
    let at = |i: usize| ramp.get(i).copied().unwrap_or(accent);
    [
        ("title", text),
        ("keyword", at(5)),
        ("function", at(4)),
        ("string", at(2)),
        ("string.special", at(6)),
        ("number", at(0)),
        ("boolean", at(0)),
        ("constant", at(0)),
        ("type", at(1)),
        ("tag", accent),
        ("operator", at(3)),
        ("punctuation", muted),
        ("punctuation.special", accent),
        ("punctuation.list_marker", at(3)),
        ("text.code.span", at(2)),
        ("comment", gray(0x71, 0x71, 0x7a)),
        ("comment.doc", gray(0x71, 0x71, 0x7a)),
        ("link_text", accent),
        ("link_uri", at(6)),
        ("text.literal", at(2)),
        ("variable", text),
        ("property", at(5)),
    ]
    .into_iter()
    .map(|(key, c)| (key.to_owned(), c))
    .collect()
}

fn gray(r: u8, g: u8, b: u8) -> Hsla {
    Rgba {
        r: f32::from(r) / 255.0,
        g: f32::from(g) / 255.0,
        b: f32::from(b) / 255.0,
        a: 1.0,
    }
    .into()
}

/// Marked's default palette (Marked `src/theme.rs` lines 32 to 80).
#[must_use]
pub(crate) fn wallpaper_adaptive() -> Theme {
    let c = |hex: &str, fallback: Hsla| color(hex).unwrap_or(fallback);
    let base = gruvbox_material_dark_hard();
    let accent = c("#68acd4", base.text_accent);
    let text = c("#f4f4f5", base.text);
    let muted = c("#a1a1aa", base.text_muted);
    let ramp: Vec<Hsla> = [
        "#aea1db", "#e0a29c", "#b5bd88", "#97abb7", "#63bec9", "#d99db3", "#89a4de", "#c5ab72",
    ]
    .iter()
    .filter_map(|h| color(h))
    .collect();
    let accents = [
        "#68acd4", "#e0a29c", "#63bec9", "#b5bd88", "#62b193", "#98b374", "#6faad7",
    ]
    .iter()
    .filter_map(|h| color(h))
    .collect();
    let border = c("#ffffff15", base.border);
    Theme {
        background: c("#101010b8", base.background),
        surface: c("#121214a0", base.surface),
        elevated_surface: c("#18181be0", base.elevated_surface),
        element: c("#20202480", base.element),
        element_hover: c("#2c2c32a0", base.element_hover),
        element_selected: c("#36363ea0", base.element_selected),
        border,
        border_variant: border,
        text,
        text_muted: muted,
        text_placeholder: c("#71717a", base.text_placeholder),
        text_accent: accent,
        selection: c("#ffffff20", base.selection),
        created: c("#62b193", base.created),
        modified: c("#98b374", base.modified),
        // Marked's destructive and informational are Zed's error and info.
        error: c("#d590aa", base.error),
        deleted: c("#d590aa", base.deleted),
        warning: c("#abad69", base.warning),
        info: c("#6faad7", base.info),
        syntax: syntax_from_ramp(&ramp, accent, text, muted),
        accents,
        ..base
    }
}

/// Where Marked's wallpaper palette utility keeps its state. It exists on
/// Windows only today (`%LOCALAPPDATA%/ZedWallpaperThemeButton/state.json`).
// shortcut: no macOS path until the wallpaper utility runs on macOS.
fn state_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|dir| PathBuf::from(dir).join("ZedWallpaperThemeButton/state.json"))
}

/// Marked's palette with the wallpaper utility's current colors over it. A
/// palette file that cannot be read leaves the defaults and a warning.
pub(crate) fn with_wallpaper(warnings: &mut Vec<String>) -> Theme {
    with_wallpaper_at(state_path(), warnings)
}

/// [`with_wallpaper`] with the state file's path given.
pub(crate) fn with_wallpaper_at(path: Option<PathBuf>, warnings: &mut Vec<String>) -> Theme {
    let base = wallpaper_adaptive();
    let Some(path) = path else {
        return base;
    };
    match load_wallpaper_palette(&path, base.clone()) {
        Ok(theme) => theme,
        Err(err) => {
            warnings.push(format!("{err}; using Marked's default palette"));
            base
        }
    }
}

/// Overlays `active_palette` from the utility's `state.json` on `base`, as
/// Marked's `MarkedTheme::load` does. A missing file leaves `base` as is.
///
/// # Errors
///
/// When the file exists but is not JSON.
pub(crate) fn load_wallpaper_palette(path: &Path, base: Theme) -> Result<Theme, AppearanceError> {
    let state: Value = match read_jsonc(path) {
        Ok(state) => state,
        Err(AppearanceError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(base);
        }
        Err(err) => return Err(err),
    };
    let Some(active) = state.get("active_palette").and_then(Value::as_object) else {
        return Ok(base);
    };
    let one = |key: &str| active.get(key).and_then(Value::as_str).and_then(color);
    let many = |key: &str| -> Option<Vec<Hsla>> {
        let list: Vec<Hsla> = active
            .get(key)?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .filter_map(color)
            .collect();
        (!list.is_empty()).then_some(list)
    };
    let mut theme = base;
    if let Some(c) = one("accent") {
        theme.text_accent = c;
    }
    if let Some(list) = many("accents") {
        theme.accents = list;
    }
    if let Some(ramp) = many("syntax") {
        theme.syntax = syntax_from_ramp(&ramp, theme.text_accent, theme.text, theme.text_muted);
    }
    for (key, slot) in [
        ("created", &mut theme.created),
        ("modified", &mut theme.modified),
        ("destructive", &mut theme.error),
        ("warning", &mut theme.warning),
        ("informational", &mut theme.info),
    ] {
        if let Some(c) = one(key) {
            *slot = c;
        }
    }
    Ok(theme)
}

#[cfg(test)]
#[path = "marked_tests.rs"]
mod tests;
