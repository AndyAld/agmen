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
    // Review C198: the Zed crates (Apache-2.0) and zed-font-kit (MIT) need
    // their license texts in the binary too.
    let has = |name: &str, text: &str| {
        NOTICES
            .iter()
            .any(|(title, body)| title.contains(name) && body.contains(text))
    };
    assert!(has("gpui", "Apache License"), "Zed's crates");
    assert!(has("font-kit", "Permission is hereby granted"));
    // Review C214: zed-scap's LICENSE is not MIT (it limits use by Valorant
    // rank), and agmen does not build it, so About must not list it.
    assert!(NOTICES.iter().all(|(title, _)| !title.contains("scap")));
}

#[test]
fn zed_extension_themes_show_author_and_repository() {
    // Review C202: read from the extension's own extension.toml. Zed keeps
    // no license file in its installed folders, so the line says Zed has it.
    let home = Home::new();
    let nord = home.extensions().join("nord");
    fs::create_dir_all(nord.join("themes")).unwrap();
    fs::write(
        nord.join("extension.toml"),
        "id = \"nord\"\nname = \"Nord\"\nversion = \"1.0.0\"\n\
         authors = [\"Nord Folks <nord@example.org>\"]\n\
         repository = \"https://github.com/example/nord\"\n",
    )
    .unwrap();
    fs::write(
        nord.join("themes/nord.json"),
        r#"{"themes": [{"name": "Nord", "appearance": "dark", "style": {}}]}"#,
    )
    .unwrap();
    let dirs = theme_dirs(&home.config(), &home.extensions());
    let (themes, _) = installed_themes(&dirs);
    let entry = themes.iter().find(|t| t.name == "Nord").unwrap();

    let detail = rows::installed_detail(entry);
    assert!(
        detail.contains("by Nord Folks <nord@example.org>"),
        "{detail}"
    );
    assert!(
        detail.contains("https://github.com/example/nord"),
        "{detail}"
    );
    assert!(detail.contains("installed in Zed (nord)"), "{detail}");
    assert!(detail.contains("license kept by Zed"), "{detail}");
}

fn offered(id: &str) -> ThemeExtension {
    ThemeExtension {
        id: id.into(),
        name: id.into(),
        version: "1.0.0".into(),
        description: None,
        authors: Vec::new(),
        repository: None,
        download_count: 0,
    }
}

fn status(view: &Entity<SettingsView>, cx: &mut VisualTestContext) -> BrowseStatus {
    view.read_with(cx, |view, _| view.browse.as_ref().unwrap().status.clone())
}

#[gpui::test]
fn stale_search_does_not_overwrite_newer(cx: &mut TestAppContext) {
    // Review C209: answers can come back out of order.
    let home = Home::new();
    let (view, cx) = open(cx, &home);
    click(cx, "browse-themes");
    let second = view.update(cx, |view, cx| {
        let first = view.search_generation;
        view.search("nord".into(), cx);
        let second = view.search_generation;
        assert_ne!(first, second);

        view.searched(second, Ok(vec![offered("nord")]), cx);
        view.searched(first, Ok(vec![offered("everything")]), cx);
        view.searched(first, Err("late failure".into()), cx);

        let browse = view.browse.as_ref().unwrap();
        assert_eq!(browse.results, [offered("nord")]);
        assert_eq!(browse.status, BrowseStatus::Ready);
        second
    });

    // Closed and opened again: a search from before is still older.
    view.update(cx, |view, _| view.browse = None);
    click(cx, "browse-themes");
    view.update(cx, |view, cx| {
        let newest = view.search_generation;
        assert_ne!(newest, second);
        view.searched(second, Ok(vec![offered("old panel")]), cx);
        assert!(view.browse.as_ref().unwrap().results.is_empty());
    });
}

