use std::fs;

use gpui::{Hsla, Rgba, WindowBackgroundAppearance};

use super::*;
use crate::appearance::fallback::gruvbox_material_dark_hard;
use crate::appearance::theme::window_background;

fn hex(s: &str) -> Hsla {
    Rgba::try_from(s).unwrap().into()
}

fn close(a: Hsla, b: Hsla) -> bool {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    let tol = 1.0 / 255.0 + f32::EPSILON;
    (a.r - b.r).abs() <= tol
        && (a.g - b.g).abs() <= tol
        && (a.b - b.b).abs() <= tol
        && (a.a - b.a).abs() <= tol
}

#[test]
fn wallpaper_adaptive_matches_marked() {
    let t = wallpaper_adaptive();
    assert!(close(t.text_accent, hex("#68acd4")));
    assert!(close(t.background, hex("#101010b8")));
    assert!(t.background.a < 1.0, "Marked's background is translucent");
    assert_eq!(t.accents.len(), 7);
    // Marked's syntax JSON: keyword is ramp slot 5, string slot 2, number slot 0.
    // Every ramp slot lands where Marked's syntax JSON puts it (C123).
    for (key, want) in [
        ("number", "#aea1db"),
        ("type", "#e0a29c"),
        ("string", "#b5bd88"),
        ("operator", "#97abb7"),
        ("function", "#63bec9"),
        ("keyword", "#d99db3"),
        ("string.special", "#89a4de"),
        ("punctuation.list_marker", "#97abb7"),
        ("text.code.span", "#b5bd88"),
    ] {
        assert!(close(t.syntax[key], hex(want)), "{key}");
    }
    // The eighth slot (#c5ab72) has no syntax name in Marked's JSON.
    assert!(close(t.syntax["comment"], hex("#71717a")));
    assert!(close(t.created, hex("#62b193")));
    assert!(close(t.error, hex("#d590aa")), "destructive is error");
    assert!(close(t.info, hex("#6faad7")), "informational is info");
}

#[test]
fn state_json_overrides_accent_and_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    fs::write(
        &path,
        r##"{"active_palette": {"accent": "#ff0000", "accents": ["#00ff00"],
            "syntax": ["#000001", "#000002", "#000003", "#000004", "#000005", "#000006", "#000007", "#000008"],
            "created": "#123456"}}"##,
    )
    .unwrap();
    let t = load_wallpaper_palette(&path, wallpaper_adaptive()).unwrap();
    assert!(close(t.text_accent, hex("#ff0000")));
    assert_eq!(t.accents.len(), 1);
    assert!(close(t.syntax["keyword"], hex("#000006")));
    assert!(close(t.created, hex("#123456")));
}

#[test]
fn missing_state_json_keeps_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let t = load_wallpaper_palette(&dir.path().join("nope.json"), wallpaper_adaptive()).unwrap();
    assert_eq!(t, wallpaper_adaptive());
}

#[test]
fn translucent_theme_requests_blur() {
    assert_eq!(
        window_background(&wallpaper_adaptive()),
        WindowBackgroundAppearance::Blurred
    );
    assert_eq!(
        window_background(&gruvbox_material_dark_hard()),
        WindowBackgroundAppearance::Opaque
    );
}

#[test]
fn broken_state_json_warns() {
    // Review C126.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    fs::write(&path, "{ not json").unwrap();
    assert!(load_wallpaper_palette(&path, wallpaper_adaptive()).is_err());
}
