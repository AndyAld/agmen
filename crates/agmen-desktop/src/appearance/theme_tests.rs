use std::fs;
use std::path::{Path, PathBuf};

use gpui::{Hsla, Rgba};

use super::fallback::gruvbox_material_dark_hard;
use super::settings::{AgmenSettings, Density, IconThemeChoice};
use super::theme::load_theme;
use super::zed::{Mode, ThemeChoice, read_zed_settings};
use super::{AppearanceSource, SystemMode, resolve_paths};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn zed_dir() -> PathBuf {
    fixtures().join("zed")
}

fn ext_dir() -> PathBuf {
    fixtures().join("zed-ext")
}

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
fn zed_settings_jsonc_trailing_commas() {
    let s = read_zed_settings(&zed_dir()).unwrap();
    assert_eq!(
        s.theme,
        Some(ThemeChoice::Named("Gruvbox Sample Dark".into()))
    );
    assert_eq!(s.icon_theme.as_deref(), Some("Material Icon Theme"));
    assert_eq!(
        s.fonts.code_family.as_deref(),
        Some("CaskaydiaCove Nerd Font Mono")
    );
    assert_eq!(s.fonts.code_size, Some(13.0));
    assert_eq!(s.fonts.ui_size, Some(15.0));
    assert_eq!(s.fonts.terminal_size, Some(13.0));
}

#[test]
fn zed_settings_missing_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_zed_settings(dir.path()).is_err());
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let deep = format!(
        "{{\"theme\": {}{}}}",
        "[".repeat(30_000),
        "]".repeat(30_000)
    );
    fs::write(dir.path().join("settings.json"), deep).unwrap();
    assert!(read_zed_settings(dir.path()).is_err());
}

#[test]
fn theme_object_follows_system_mode() {
    let choice = ThemeChoice::BySystem {
        mode: Mode::System,
        light: "L".into(),
        dark: "D".into(),
    };
    assert_eq!(choice.name_for(SystemMode::Dark), "D");
    assert_eq!(choice.name_for(SystemMode::Light), "L");
    let forced = ThemeChoice::BySystem {
        mode: Mode::Light,
        light: "L".into(),
        dark: "D".into(),
    };
    assert_eq!(forced.name_for(SystemMode::Dark), "L");

    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("settings.json"),
        r#"{"theme": {"mode": "system", "light": "One Light", "dark": "One Dark"}}"#,
    )
    .unwrap();
    let s = read_zed_settings(dir.path()).unwrap();
    assert_eq!(
        s.theme,
        Some(ThemeChoice::BySystem {
            mode: Mode::System,
            light: "One Light".into(),
            dark: "One Dark".into(),
        })
    );
}

#[test]
fn gruvbox_hard_matches_mockup() {
    let t = load_theme(&zed_dir(), &ext_dir(), "Gruvbox Sample Dark").unwrap();
    assert!(close(t.background, hex("#1d2021")));
    assert!(close(t.text, hex("#d8bf94")));
    assert!(close(t.players[1], hex("#da7f98")));

    let fallback = gruvbox_material_dark_hard();
    assert!(close(fallback.background, hex("#1d2021")));
    assert!(close(fallback.text, hex("#d8bf94")));
    assert!(close(fallback.text_accent, hex("#72b9a9")));
}

#[test]
fn partial_theme_fills_from_fallback() {
    let t = load_theme(&zed_dir(), &ext_dir(), "Partial Dark").unwrap();
    let fallback = gruvbox_material_dark_hard();
    assert!(close(t.background, hex("#101010")));
    assert!(close(t.ansi[2], hex("#00ff00")));
    assert!(close(t.ansi[1], fallback.ansi[1]));
    assert!(close(t.text, fallback.text));
    assert_eq!(t.players.len(), fallback.players.len());
}

#[test]
fn unknown_theme_is_an_error() {
    let err = load_theme(&zed_dir(), &ext_dir(), "No Such Theme").unwrap_err();
    assert!(err.to_string().contains("No Such Theme"));
}

