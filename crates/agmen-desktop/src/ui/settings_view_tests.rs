use std::cell::RefCell;
use std::fs;
use std::rc::Rc;

use gpui::{Entity, Hsla, Rgba, TestAppContext, VisualTestContext};

use super::*;
use crate::appearance::{AgmenSettings, Appearance, AppearanceSource, Density, resolve_paths};
use crate::ui::test_support::{click, init, redraw};

fn hex(s: &str) -> Hsla {
    Rgba::try_from(s).unwrap().into()
}

/// A throwaway home: `.config/zed`, `.config/agmen` and an empty Zed
/// extensions folder, never the real ones.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let home = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::create_dir_all(home.config().join("zed/themes")).unwrap();
        fs::create_dir_all(home.extensions()).unwrap();
        fs::write(
            home.config().join("zed/themes/fixture.json"),
            r##"{"name": "Fixture", "author": "agmen tests", "themes": [
                {"name": "Fixture Dark", "appearance": "dark",
                 "style": {"background": "#102030"}},
                {"name": "Fixture Dark Two", "appearance": "dark",
                 "style": {"background": "#405060"}},
                {"name": "Fixture Light", "appearance": "light",
                 "style": {"background": "#f0f0f0"}}]}"##,
        )
        .unwrap();
        home
    }

    fn config(&self) -> std::path::PathBuf {
        self.dir.path().join(".config")
    }

    fn extensions(&self) -> std::path::PathBuf {
        self.dir.path().join("zed-extensions")
    }

    fn settings_path(&self) -> std::path::PathBuf {
        self.config().join("agmen/settings.toml")
    }

    fn write_settings(&self, text: &str) {
        fs::create_dir_all(self.config().join("agmen")).unwrap();
        fs::write(self.settings_path(), text).unwrap();
    }

    fn saved(&self) -> AgmenSettings {
        AgmenSettings::load(&self.settings_path()).unwrap()
    }

    fn sources(&self) -> SettingsSources {
        SettingsSources {
            config: self.config(),
            extensions: self.extensions(),
            // A closed port: these tests never browse, and never reach the API.
            market: "http://127.0.0.1:9".into(),
        }
    }
}

/// Opens Settings on `home` with the appearance resolved from it, as the
/// app does at start.
fn open<'a>(
    cx: &'a mut TestAppContext,
    home: &Home,
) -> (Entity<SettingsView>, &'a mut VisualTestContext) {
    init(cx);
    let sources = home.sources();
    cx.update(|cx| {
        cx.set_global(resolve_paths(
            &sources.config,
            &sources.extensions,
            crate::appearance::SystemMode::Dark,
        ));
    });
    let (view, cx) = cx.add_window_view(|_, cx| SettingsView::new(sources, cx));
    redraw(cx);
    (view, cx)
}

fn appearance(cx: &mut VisualTestContext) -> Appearance {
    cx.update(|_, cx| cx.global::<Appearance>().clone())
}

#[gpui::test]
fn toggling_follow_zed_off_keeps_current_values(cx: &mut TestAppContext) {
    let home = Home::new();
    fs::write(
        home.config().join("zed/settings.json"),
        r#"{
            // Zed's own file: JSONC.
            "theme": "Fixture Dark",
            "ui_font_size": 15,
            "buffer_font_family": "Lilex",
            "buffer_font_size": 13,
        }"#,
    )
    .unwrap();
    let (_, cx) = open(cx, &home);
    let before = appearance(cx);
    assert_eq!(before.source, AppearanceSource::Zed);

    click(cx, "follow-zed");

    let saved = home.saved();
    assert!(!saved.follow_zed);
    assert_eq!(saved.theme.as_deref(), Some("Fixture Dark"));
    assert_eq!(saved.fonts.ui_size, Some(15.));
    assert_eq!(saved.fonts.code_family.as_deref(), Some("Lilex"));
    assert_eq!(saved.fonts.code_size, Some(13.));
    let after = appearance(cx);
    assert_eq!(after.source, AppearanceSource::Agmen);
    assert_eq!(after.theme, before.theme);
    assert_eq!(after.fonts, before.fonts);
    assert_eq!(after.theme_name, "Fixture Dark");
}

#[gpui::test]
fn size_stepper_bounds(cx: &mut TestAppContext) {
    let home = Home::new();
    home.write_settings(
        "[appearance]\nfollow_zed = false\ntheme = \"Fixture Dark\"\n\
         [appearance.fonts]\nui_size = 72.0\ncode_size = 6.0\n",
    );
    let (_, cx) = open(cx, &home);

    click(cx, "ui-size:increment");
    click(cx, "code-size:decrement");
    assert_eq!(home.saved().fonts.ui_size, Some(72.), "72 is the top");
    assert_eq!(home.saved().fonts.code_size, Some(6.), "6 is the bottom");

    click(cx, "ui-size:decrement");
    click(cx, "code-size:increment");
    assert_eq!(home.saved().fonts.ui_size, Some(71.));
    assert_eq!(home.saved().fonts.code_size, Some(7.));
    assert_eq!(appearance(cx).fonts.ui_size, gpui::px(71.));
}