#[gpui::test]
fn shadowed_install_warns(cx: &mut TestAppContext) {
    // Review C210: "Fixture Dark" is already in ~/.config/zed/themes, so the
    // installed copy is never listed; say so instead of a silent Ready.
    let home = Home::new();
    home.write_settings(
        "[appearance]\nfollow_zed = false\ntheme = \"Fixture Dark Two\"\nmode = \"dark\"\n",
    );
    let (view, cx) = open(cx, &home);
    click(cx, "browse-themes");
    let installed = InstalledTheme {
        id: "fixture-copy".into(),
        ..InstalledTheme::default()
    };
    view.update(cx, |view, cx| {
        view.installed(&installed, &["Fixture Dark".to_owned()], cx);
    });

    // C236: the install itself worked, so this is a note, not a failure.
    match status(&view, cx) {
        BrowseStatus::Installed(text) => {
            assert!(text.contains("Fixture Dark"), "{text}");
            assert!(text.contains("your Zed themes folder"), "{text}");
        }
        other => panic!("expected a warning, got {other:?}"),
    }
    assert_eq!(home.saved().theme.as_deref(), Some("Fixture Dark Two"));
}

#[gpui::test]
fn zed_credit_links_to_the_extensions_page(cx: &mut TestAppContext) {
    // Review C201. The link is not clicked here: clicking opens a browser.
    let home = Home::new();
    let (_, cx) = open(cx, &home);
    assert!(cx.debug_bounds("zed-extensions-link").is_none());
    click(cx, "browse-themes");
    assert!(cx.debug_bounds("zed-extensions-link").is_some());
    assert_eq!(EXTENSIONS_PAGE, "https://zed.dev/extensions");
}

fn entry(name: &str, origin: ThemeOrigin) -> ThemeEntry {
    ThemeEntry {
        name: name.into(),
        appearance: None,
        author: None,
        origin,
    }
}

#[test]
fn partly_shadowed_install_names_the_taken_themes() {
    // Review C236: the install works, but one of its names was taken.
    let ours = InstalledTheme {
        id: "nord".into(),
        name: Some("Nord".into()),
        ..InstalledTheme::default()
    };
    let themes = vec![
        entry("Nord Dark", ThemeOrigin::ZedUser),
        entry("Nord Light", ThemeOrigin::Agmen(ours.clone())),
    ];
    let names = ["Nord Dark".to_owned(), "Nord Light".to_owned()];
    let (first, status) = SettingsView::install_outcome(&ours, &names, &themes);
    assert_eq!(first.as_deref(), Some("Nord Light"));
    match status {
        BrowseStatus::Installed(note) => assert!(note.contains("Nord Dark"), "{note}"),
        other => panic!("expected a note, got {other:?}"),
    }
}

#[test]
fn fully_shadowed_install_is_installed_not_failed() {
    let ours = InstalledTheme {
        id: "nord".into(),
        ..InstalledTheme::default()
    };
    let themes = vec![entry("Nord Dark", ThemeOrigin::ZedUser)];
    let (first, status) = SettingsView::install_outcome(&ours, &["Nord Dark".to_owned()], &themes);
    assert_eq!(first, None);
    assert!(matches!(status, BrowseStatus::Installed(_)), "{status:?}");
}

#[test]
fn unreadable_manifest_keeps_the_license_and_says_why() {
    // Reviews C238 and C239: the note adds to the credit and shows the error.
    let theme = entry(
        "Broken",
        ThemeOrigin::ZedExtension(InstalledTheme {
            id: "broken".into(),
            license: Some("MIT License".into()),
            manifest_error: Some("expected `]`".into()),
            ..InstalledTheme::default()
        }),
    );
    let detail = rows::installed_detail(&theme);
    assert!(detail.contains("MIT License"), "{detail}");
    assert!(detail.contains("expected `]`"), "{detail}");
}

/// Opens Settings at Andy's 15 px rem, the size the mockup was drawn at.
fn open_at_mockup_rem<'a>(
    cx: &'a mut TestAppContext,
    home: &Home,
) -> (Entity<SettingsView>, &'a mut VisualTestContext) {
    let (view, cx) = open(cx, home);
    cx.update(|window, _| window.set_rem_size(crate::ui::tokens::MOCKUP_REM));
    redraw(cx);
    (view, cx)
}