#[test]
fn role_colors_come_from_players() {
    let t = gruvbox_material_dark_hard();
    assert!(close(t.role_color("engineer"), hex("#ea894b")));
    assert!(close(t.role_color("reviewer"), hex("#ed675f")));
    assert!(close(t.role_color("tester"), hex("#afbf5c")));
    assert!(close(t.role_color("unknown-role"), t.text_muted));
}

fn home_with(settings_toml: Option<&str>) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let zed = home.path().join(".config/zed");
    fs::create_dir_all(zed.join("themes")).unwrap();
    fs::copy(zed_dir().join("settings.json"), zed.join("settings.json")).unwrap();
    fs::copy(
        zed_dir().join("themes/gruvbox-sample.json"),
        zed.join("themes/gruvbox-sample.json"),
    )
    .unwrap();
    if let Some(text) = settings_toml {
        let dir = home.path().join(".config/agmen");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("settings.toml"), text).unwrap();
    }
    home
}

#[test]
fn resolve_follows_zed_by_default() {
    let home = home_with(None);
    let a = resolve_paths(&home.path().join(".config"), &ext_dir(), SystemMode::Dark);
    assert_eq!(a.source, AppearanceSource::Zed);
    assert!(close(a.theme.background, hex("#1d2021")));
    assert_eq!(a.fonts.code_family.as_ref(), "CaskaydiaCove Nerd Font Mono");
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
}

#[test]
fn unknown_theme_falls_back() {
    let home = home_with(None);
    let zed = home.path().join(".config/zed/settings.json");
    fs::write(&zed, r#"{"theme": "Missing Theme"}"#).unwrap();
    let a = resolve_paths(&home.path().join(".config"), &ext_dir(), SystemMode::Dark);
    assert_eq!(a.source, AppearanceSource::Builtin);
    assert!(close(
        a.theme.background,
        gruvbox_material_dark_hard().background
    ));
    assert!(
        a.warnings.iter().any(|w| w.contains("Missing Theme")),
        "{:?}",
        a.warnings
    );
}

#[test]
fn no_zed_uses_builtin_without_error() {
    let home = tempfile::tempdir().unwrap();
    let a = resolve_paths(&home.path().join(".config"), &ext_dir(), SystemMode::Dark);
    assert_eq!(a.source, AppearanceSource::Builtin);
    assert_eq!(a.icon_theme, IconThemeChoice::System);
}

#[test]
fn follow_zed_off_uses_agmen_theme() {
    let home = home_with(Some(
        "follow_zed = false\ntheme = \"Partial Dark\"\n\n[fonts]\ncode_family = \"Menlo\"\ncode_size = 14\n",
    ));
    let a = resolve_paths(&home.path().join(".config"), &ext_dir(), SystemMode::Dark);
    assert_eq!(a.source, AppearanceSource::Agmen);
    assert!(close(a.theme.background, hex("#101010")));
    assert_eq!(a.fonts.code_family.as_ref(), "Menlo");
    assert_eq!(a.fonts.code_size, gpui::px(14.0));
}

#[test]
fn bad_agmen_settings_warns_and_follows_zed() {
    let home = home_with(Some("follow_zed = \"maybe\"\n"));
    let a = resolve_paths(&home.path().join(".config"), &ext_dir(), SystemMode::Dark);
    assert_eq!(a.source, AppearanceSource::Zed);
    assert!(
        a.warnings.iter().any(|w| w.contains("settings.toml")),
        "{:?}",
        a.warnings
    );
}

#[test]
fn agmen_settings_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agmen/settings.toml");
    let mut s = AgmenSettings::default();
    assert!(s.follow_zed);
    s.follow_zed = false;
    s.theme = Some("One Dark".into());
    s.mode = Mode::Dark;
    s.icon_theme = IconThemeChoice::Lucide;
    s.density = Density::Compact;
    s.fonts.ui_size = Some(14.0);
    s.save(&path).unwrap();
    assert_eq!(AgmenSettings::load(&path).unwrap(), s);
}
