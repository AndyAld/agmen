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
        Some(ThemeChoice::Named("Gruvbox Material Dark Hard".into()))
    );
    assert_eq!(
        s.icon_theme,
        Some(ThemeChoice::Named("Material Icon Theme".into()))
    );
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
    let t = load_theme(&zed_dir(), &ext_dir(), "Gruvbox Material Dark Hard").unwrap();
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
    assert!(close(t.role_color("planner"), hex("#72b9a9")));
    assert!(close(t.role_color("coordinator"), hex("#da7f98")));
    assert!(close(t.role_color("researcher"), hex("#82be78")));
    assert!(close(t.role_color("unknown-role"), t.text_muted));
    // The mockup's #a89984, within the mix's rounding.
    let listener = t.role_color("listener").to_rgb();
    let want = hex("#a89984").to_rgb();
    assert!((listener.r - want.r).abs() < 6.0 / 255.0, "{listener:?}");
    assert!((listener.g - want.g).abs() < 8.0 / 255.0, "{listener:?}");
    assert!((listener.b - want.b).abs() < 8.0 / 255.0, "{listener:?}");
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
    assert!(
        a.warnings.iter().any(|w| w.contains("Zed settings")),
        "{:?}",
        a.warnings
    );
}

#[test]
fn zed_without_theme_key_says_why() {
    let home = home_with(None);
    fs::write(home.path().join(".config/zed/settings.json"), "{}").unwrap();
    let a = resolve_paths(&home.path().join(".config"), &ext_dir(), SystemMode::Dark);
    assert_eq!(a.source, AppearanceSource::Builtin);
    assert!(
        a.warnings.iter().any(|w| w.contains("no theme")),
        "{:?}",
        a.warnings
    );
}

#[test]
fn follow_zed_off_without_theme_says_why() {
    let home = home_with(Some("[appearance]\nfollow_zed = false\n"));
    let a = resolve_paths(&home.path().join(".config"), &ext_dir(), SystemMode::Dark);
    assert_eq!(a.source, AppearanceSource::Builtin);
    assert!(
        a.warnings.iter().any(|w| w.contains("no theme")),
        "{:?}",
        a.warnings
    );
}

#[test]
fn no_zed_uses_agmen_theme_and_icons() {
    let home = tempfile::tempdir().unwrap();
    let agmen = home.path().join(".config/agmen");
    fs::create_dir_all(&agmen).unwrap();
    fs::write(
        agmen.join("settings.toml"),
        "[appearance]\ntheme = \"Partial Dark\"\n\n[appearance.icon_theme]\nkind = \"lucide\"\n",
    )
    .unwrap();
    let a = resolve_paths(&home.path().join(".config"), &ext_dir(), SystemMode::Dark);
    assert_eq!(a.source, AppearanceSource::Agmen);
    assert!(close(a.theme.background, hex("#101010")));
    assert_eq!(a.icon_theme, IconThemeChoice::Lucide);
}

#[test]
fn icon_theme_object_follows_system_mode() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("settings.json"),
        r#"{"icon_theme": {"mode": "system", "light": "Light Icons", "dark": "Dark Icons"}}"#,
    )
    .unwrap();
    let s = read_zed_settings(dir.path()).unwrap();
    let icons = s.icon_theme.unwrap();
    assert_eq!(icons.name_for(SystemMode::Light), "Light Icons");
    assert_eq!(icons.name_for(SystemMode::Dark), "Dark Icons");
}

#[test]
fn players_without_cursor_keep_their_slot() {
    let dir = tempfile::tempdir().unwrap();
    let themes = dir.path().join("themes");
    fs::create_dir_all(&themes).unwrap();
    fs::write(
        themes.join("p.json"),
        r##"{"themes":[{"name":"P","appearance":"dark","style":{"players":[
            {"cursor":"#111111"},{"selection":"#222222"},{"cursor":"#333333"}]}}]}"##,
    )
    .unwrap();
    let t = load_theme(dir.path(), &ext_dir(), "P").unwrap();
    let fallback = gruvbox_material_dark_hard();
    assert!(close(t.players[0], hex("#111111")));
    assert!(close(t.players[1], fallback.players[1]));
    assert!(close(t.players[2], hex("#333333")));
}