fn height(cx: &mut VisualTestContext, selector: &'static str) -> f32 {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("nothing is drawn as {selector}"))
        .size
        .height
        .as_f32()
}

#[gpui::test]
fn page_title_is_18_px(cx: &mut TestAppContext) {
    // V97: node 8091 is 18 px Semibold.
    let home = Home::new();
    let (_, cx) = open_at_mockup_rem(cx, &home);
    let line = height(cx, "settings-title");
    // gpui snaps the 21.6 px line to 21.5; Large's line is 18.
    assert!(
        (line - 18. * crate::ui::tokens::LINE_HEIGHT).abs() < 0.25,
        "{line}"
    );
}

#[gpui::test]
fn open_settings_toml_is_a_plain_button(cx: &mut TestAppContext) {
    // V98: node 8046 is plain, 20 px tall: 3 px padding, no border.
    let home = Home::new();
    let (_, cx) = open_at_mockup_rem(cx, &home);
    let tall = height(cx, "open-settings-toml");
    assert!((tall - 19.5).abs() < 0.01, "{tall}");
}

#[gpui::test]
fn page_list_rule_is_the_chrome_rule(cx: &mut TestAppContext) {
    // V99: node 8049 draws border.variant, as the other chrome rules.
    init(cx);
    cx.update(|cx| {
        let mut appearance = Appearance::builtin();
        appearance.theme.border = hex("#ff0000");
        appearance.theme.border_variant = hex("#00ff00");
        cx.set_global(appearance);
        assert_eq!(SettingsView::page_list_rule(cx), hex("#00ff00"));
    });
}

#[gpui::test]
fn page_rows_are_28_px_list_items(cx: &mut TestAppContext) {
    // V100, V102: nodes 8050 are 28 px rows with the icon in a leading slot.
    let home = Home::new();
    let (_, cx) = open_at_mockup_rem(cx, &home);
    assert!(cx.debug_bounds("settings-page:keys:leading").is_some());
    let row = height(cx, "settings-page:keys");
    assert!((row - 28.).abs() < 0.01, "{row}");
    click(cx, "settings-page:keys");
    assert!(cx.debug_bounds("settings-placeholder").is_some());
}

#[gpui::test]
fn dropdowns_are_28_px(cx: &mut TestAppContext) {
    // V102: node 8107 is 28 px tall.
    let home = Home::new();
    let (_, cx) = open_at_mockup_rem(cx, &home);
    let tall = height(cx, "theme");
    assert!((tall - 28.).abs() < 0.01, "{tall}");
}

#[gpui::test]
fn footer_sits_under_the_last_rule(cx: &mut TestAppContext) {
    // V102: node 8214 starts right under the Chat density rule.
    let home = Home::new();
    let (_, cx) = open_at_mockup_rem(cx, &home);
    let density = cx.debug_bounds("setting:Chat density").unwrap();
    let footer = cx.debug_bounds("settings-footer-text").unwrap();
    assert_eq!(footer.origin.y, density.bottom());
}

#[test]
fn zed_installed_theme_without_license_says_zed_keeps_it() {
    // V107: Zed never copies a LICENSE into its installed folders, so a
    // missing file there is no fault of the theme.
    let zed = entry(
        "Nord",
        ThemeOrigin::ZedExtension(InstalledTheme {
            id: "nord".into(),
            ..InstalledTheme::default()
        }),
    );
    let detail = rows::installed_detail(&zed);
    assert!(detail.contains("license kept by Zed"), "{detail}");
    assert!(!detail.contains("no license file"), "{detail}");

    let ours = entry(
        "Nord",
        ThemeOrigin::Agmen(InstalledTheme {
            id: "nord".into(),
            ..InstalledTheme::default()
        }),
    );
    let detail = rows::installed_detail(&ours);
    assert!(detail.contains("no license file"), "{detail}");
}
