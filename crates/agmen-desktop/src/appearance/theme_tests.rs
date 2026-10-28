use std::fs;
use std::path::{Path, PathBuf};

use gpui::{Hsla, Rgba};

use super::fallback::gruvbox_material_dark_hard;
use super::settings::{AgmenSettings, Density, DesktopSettings, IconThemeChoice};
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
fn fallback_matches_the_sample_themes_ui_keys() {
    // V76: the fallback is Andy's theme, so each UI key the sample sets
    // (taken from that theme) must resolve to the fallback's own value.
    let t = load_theme(&zed_dir(), &ext_dir(), "Gruvbox Material Dark Hard").unwrap();
    let f = gruvbox_material_dark_hard();
    let pairs = [
        ("background", t.background, f.background),
        ("surface", t.surface, f.surface),
        ("elevated_surface", t.elevated_surface, f.elevated_surface),
        ("element", t.element, f.element),
        ("element_selected", t.element_selected, f.element_selected),
        ("element_hover", t.element_hover, f.element_hover),
        ("border", t.border, f.border),
        ("border_variant", t.border_variant, f.border_variant),
        ("text", t.text, f.text),
        ("text_muted", t.text_muted, f.text_muted),
        ("text_placeholder", t.text_placeholder, f.text_placeholder),
        ("text_accent", t.text_accent, f.text_accent),
        ("icon_muted", t.icon_muted, f.icon_muted),
        (
            "editor_background",
            t.editor_background,
            f.editor_background,
        ),
        ("line_number", t.line_number, f.line_number),
        ("error", t.error, f.error),
        ("warning", t.warning, f.warning),
        ("success", t.success, f.success),
        ("info", t.info, f.info),
        ("created", t.created, f.created),
        ("modified", t.modified, f.modified),
        ("deleted", t.deleted, f.deleted),
        ("selection", t.selection, f.selection),
    ];
    for (key, theme, fallback) in pairs {
        assert!(close(theme, fallback), "{key}: {theme:?} vs {fallback:?}");
    }
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
fn terminal_colors_fall_back_to_text_and_background() {
    // Task 14: `terminal.foreground` when the theme has it, else the theme's
    // own text; `terminal.background` likewise falls back to `background`.
    let t = load_theme(&zed_dir(), &ext_dir(), "Partial Dark").unwrap();
    assert!(close(t.terminal_foreground, hex("#aabbcc")));
    assert!(close(t.terminal_background, hex("#101010")));
    let fallback = gruvbox_material_dark_hard();
    assert!(close(fallback.terminal_foreground, hex("#d8bf94")));
    assert!(close(fallback.terminal_background, hex("#1d2021")));
}

#[test]
fn highlighted_line_reads_zeds_editor_key() {
    // C313: a ref's highlighted lines take the theme's
    // `editor.highlighted_line.background`, else the fallback's.
    let t = load_theme(&zed_dir(), &ext_dir(), "Partial Dark").unwrap();
    assert!(close(t.highlighted_line, hex("#334455")));
    assert!(!close(
        gruvbox_material_dark_hard().highlighted_line,
        hex("#334455")
    ));
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
fn last_project_keeps_the_appearance() {
    // Task 7: a live launch writes its project to the [desktop] table and
    // leaves every other table as it was.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agmen/settings.toml");
    assert_eq!(
        DesktopSettings::load(&path).unwrap(),
        DesktopSettings::default()
    );
    let look = AgmenSettings {
        density: Density::Compact,
        ..AgmenSettings::default()
    };
    look.save(&path).unwrap();
    fs::write(
        &path,
        fs::read_to_string(&path).unwrap() + "\n[desktop]\nadded_later = 1\n",
    )
    .unwrap();
    let project: agmen_proto::ProjectId = "project1".parse().unwrap();
    DesktopSettings {
        last_project: Some(project.clone()),
        ..DesktopSettings::default()
    }
    .save(&path)
    .unwrap();
    assert_eq!(
        DesktopSettings::load(&path).unwrap().last_project,
        Some(project)
    );
    assert_eq!(AgmenSettings::load(&path).unwrap(), look);
    let table: toml::Table = fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(table["desktop"]["added_later"].as_integer(), Some(1));
    // A value that is not a project id is an error, not a project.
    fs::write(&path, "[desktop]\nlast_project = \"../etc\"\n").unwrap();
    assert!(DesktopSettings::load(&path).is_err());
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

#[test]
fn theme_overrides_replace_keys() {
    let dir = tempfile::tempdir().unwrap();
    let zed = dir.path().join("zed");
    fs::create_dir_all(zed.join("themes")).unwrap();
    fs::copy(
        zed_dir().join("themes/gruvbox-sample.json"),
        zed.join("themes/g.json"),
    )
    .unwrap();
    fs::write(
        zed.join("settings.json"),
        r##"{
            "theme": "Gruvbox Material Dark Hard",
            "experimental.theme_overrides": { "border": "#111111" },
            "theme_overrides": {
                "Gruvbox Material Dark Hard": { "background": "#222222" },
                "Some Other Theme": { "background": "#333333" }
            }
        }"##,
    )
    .unwrap();
    let a = resolve_paths(dir.path(), &ext_dir(), SystemMode::Dark);
    assert!(
        close(a.theme.background, hex("#222222")),
        "the per-theme override wins"
    );
    assert!(
        close(a.theme.border, hex("#111111")),
        "the global override applies"
    );
    assert!(
        close(a.theme.text, hex("#d8bf94")),
        "other keys stay the theme's"
    );
}

#[test]
fn zed_syntax_map_is_read() {
    let t = load_theme(&zed_dir(), &ext_dir(), "Gruvbox Material Dark Hard").unwrap();
    assert!(close(t.syntax["keyword"], hex("#ed675f")));
    let fallback = gruvbox_material_dark_hard();
    assert!(close(fallback.syntax["string"], hex("#afbf5c")));
}

#[test]
fn marked_theme_is_built_in() {
    let t = load_theme(&zed_dir(), &ext_dir(), super::marked::MARKED_THEME).unwrap();
    assert!(close(t.text_accent, hex("#68acd4")));
}

#[test]
fn stray_files_beside_extensions_are_not_errors() {
    // Review C109: a .DS_Store among the extension folders is not a folder.
    let dir = tempfile::tempdir().unwrap();
    let ext = dir.path().join("ext");
    fs::create_dir_all(ext.join("some-theme/themes")).unwrap();
    fs::write(ext.join(".DS_Store"), b"junk").unwrap();
    let err = load_theme(&zed_dir(), &ext, "Missing").unwrap_err();
    assert!(!err.to_string().contains(".DS_Store"), "{err}");
}

#[test]
fn unknown_font_keys_survive_save() {
    // Review C110.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(
        &path,
        "[appearance.fonts]\nui_size = 14\nfuture_key = true\n",
    )
    .unwrap();
    let mut s = AgmenSettings::load(&path).unwrap();
    s.fonts.ui_size = Some(16.0);
    s.save(&path).unwrap();
    let table: toml::Table = fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(
        table["appearance"]["fonts"]["future_key"].as_bool(),
        Some(true)
    );
    assert_eq!(
        table["appearance"]["fonts"]["ui_size"].as_float(),
        Some(16.0)
    );
}

#[test]
fn owned_font_keys_match_the_struct() {
    let all = super::FontPrefs {
        ui_family: Some("a".into()),
        ui_size: Some(1.0),
        code_family: Some("b".into()),
        code_size: Some(1.0),
        code_ligatures: Some(true),
        terminal_family: Some("c".into()),
        terminal_size: Some(1.0),
    };
    let value = toml::Value::try_from(all).unwrap();
    let mut keys: Vec<String> = value.as_table().unwrap().keys().cloned().collect();
    keys.sort();
    let mut owned: Vec<String> = super::settings::OWNED_FONT_KEYS
        .iter()
        .map(|k| (*k).to_owned())
        .collect();
    owned.sort();
    assert_eq!(keys, owned);
}

#[test]
fn overrides_merge_like_zed() {
    // Review C121 and C127: Zed applies experimental overrides, then the
    // per-theme ones; syntax merges per highlight name and players per index.
    let dir = tempfile::tempdir().unwrap();
    let zed = dir.path().join("zed");
    fs::create_dir_all(zed.join("themes")).unwrap();
    fs::copy(
        zed_dir().join("themes/gruvbox-sample.json"),
        zed.join("themes/g.json"),
    )
    .unwrap();
    fs::write(
        zed.join("settings.json"),
        r##"{
            "theme": "Gruvbox Material Dark Hard",
            "experimental.theme_overrides": {
                "border": "#111111",
                "syntax": { "string": { "color": "#010101" } }
            },
            "theme_overrides": {
                "Gruvbox Material Dark Hard": {
                    "border": "#222222",
                    "players": [{}, { "cursor": "#020202" }]
                }
            }
        }"##,
    )
    .unwrap();
    let a = resolve_paths(dir.path(), &ext_dir(), SystemMode::Dark);
    assert!(
        close(a.theme.border, hex("#222222")),
        "per-theme beats global on one key"
    );
    assert!(
        close(a.theme.syntax["string"], hex("#010101")),
        "the overridden name"
    );
    assert!(
        close(a.theme.syntax["keyword"], hex("#ed675f")),
        "other names stay the theme's"
    );
    assert!(
        close(a.theme.syntax["type"], hex("#123123")),
        "not the fallback's #dda752"
    );
    assert!(
        close(a.theme.players[0], hex("#72b9a9")),
        "an empty player keeps its colors"
    );
    assert!(
        close(a.theme.players[1], hex("#020202")),
        "one field of one player"
    );
}