#[gpui::test]
fn change_saves_settings_toml(cx: &mut TestAppContext) {
    let home = Home::new();
    home.write_settings("[tui]\nkeys = \"vi\"\n");
    let (_, cx) = open(cx, &home);
    assert_eq!(appearance(cx).density, Density::Comfortable);

    click(cx, "density:1");

    let text = fs::read_to_string(home.settings_path()).unwrap();
    assert!(text.contains("keys = \"vi\""), "other tables stay: {text}");
    let saved = home.saved();
    assert_eq!(saved.density, Density::Compact);
    // Ruling D14: density is agmen's own setting, so Follow Zed stays on.
    assert!(saved.follow_zed);
    assert_eq!(saved.theme, None, "nothing copied from Zed");
    assert_eq!(appearance(cx).density, Density::Compact);

    // The light, dark or system mode is agmen's own setting too.
    click(cx, "theme-mode:0");
    let saved = home.saved();
    assert_eq!(saved.mode, crate::appearance::Mode::Light);
    assert!(saved.follow_zed);
}

#[gpui::test]
fn theme_change_repaints(cx: &mut TestAppContext) {
    let home = Home::new();
    home.write_settings(
        "[appearance]\nfollow_zed = false\ntheme = \"Fixture Dark\"\nmode = \"dark\"\n",
    );
    let (view, cx) = open(cx, &home);
    assert_eq!(appearance(cx).theme.background, hex("#102030"));
    let options = view.read_with(cx, |view, cx| view.theme_options(cx));
    assert_eq!(
        options,
        [
            crate::appearance::BUILTIN_THEME,
            "Wallpaper Adaptive (Balanced)",
            "Fixture Dark",
            "Fixture Dark Two"
        ],
        "dark mode lists dark themes only"
    );

    click(cx, "theme");
    click(cx, "theme:3");

    let after = appearance(cx);
    assert_eq!(after.theme.background, hex("#405060"));
    assert_eq!(after.theme_name, "Fixture Dark Two");
    assert_eq!(home.saved().theme.as_deref(), Some("Fixture Dark Two"));
}

#[gpui::test]
fn light_mode_lists_light_themes(cx: &mut TestAppContext) {
    let home = Home::new();
    home.write_settings("[appearance]\nfollow_zed = false\ntheme = \"Fixture Dark\"\n");
    let (view, cx) = open(cx, &home);
    click(cx, "theme-mode:0");
    assert_eq!(home.saved().mode, crate::appearance::Mode::Light);
    let options = view.read_with(cx, |view, cx| view.theme_options(cx));
    // The theme in use stays listed, so the box can show it.
    assert_eq!(options, ["Fixture Dark", "Fixture Light"]);
}

#[gpui::test]
fn about_shows_lucide_notice(cx: &mut TestAppContext) {
    let home = Home::new();
    let (view, cx) = open(cx, &home);
    assert!(cx.debug_bounds("about:lucide").is_none());

    click(cx, "settings-page:about");

    assert!(cx.debug_bounds("about:lucide").is_some());
    view.read_with(cx, |view, _| assert_eq!(view.page, Page::About));
    let lucide = NOTICES
        .iter()
        .find(|(name, _)| name.contains("Lucide"))
        .unwrap();
    assert_eq!(lucide.1, crate::assets::LUCIDE_LICENSE);
    assert!(lucide.1.contains("ISC License"));
}

#[gpui::test]
fn open_settings_toml_names_the_file(cx: &mut TestAppContext) {
    let home = Home::new();
    let (view, cx) = open(cx, &home);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &SettingsEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });

    click(cx, "open-settings-toml");

    assert_eq!(
        *seen.borrow(),
        [SettingsEvent::OpenFile(home.settings_path())]
    );
}

#[gpui::test]
fn unbuilt_pages_say_they_come_with_agmend(cx: &mut TestAppContext) {
    let home = Home::new();
    let (_, cx) = open(cx, &home);
    click(cx, "settings-page:listener");
    assert!(cx.debug_bounds("settings-placeholder").is_some());
    assert!(cx.debug_bounds("follow-zed").is_none());
}

#[test]
fn about_carries_every_linked_crates_license() {
    // Review C198: the Zed crates (Apache-2.0) and zed-font-kit and zed-scap
    // (MIT) need their license texts in the binary too.
    let has = |name: &str, text: &str| {
        NOTICES
            .iter()
            .any(|(title, body)| title.contains(name) && body.contains(text))
    };
    assert!(has("gpui", "Apache License"), "Zed's crates");
    assert!(has("font-kit", "Permission is hereby granted"));
    assert!(has("scap", "MIT License"));
}