#[test]
fn unparsable_theme_files_are_named() {
    let dir = tempfile::tempdir().unwrap();
    let themes = dir.path().join("themes");
    fs::create_dir_all(&themes).unwrap();
    fs::write(themes.join("broken.json"), "{ not json").unwrap();
    let err = load_theme(dir.path(), &ext_dir(), "Anything").unwrap_err();
    assert!(err.to_string().contains("broken.json"), "{err}");
    assert!(
        err.to_string().contains("line 1"),
        "the parse message is kept: {err}"
    );
}

#[test]
fn follow_zed_off_uses_agmen_theme() {
    let home = home_with(Some(
        "[appearance]\nfollow_zed = false\ntheme = \"Partial Dark\"\n\n[appearance.fonts]\ncode_family = \"Menlo\"\ncode_size = 14\n",
    ));
    let a = resolve_paths(&home.path().join(".config"), &ext_dir(), SystemMode::Dark);
    assert_eq!(a.source, AppearanceSource::Agmen);
    assert!(close(a.theme.background, hex("#101010")));
    assert_eq!(a.fonts.code_family.as_ref(), "Menlo");
    assert_eq!(a.fonts.code_size, gpui::px(14.0));
}

#[test]
fn bad_agmen_settings_warns_and_follows_zed() {
    let home = home_with(Some("[appearance]\nfollow_zed = \"maybe\"\n"));
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

#[test]
fn settings_keep_other_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(
        &path,
        "[tui]\nicons = \"nerd\"\n\n[appearance]\nfollow_zed = true\nadded_later = 1\n",
    )
    .unwrap();
    let mut s = AgmenSettings::load(&path).unwrap();
    s.density = Density::Compact;
    s.save(&path).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    let table: toml::Table = text.parse().unwrap();
    assert_eq!(table["tui"]["icons"].as_str(), Some("nerd"), "{text}");
    assert_eq!(
        table["appearance"]["density"].as_str(),
        Some("compact"),
        "{text}"
    );
    assert_eq!(
        table["appearance"]["added_later"].as_integer(),
        Some(1),
        "{text}"
    );
    assert_eq!(
        AgmenSettings::load(&path).unwrap().density,
        Density::Compact
    );
}

#[test]
fn settings_refuse_to_overwrite_a_broken_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(&path, "this is = = not toml").unwrap();
    assert!(AgmenSettings::default().save(&path).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "this is = = not toml");
}

#[test]
fn settings_over_size_cap_fail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(&path, "#".repeat(5 * 1024 * 1024)).unwrap();
    assert!(AgmenSettings::load(&path).is_err());
}

#[test]
fn unset_fonts_default_to_zeds() {
    let a = super::Appearance::builtin();
    assert_eq!(a.fonts.ui_family.as_ref(), ".ZedSans");
    assert_eq!(a.fonts.ui_size, gpui::px(16.0));
    assert_eq!(a.fonts.code_family.as_ref(), ".ZedMono");
    assert_eq!(a.fonts.code_size, gpui::px(15.0));
    assert_eq!(a.fonts.terminal_size, gpui::px(15.0));
}

#[test]
fn zed_fonts_follow_settings() {
    let home = home_with(None);
    let a = resolve_paths(&home.path().join(".config"), &ext_dir(), SystemMode::Dark);
    assert_eq!(
        a.fonts.ui_family.as_ref(),
        ".ZedSans",
        "Andy's settings set no UI font"
    );
    assert_eq!(a.fonts.ui_size, gpui::px(15.0));
    assert_eq!(a.fonts.code_size, gpui::px(13.0));
    assert_eq!(a.fonts.terminal_size, gpui::px(13.0));
}

#[test]
fn clearing_a_setting_removes_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(
        &path,
        "[appearance]\ntheme = \"One Dark\"\nfollow_zed = false\n",
    )
    .unwrap();
    let mut s = AgmenSettings::load(&path).unwrap();
    s.theme = None;
    s.save(&path).unwrap();
    assert_eq!(AgmenSettings::load(&path).unwrap().theme, None);
}

#[test]
fn owned_keys_match_the_struct() {
    let value = toml::Value::try_from(AgmenSettings {
        theme: Some("x".into()),
        ..AgmenSettings::default()
    })
    .unwrap();
    let mut keys: Vec<String> = value.as_table().unwrap().keys().cloned().collect();
    keys.sort();
    let mut owned: Vec<String> = super::settings::OWNED_KEYS
        .iter()
        .map(|k| (*k).to_owned())
        .collect();
    owned.sort();
    assert_eq!(
        keys, owned,
        "a new AgmenSettings field must be added to OWNED_KEYS"
    );
}