#[test]
fn overrides_apply_to_the_marked_theme() {
    // Review C125.
    let mut overrides = serde_json::Map::new();
    overrides.insert("border".into(), serde_json::json!("#333333"));
    let mut warnings = Vec::new();
    let t = super::theme::load_theme_with(
        &super::theme_dirs(&fixtures(), &ext_dir()),
        super::marked::MARKED_THEME,
        &overrides,
        &mut warnings,
    )
    .unwrap();
    assert!(close(t.border, hex("#333333")));
}

#[test]
fn bad_override_values_keep_the_themes_own() {
    // Review C142: Zed keeps the theme's value for an unparsable override
    // and treats "accents": [] as no change.
    let mut style: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        r##"{"border": "#123456", "text": "#654321", "accents": ["#abcdef"]}"##,
    )
    .unwrap();
    let overrides = serde_json::from_str(
        r##"{"border": "not a color", "text": null, "accents": [], "background": "#010101"}"##,
    )
    .unwrap();
    super::theme::merge_style(&mut style, &overrides);
    assert_eq!(style["border"], "#123456");
    assert_eq!(style["text"], "#654321");
    assert_eq!(style["accents"][0], "#abcdef");
    assert_eq!(style["background"], "#010101", "good values still apply");
}

#[test]
fn bad_nested_override_values_keep_the_themes_own() {
    // Review C148: the C142 rule holds inside syntax entries and players.
    let mut style: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        r##"{"syntax": {"comment": {"color": "#111111", "font_style": "italic"}},
             "players": [{"cursor": "#222222", "selection": "#333333"}]}"##,
    )
    .unwrap();
    let overrides = serde_json::from_str(
        r##"{"syntax": {"comment": {"color": null, "font_style": "normal"}},
             "players": [{"cursor": "not a color", "selection": "#444444"}]}"##,
    )
    .unwrap();
    super::theme::merge_style(&mut style, &overrides);
    assert_eq!(style["syntax"]["comment"]["color"], "#111111");
    assert_eq!(style["syntax"]["comment"]["font_style"], "normal");
    assert_eq!(style["players"][0]["cursor"], "#222222");
    assert_eq!(style["players"][0]["selection"], "#444444");
}

/// Final review fix 4: a settings file the person wrote by hand, with
/// comments and its own key order.
const HAND_WRITTEN: &str = "# agmen settings\n\
[desktop]\n\
# opened last\n\
last_project = \"project1\" # keep\n\
added_later = 1\n\
\n\
[appearance]\n\
density = \"compact\"\n";

#[test]
fn desktop_settings_unchanged_leave_the_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(&path, HAND_WRITTEN).unwrap();
    DesktopSettings {
        last_project: Some("project1".parse().unwrap()),
        ..DesktopSettings::default()
    }
    .save(&path)
    .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), HAND_WRITTEN);
}

#[test]
fn desktop_settings_change_keeps_comments_and_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    fs::write(&path, HAND_WRITTEN).unwrap();
    let project: agmen_proto::ProjectId = "project2".parse().unwrap();
    DesktopSettings {
        last_project: Some(project.clone()),
        ..DesktopSettings::default()
    }
    .save(&path)
    .unwrap();
    assert_eq!(
        DesktopSettings::load(&path).unwrap().last_project,
        Some(project)
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        HAND_WRITTEN.replace("\"project1\"", "\"project2\"")
    );
}

#[test]
fn desktop_quit_preference_and_optional_login_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let mut settings = DesktopSettings::load(&path).unwrap();
    assert_eq!(settings.quit_keep_running, None);
    assert!(!settings.start_at_login);
    for keep in [true, false] {
        settings.quit_keep_running = Some(keep);
        settings.start_at_login = true;
        settings.save(&path).unwrap();
        assert_eq!(DesktopSettings::load(&path).unwrap(), settings);
    }
}

#[test]
fn desktop_recents_and_quit_settings_survive_each_others_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let mut settings = DesktopSettings::load(&path).unwrap();
    settings.quit_keep_running = Some(false);
    settings.start_at_login = true;
    settings.save(&path).unwrap();

    let mut settings = DesktopSettings::load(&path).unwrap();
    settings.last_project = Some("project1".parse().unwrap());
    settings.recent_projects = vec![dir.path().canonicalize().unwrap()];
    settings.save(&path).unwrap();
    assert_eq!(DesktopSettings::load(&path).unwrap(), settings);

    let mut settings = DesktopSettings::load(&path).unwrap();
    settings.quit_keep_running = None;
    settings.start_at_login = false;
    settings.save(&path).unwrap();
    assert_eq!(DesktopSettings::load(&path).unwrap(), settings);
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("quit_keep_running"));
    assert!(!text.contains("start_at_login"));
}
