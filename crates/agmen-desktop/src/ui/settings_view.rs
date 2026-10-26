//! Settings (`desktop-settings.webp`): the page list, Appearance and About.
//! The other pages come with agmend. Every change saves `settings.toml` and
//! re-resolves the `Appearance` global, so every window repaints at once.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, Entity, EventEmitter, Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, WindowAppearance, div,
};

use super::components::{
    Breadcrumb, Button, Color, Dropdown, Icon, IconSize, Label, LabelSize, Segmented, Stepper,
    Toggle,
};
use super::text_input::{TextInput, TextInputEvent};
use super::tokens::{
    BORDER_WIDTH, ICON_PREVIEW, RADIUS_MD, SETTING_LABEL_WIDTH, SETTINGS_PAGES_WIDTH, SPACE_2,
    SPACE_3, SPACE_6, SPACE_8, SPACE_10, SPACE_12, SPACE_16, SPACE_24,
};
use crate::appearance::marketplace::{
    API_BASE, EXTENSIONS_PAGE, MarketError, Marketplace, ThemeExtension,
};
use crate::appearance::{
    AgmenSettings, Appearance, AppearanceSource, Density, FontPrefs, Fonts, IconThemeChoice, Mode,
    PickerEntry, SystemMode, ThemeEntry, ThemeOrigin, agmen_settings_path, installed_themes,
    picker_entries, resolve_paths, theme_dirs, zed_extensions_dir,
};
use crate::icons::{FileIcons, IconTheme, UiIcon};

/// Where Settings reads and writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SettingsSources {
    /// `~/.config`: holds `zed/` (read only) and `agmen/`.
    pub(crate) config: PathBuf,
    /// Zed's installed extensions (read only).
    pub(crate) extensions: PathBuf,
    /// Zed's extension API: [`API_BASE`] in the app.
    pub(crate) market: String,
}

impl SettingsSources {
    /// The real folders under `home` and the real API.
    #[must_use]
    pub(crate) fn for_home(home: &Path) -> Self {
        Self {
            config: home.join(".config"),
            extensions: zed_extensions_dir(home),
            market: API_BASE.to_owned(),
        }
    }

    fn settings_path(&self) -> PathBuf {
        agmen_settings_path(&self.config)
    }
}

/// The app sets the real folders at launch and tests a throwaway home; a
/// Settings tab without it shows a placeholder.
impl gpui::Global for SettingsSources {}

/// What Settings asks of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingsEvent {
    /// "Open settings.toml": open this file in a tab.
    OpenFile(PathBuf),
}

/// A page in the list at the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    General,
    Appearance,
    Listener,
    Agents,
    Roles,
    Notifications,
    Terminal,
    Keys,
    About,
}

/// Each page's title, selector slug and icon, in list order (console.py
/// `build_d_settings`, plus About for the notices).
const PAGES: [(Page, &str, &str, UiIcon); 9] = [
    (Page::General, "General", "general", UiIcon::Settings),
    (
        Page::Appearance,
        "Appearance",
        "appearance",
        UiIcon::Sparkle,
    ),
    (Page::Listener, "Listener", "listener", UiIcon::Chat),
    (Page::Agents, "Agents", "agents", UiIcon::Agents),
    (Page::Roles, "Roles", "roles", UiIcon::Roles),
    (
        Page::Notifications,
        "Notifications",
        "notifications",
        UiIcon::Bell,
    ),
    (Page::Terminal, "Terminal", "terminal", UiIcon::Terminal),
    (Page::Keys, "Keys", "keys", UiIcon::Pencil),
    (Page::About, "About", "about", UiIcon::File),
];

/// The notices About shows: what agmen bundles or links, with its license
/// text (THIRD_PARTY_NOTICES.md lists the same).
pub(crate) const NOTICES: [(&str, &str); 5] = [
    ("Lucide icons (ISC License)", crate::assets::LUCIDE_LICENSE),
    (
        "IBM Plex Sans (SIL Open Font License 1.1)",
        crate::assets::IBM_PLEX_LICENSE,
    ),
    (
        "Lilex (SIL Open Font License 1.1)",
        crate::assets::LILEX_LICENSE,
    ),
    (
        "gpui and Zed's other crates (Apache License 2.0)",
        crate::assets::ZED_APACHE_LICENSE,
    ),
    (
        "zed-font-kit (MIT License)",
        crate::assets::FONT_KIT_MIT_LICENSE,
    ),
];

/// The dropdown that is open, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Theme,
    IconTheme,
    UiFont,
    CodeFont,
    TerminalFont,
}

/// Where Zed's app usually is; Follow Zed names the one it finds.
const ZED_APPS: [&str; 2] = ["/Applications/Zed.app", "Applications/Zed.app"];

/// Rows the browse list shows at most.
// shortcut: a flat list capped at 40 rows; a virtual list (Task 3's) if people browse all ~700.
const MAX_BROWSE_ROWS: usize = 40;

/// What "Browse Zed themes" is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BrowseStatus {
    Loading,
    Ready,
    Installing(String),
    Failed(String),
}

struct Browse {
    search: Entity<TextInput>,
    query: String,
    status: BrowseStatus,
    results: Vec<ThemeExtension>,
    _submit: Subscription,
}

/// The Settings center tab. Tests find each control by the selector of the
/// component it uses: `follow-zed`, `theme`, `theme-mode`, `icon-theme`,
/// `ui-font`, `ui-size`, `code-font`, `code-size`, `ligatures`,
/// `terminal-font`, `terminal-size`, `density`; pages as
/// `settings-page:{slug}`.
pub(crate) struct SettingsView {
    sources: SettingsSources,
    settings: AgmenSettings,
    pub(super) page: Page,
    menu: Option<Menu>,
    themes: Vec<ThemeEntry>,
    icon_themes: Vec<String>,
    fonts: Vec<String>,
    zed_app: Option<PathBuf>,
    /// The last failure to read or save settings, shown under the form.
    error: Option<String>,
    market: Option<Arc<Mutex<Marketplace>>>,
    browse: Option<Browse>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    /// Settings on Appearance, reading `sources`. The `Appearance` global
    /// must already be set.
    pub(crate) fn new(sources: SettingsSources, cx: &mut Context<Self>) -> Self {
        let (settings, error) = match AgmenSettings::load(&sources.settings_path()) {
            Ok(settings) => (settings, None),
            // Saving refuses to overwrite a file it cannot parse, so the
            // person's file is safe; say why changes will not stick.
            Err(err) => (AgmenSettings::default(), Some(err.to_string())),
        };
        let home = sources.config.parent().map(Path::to_owned);
        let zed_app = ZED_APPS
            .iter()
            .map(|app| match &home {
                Some(home) if !app.starts_with('/') => home.join(app),
                _ => PathBuf::from(app),
            })
            .find(|app| app.is_dir());
        let mut fonts = cx.text_system().all_font_names();
        fonts.retain(|name| !name.starts_with('.'));
        fonts.extend([".ZedSans".to_owned(), ".ZedMono".to_owned()]);
        fonts.sort();
        fonts.dedup();
        let mut view = Self {
            sources,
            settings,
            page: Page::Appearance,
            menu: None,
            themes: Vec::new(),
            icon_themes: Vec::new(),
            fonts,
            zed_app,
            error,
            market: None,
            browse: None,
        };
        view.rescan();
        view
    }

    /// Reads the installed themes and icon themes again.
    // shortcut: scans on the UI thread when Settings opens or an install ends; move to the
    // background executor if a person's extension folder makes opening Settings slow.
    fn rescan(&mut self) {
        let dirs = theme_dirs(&self.sources.config, &self.sources.extensions);
        self.themes = installed_themes(&dirs).0;
        self.icon_themes = IconTheme::installed(&self.sources.extensions);
    }

    /// The light or dark half the theme list shows.
    fn wanted_mode(&self, cx: &App) -> SystemMode {
        match self.settings.mode {
            Mode::Light => SystemMode::Light,
            Mode::Dark => SystemMode::Dark,
            Mode::System => system_mode(cx.window_appearance()),
        }
    }

    /// The theme dropdown's names: the installed themes of the mode chosen
    /// (or of either, when a theme does not say), with the theme in use
    /// first when it is of the other mode.
    pub(super) fn theme_options(&self, cx: &App) -> Vec<String> {
        let wanted = self.wanted_mode(cx);
        let mut names: Vec<String> = self
            .themes
            .iter()
            .filter(|t| t.appearance.is_none_or(|mode| mode == wanted))
            .map(|t| t.name.clone())
            .collect();
        let current = &cx.global::<Appearance>().theme_name;
        if !names.contains(current) {
            names.insert(0, current.clone());
        }
        names
    }

    /// The icon theme dropdown's labels and choices.
    fn icon_options(&self, current: &IconThemeChoice) -> Vec<(String, IconThemeChoice)> {
        let mut options: Vec<(String, IconThemeChoice)> = self
            .icon_themes
            .iter()
            .map(|name| {
                (
                    format!("{name} (from Zed)"),
                    IconThemeChoice::Zed(name.clone()),
                )
            })
            .collect();
        if let IconThemeChoice::Zed(name) = current
            && !self.icon_themes.contains(name)
        {
            options.insert(0, (format!("{name} (not installed)"), current.clone()));
        }
        options.push(("Lucide (agmen's own)".to_owned(), IconThemeChoice::Lucide));
        options.push(("System".to_owned(), IconThemeChoice::System));
        options
    }

    /// The font list with `current` in it.
    fn font_options(&self, current: &str) -> Vec<String> {
        let mut fonts = self.fonts.clone();
        if !fonts.iter().any(|f| f == current) {
            fonts.insert(0, current.to_owned());
        }
        fonts
    }

    /// Copies what is on screen into agmen's settings and turns Follow Zed
    /// off, so turning it off changes nothing visible.
    fn keep_current(&mut self, cx: &App) {
        let appearance = cx.global::<Appearance>();
        self.settings.follow_zed = false;
        self.settings.theme = Some(appearance.theme_name.clone());
        self.settings.fonts = FontPrefs::from(&appearance.fonts);
        self.settings.icon_theme = appearance.icon_theme.clone();
        self.settings.density = appearance.density;
    }

    /// Applies one change. A change while following Zed first keeps what
    /// Zed gave, then turns Follow Zed off: the person chose something here.
    fn change(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut AgmenSettings)) {
        if self.settings.follow_zed {
            self.keep_current(cx);
        }
        edit(&mut self.settings);
        self.commit(cx);
    }

    fn set_follow_zed(&mut self, on: bool, cx: &mut Context<Self>) {
        if on {
            self.settings.follow_zed = true;
        } else {
            self.keep_current(cx);
        }
        self.commit(cx);
    }

    /// The mode only picks which themes the list shows, so it does not turn
    /// Follow Zed off.
    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.settings.mode = mode;
        self.commit(cx);
    }

    /// Saves `settings.toml` and repaints every window with the result.
    fn commit(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        match self.settings.save(&self.sources.settings_path()) {
            Ok(()) => {
                self.error = None;
                apply(&self.sources, cx);
            }
            Err(err) => self.error = Some(format!("{err}; nothing was changed")),
        }
        cx.notify();
    }

    fn set_menu(&mut self, menu: Menu, open: bool, cx: &mut Context<Self>) {
        self.menu = open.then_some(menu);
        cx.notify();
    }

    /// Opens or closes "Browse Zed themes", listing all on open.
    fn toggle_browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browse.take().is_some() {
            cx.notify();
            return;
        }
        let search = cx.new(|cx| TextInput::new(cx).placeholder("Search Zed's theme extensions"));
        let submit = cx.subscribe(&search, |this, _, event: &TextInputEvent, cx| {
            let TextInputEvent::Submit(query) = event;
            this.search(query.clone(), cx);
        });
        window.focus(&search.read(cx).focus_handle(cx), cx);
        self.browse = Some(Browse {
            search,
            query: String::new(),
            status: BrowseStatus::Loading,
            results: Vec::new(),
            _submit: submit,
        });
        self.search(String::new(), cx);
    }

    /// The API client, made on first use.
    fn market(&mut self) -> Result<Arc<Mutex<Marketplace>>, MarketError> {
        if let Some(market) = &self.market {
            return Ok(market.clone());
        }
        let market = Arc::new(Mutex::new(Marketplace::new(self.sources.market.clone())?));
        self.market = Some(market.clone());
        Ok(market)
    }

    fn set_status(&mut self, status: BrowseStatus, cx: &mut Context<Self>) {
        if let Some(browse) = &mut self.browse {
            browse.status = status;
        }
        cx.notify();
    }

    /// Asks the API for themes matching `query`, off the UI thread.
    fn search(&mut self, query: String, cx: &mut Context<Self>) {
        let market = match self.market() {
            Ok(market) => market,
            Err(err) => return self.set_status(BrowseStatus::Failed(err.to_string()), cx),
        };
        if let Some(browse) = &mut self.browse {
            browse.query.clone_from(&query);
        }
        self.set_status(BrowseStatus::Loading, cx);
        let task = cx.background_executor().spawn(async move {
            market
                .lock()
                .map_err(|_| "the theme list is unavailable after an earlier crash".to_owned())?
                .search(&query, Instant::now())
                .map_err(|err| err.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(results) => {
                    if let Some(browse) = &mut this.browse {
                        browse.results = results;
                    }
                    this.set_status(BrowseStatus::Ready, cx);
                }
                Err(err) => this.set_status(BrowseStatus::Failed(err), cx),
            })
            // An error means Settings closed meanwhile: nothing to show.
            .unwrap_or(());
        })
        .detach();
    }

    /// Installs `ext` into agmen's themes folder, then switches to its first
    /// theme.
    fn install(&mut self, ext: ThemeExtension, cx: &mut Context<Self>) {
        let market = match self.market() {
            Ok(market) => market,
            Err(err) => return self.set_status(BrowseStatus::Failed(err.to_string()), cx),
        };
        self.set_status(BrowseStatus::Installing(ext.name.clone()), cx);
        let dir = theme_dirs(&self.sources.config, &self.sources.extensions).agmen;
        let task = cx.background_executor().spawn(async move {
            market
                .lock()
                .map_err(|_| "the theme list is unavailable after an earlier crash".to_owned())?
                .install(&ext, &dir, Instant::now())
                .map_err(|err| err.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(installed) => {
                    this.rescan();
                    let first = this.themes.iter().find_map(|t| match &t.origin {
                        ThemeOrigin::Agmen(theme) if theme.id == installed.id => {
                            Some(t.name.clone())
                        }
                        _ => None,
                    });
                    this.set_status(BrowseStatus::Ready, cx);
                    if let Some(name) = first {
                        this.change(cx, |s| s.theme = Some(name));
                    }
                }
                Err(err) => this.set_status(BrowseStatus::Failed(err), cx),
            })
            // An error means Settings closed meanwhile: nothing to show.
            .unwrap_or(());
        })
        .detach();
    }

    /// A handler for a component's `on_change` that calls `f` on this view.
    fn handler<T: 'static>(
        cx: &Context<Self>,
        f: impl Fn(&mut Self, T, &mut Context<Self>) + 'static,
    ) -> impl Fn(T, &mut Window, &mut App) + 'static {
        let this = cx.entity().downgrade();
        move |value, _, cx| {
            // Gone only while the window closes; there is nothing to change.
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| f(this, value, cx));
            }
        }
    }

    fn page_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let (selected, hover, rule) = (theme.element_selected, theme.element_hover, theme.border);
        let rows = PAGES.iter().map(|&(page, title, slug, icon)| {
            let on = page == self.page;
            let selector = format!("settings-page:{slug}");
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(|| selector)
                .flex()
                .items_center()
                .gap(SPACE_8)
                .py(SPACE_6)
                .px(SPACE_10)
                .rounded(RADIUS_MD)
                .cursor_pointer()
                .map(|this| {
                    if on {
                        this.bg(selected)
                    } else {
                        this.hover(move |style| style.bg(hover))
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.page = page;
                    this.menu = None;
                    cx.notify();
                }))
                .child(Icon::new(icon).size(IconSize::Medium).color(if on {
                    Color::Default
                } else {
                    Color::Placeholder
                }))
                .child(
                    Label::new(title)
                        .color(if on {
                            Color::Default
                        } else {
                            Color::Placeholder
                        })
                        .when(on, |label| label.weight(FontWeight::MEDIUM)),
                )
        });
        div()
            .w(SETTINGS_PAGES_WIDTH)
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .py(SPACE_12)
            .px(SPACE_8)
            .border_r(BORDER_WIDTH)
            .border_color(rule)
            .children(rows)
    }

    fn appearance_page(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let appearance = cx.global::<Appearance>().clone();
        vec![
            self.follow_row(&appearance, cx),
            self.theme_row(&appearance, cx),
            self.icon_row(&appearance, cx),
            self.ui_font_row(&appearance.fonts, cx),
            self.code_font_row(&appearance.fonts, cx),
            self.terminal_row(&appearance.fonts, cx),
            density_row(appearance.density, cx),
            self.footer(),
        ]
    }

    fn follow_row(&self, appearance: &Appearance, cx: &mut Context<Self>) -> AnyElement {
        let follow = self.settings.follow_zed;
        let zed_dir = self.sources.config.join("zed");
        let status = match (follow, &self.zed_app) {
            (true, Some(app)) => format!("On \u{b7} Zed found at {}", app.display()),
            (true, None) => format!(
                "On \u{b7} reading {}",
                zed_dir.join("settings.json").display()
            ),
            (false, _) => "Off \u{b7} agmen's own settings below".to_owned(),
        };
        let warnings = appearance.warnings.iter().map(|warning| {
            Label::new(warning.clone())
                .size(LabelSize::Small)
                .color(Color::Warning)
                .wrap()
        });
        setting(
            "Follow Zed",
            "Use the theme, icon theme and fonts from your Zed settings. Turn off to choose them here.",
            cx,
        )
        .child(
            controls()
                .child(
                    row()
                        .child(
                            Toggle::new("follow-zed", follow)
                                .on_change(Self::handler(cx, |this, on, cx| this.set_follow_zed(on, cx))),
                        )
                        .child(Label::new(status).size(LabelSize::Small)),
                )
                .when(appearance.source == AppearanceSource::Builtin || !appearance.warnings.is_empty(), |this| {
                    this.children(warnings)
                }),
        )
        .into_any_element()
    }

    fn theme_row(&self, appearance: &Appearance, cx: &mut Context<Self>) -> AnyElement {
        let themes = self.theme_options(cx);
        let theme_ix = themes
            .iter()
            .position(|t| *t == appearance.theme_name)
            .unwrap_or_default();
        let mode_ix = match self.settings.mode {
            Mode::Light => 0,
            Mode::Dark => 1,
            Mode::System => 2,
        };
        let theme_names = themes.clone();
        setting(
            "Theme",
            "Any Zed theme: yours from ~/.config/zed/themes, or one from Zed's theme extensions.",
            cx,
        )
        .child(
            controls()
                .child(
                    row()
                        .child(
                            Dropdown::new("theme", themes, theme_ix)
                                .open(self.menu == Some(Menu::Theme))
                                .on_toggle(Self::handler(cx, |this, open, cx| {
                                    this.set_menu(Menu::Theme, open, cx);
                                }))
                                .on_change(Self::handler(cx, move |this, ix: usize, cx| {
                                    if let Some(name) = theme_names.get(ix).cloned() {
                                        this.change(cx, |s| s.theme = Some(name));
                                    }
                                })),
                        )
                        .child(
                            Segmented::new("theme-mode", ["Light", "Dark", "System"], mode_ix)
                                .on_change(Self::handler(cx, |this, ix, cx| {
                                    let mode = match ix {
                                        0 => Mode::Light,
                                        1 => Mode::Dark,
                                        _ => Mode::System,
                                    };
                                    this.set_mode(mode, cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .id("browse-themes")
                        .debug_selector(|| "browse-themes".into())
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_browse(window, cx)))
                        .child(
                            Label::new(if self.browse.is_some() {
                                "Close Zed themes"
                            } else {
                                "Browse Zed themes\u{2026}"
                            })
                            .size(LabelSize::Small)
                            .color(Color::Accent),
                        ),
                )
                .children(self.browse_panel(cx)),
        )
        .into_any_element()
    }

    fn icon_row(&self, appearance: &Appearance, cx: &mut Context<Self>) -> AnyElement {
        let icon_options = self.icon_options(&appearance.icon_theme);
        let icon_ix = icon_options
            .iter()
            .position(|(_, choice)| *choice == appearance.icon_theme)
            .unwrap_or_default();
        let icon_choices: Vec<IconThemeChoice> = icon_options
            .iter()
            .map(|(_, choice)| choice.clone())
            .collect();
        setting(
            "Icon theme",
            "File and folder icons. Zed's installed icon themes, agmen's own (Lucide), or your system's.",
            cx,
        )
        .child(
            controls()
                .child(
                    Dropdown::new(
                        "icon-theme",
                        icon_options.into_iter().map(|(label, _)| label),
                        icon_ix,
                    )
                    .open(self.menu == Some(Menu::IconTheme))
                    .on_toggle(Self::handler(cx, |this, open, cx| {
                        this.set_menu(Menu::IconTheme, open, cx);
                    }))
                    .on_change(Self::handler(cx, move |this, ix: usize, cx| {
                        if let Some(choice) = icon_choices.get(ix).cloned() {
                            this.change(cx, |s| s.icon_theme = choice);
                        }
                    })),
                )
                .child(icon_preview(cx)),
        )
        .into_any_element()
    }

    fn ui_font_row(&self, fonts: &Fonts, cx: &mut Context<Self>) -> AnyElement {
        let ui_fonts = self.font_options(&fonts.ui_family);
        setting("UI font", "Panels, chat text and menus.", cx)
            .child(
                controls().child(
                    row()
                        .child(self.font_dropdown(
                            Menu::UiFont,
                            "ui-font",
                            ui_fonts,
                            &fonts.ui_family,
                            cx,
                            |s, f| {
                                s.fonts.ui_family = Some(f);
                            },
                        ))
                        .child(size_stepper("ui-size", fonts.ui_size, cx, |s, v| {
                            s.fonts.ui_size = Some(v)
                        })),
                ),
            )
            .into_any_element()
    }

    fn code_font_row(&self, fonts: &Fonts, cx: &mut Context<Self>) -> AnyElement {
        let code_fonts = self.font_options(&fonts.code_family);
        let ligatures = fonts.code_ligatures;
        setting(
            "Code font",
            "Files, diffs, handles, branches and code in chat.",
            cx,
        )
        .child(
            controls().child(
                row()
                    .child(
                        self.font_dropdown(
                            Menu::CodeFont,
                            "code-font",
                            code_fonts,
                            &fonts.code_family,
                            cx,
                            |s, f| {
                                s.fonts.code_family = Some(f);
                            },
                        )
                        .code(),
                    )
                    .child(size_stepper("code-size", fonts.code_size, cx, |s, v| {
                        s.fonts.code_size = Some(v);
                    }))
                    .child(
                        Segmented::new(
                            "ligatures",
                            ["Ligatures on", "off"],
                            usize::from(!ligatures),
                        )
                        .on_change(Self::handler(
                            cx,
                            |this, ix: usize, cx| {
                                this.change(cx, |s| s.fonts.code_ligatures = Some(ix == 0));
                            },
                        )),
                    ),
            ),
        )
        .into_any_element()
    }

    fn terminal_row(&self, fonts: &Fonts, cx: &mut Context<Self>) -> AnyElement {
        let mut terminal_fonts = vec!["Same as code font".to_owned()];
        terminal_fonts.extend(
            self.font_options(
                fonts
                    .terminal_family
                    .as_deref()
                    .unwrap_or(&fonts.code_family),
            ),
        );
        let terminal_ix = fonts
            .terminal_family
            .as_ref()
            .and_then(|f| terminal_fonts.iter().skip(1).position(|t| t == f.as_ref()))
            .map_or(0, |ix| ix + 1);
        let terminal_names = terminal_fonts.clone();
        setting("Terminal font", "Embedded terminals and take-over.", cx)
            .child(
                controls().child(
                    row()
                        .child(
                            Dropdown::new(
                                "terminal-font",
                                terminal_fonts.iter().map(|f| font_label(f)),
                                terminal_ix,
                            )
                            .open(self.menu == Some(Menu::TerminalFont))
                            .on_toggle(Self::handler(cx, |this, open, cx| {
                                this.set_menu(Menu::TerminalFont, open, cx);
                            }))
                            .on_change(Self::handler(
                                cx,
                                move |this, ix: usize, cx| {
                                    let family =
                                        (ix > 0).then(|| terminal_names.get(ix).cloned()).flatten();
                                    this.change(cx, |s| s.fonts.terminal_family = family);
                                },
                            )),
                        )
                        .child(size_stepper(
                            "terminal-size",
                            fonts.terminal_size,
                            cx,
                            |s, v| {
                                s.fonts.terminal_size = Some(v);
                            },
                        )),
                ),
            )
            .into_any_element()
    }

    fn footer(&self) -> AnyElement {
        let saved_to = format!(
            "Saved to {}. The terminal UI reads the same file; there, :set theme=\u{2026} and :set icons=nerd|none change it, and the font is your terminal's.",
            tilde(&self.sources.settings_path(), self.sources.config.parent())
        );
        div()
            .py(SPACE_6)
            .child(
                Label::new(saved_to)
                    .size(LabelSize::Small)
                    .color(Color::Placeholder)
                    .wrap(),
            )
            .into_any_element()
    }

    /// A font dropdown whose choice `set` stores.
    fn font_dropdown(
        &self,
        menu: Menu,
        id: &'static str,
        fonts: Vec<String>,
        current: &str,
        cx: &Context<Self>,
        set: fn(&mut AgmenSettings, String),
    ) -> Dropdown {
        let ix = fonts.iter().position(|f| f == current).unwrap_or_default();
        let names = fonts.clone();
        Dropdown::new(id, fonts.iter().map(|f| font_label(f)), ix)
            .open(self.menu == Some(menu))
            .on_toggle(Self::handler(cx, move |this, open, cx| {
                this.set_menu(menu, open, cx)
            }))
            .on_change(Self::handler(cx, move |this, ix: usize, cx| {
                if let Some(family) = names.get(ix).cloned() {
                    this.change(cx, |s| set(s, family));
                }
            }))
    }

    /// "Browse Zed themes": credit, search, then installed and offered themes.
    fn browse_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let browse = self.browse.as_ref()?;
        let theme = &cx.global::<Appearance>().theme;
        let (border, hover) = (theme.border, theme.element_hover);
        let status = match &browse.status {
            BrowseStatus::Loading => {
                Some(Label::new("Asking Zed's extension list\u{2026}").color(Color::Placeholder))
            }
            BrowseStatus::Installing(name) => {
                Some(Label::new(format!("Installing {name}\u{2026}")).color(Color::Placeholder))
            }
            BrowseStatus::Failed(err) => Some(Label::new(err.clone()).color(Color::Warning).wrap()),
            BrowseStatus::Ready => None,
        };
        let entries = picker_entries(&self.themes, &browse.results);
        let more = entries.len().saturating_sub(MAX_BROWSE_ROWS);
        let rows = entries
            .into_iter()
            .take(MAX_BROWSE_ROWS)
            .enumerate()
            .map(|(ix, entry)| browse_row(ix, entry, hover, cx));
        Some(
            div()
                .debug_selector(|| "browse-panel".into())
                .w_full()
                .flex()
                .flex_col()
                .gap(SPACE_6)
                .p(SPACE_8)
                .rounded(RADIUS_MD)
                .border(BORDER_WIDTH)
                .border_color(border)
                .child(
                    Label::new(format!(
                        "Themes from Zed extensions ({EXTENSIONS_PAGE}). Each keeps its author and license; one without a license file is not installed."
                    ))
                    .size(LabelSize::Small)
                    .color(Color::Placeholder)
                    .wrap(),
                )
                .child(
                    div()
                        .py(SPACE_3)
                        .px(SPACE_8)
                        .rounded(RADIUS_MD)
                        .border(BORDER_WIDTH)
                        .border_color(border)
                        .child(browse.search.clone()),
                )
                .when(!browse.query.is_empty(), |this| {
                    this.child(
                        Label::new(format!("Matching \u{201c}{}\u{201d}", browse.query))
                            .size(LabelSize::Small)
                            .color(Color::Placeholder),
                    )
                })
                .children(status)
                .children(rows)
                .when(more > 0, |this| {
                    this.child(
                        Label::new(format!("and {more} more; search to narrow the list"))
                            .size(LabelSize::Small)
                            .color(Color::Placeholder),
                    )
                }),
        )
    }

    fn about_page() -> Vec<AnyElement> {
        let mut out = vec![
            Label::new(format!("agmen-desktop {}", env!("CARGO_PKG_VERSION"))).into_any_element(),
        ];
        for (ix, (title, text)) in NOTICES.iter().enumerate() {
            let selector = if ix == 0 {
                "about:lucide".to_owned()
            } else {
                format!("about:{ix}")
            };
            out.push(
                div()
                    .debug_selector(|| selector)
                    .flex()
                    .flex_col()
                    .gap(SPACE_6)
                    .py(SPACE_12)
                    .child(
                        Label::new(*title)
                            .weight(FontWeight::MEDIUM)
                            .color(Color::TextMuted),
                    )
                    .child(
                        Label::new(*text)
                            .size(LabelSize::Small)
                            .color(Color::Placeholder)
                            .wrap(),
                    )
                    .into_any_element(),
            );
        }
        out
    }
}

/// The OS's light or dark mode.
const fn system_mode(os: WindowAppearance) -> SystemMode {
    match os {
        WindowAppearance::Light | WindowAppearance::VibrantLight => SystemMode::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => SystemMode::Dark,
    }
}

/// Re-resolves the appearance from `sources` and repaints every window.
pub(crate) fn apply(sources: &SettingsSources, cx: &mut App) {
    let mut appearance = resolve_paths(
        &sources.config,
        &sources.extensions,
        system_mode(cx.window_appearance()),
    );
    let (icons, warning) = FileIcons::new(&appearance.icon_theme, &sources.extensions);
    if let Some(warning) = warning {
        appearance.warnings.push(warning);
    }
    cx.set_global(appearance);
    cx.set_global(icons);
    cx.refresh_windows();
}

/// A font family as the list shows it: Zed's dotted names by their fonts.
fn font_label(family: &str) -> String {
    match family {
        ".ZedSans" => "Zed Sans (IBM Plex Sans)".to_owned(),
        ".ZedMono" => "Zed Mono (Lilex)".to_owned(),
        other => other.to_owned(),
    }
}

/// `path` with the home folder written as `~`.
fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The credit line of an installed theme: who made it and where it is from.
fn installed_detail(theme: &ThemeEntry) -> String {
    let by = theme
        .author
        .as_ref()
        .map(|a| format!("by {a} \u{b7} "))
        .unwrap_or_default();
    let from = match &theme.origin {
        ThemeOrigin::BuiltIn => "built into agmen".to_owned(),
        ThemeOrigin::ZedUser => "your Zed themes folder".to_owned(),
        ThemeOrigin::ZedExtension(id) => format!("installed in Zed ({id})"),
        ThemeOrigin::Agmen(t) => {
            let license = t.license.as_deref().unwrap_or("no license file");
            let repo = t
                .repository
                .as_ref()
                .map(|r| format!(" \u{b7} {r}"))
                .unwrap_or_default();
            format!("installed by agmen \u{b7} {license}{repo}")
        }
    };
    format!("{by}{from}")
}

/// The credit line of an offered theme; its license is checked on install.
fn offered_detail(ext: &ThemeExtension) -> String {
    let by = if ext.authors.is_empty() {
        String::new()
    } else {
        format!("by {} \u{b7} ", ext.authors.join(", "))
    };
    let repo = ext.repository.as_deref().unwrap_or("no repository");
    format!("{by}{repo} \u{b7} license checked on install")
}

fn density_row(density: Density, cx: &mut Context<SettingsView>) -> AnyElement {
    let density_ix = usize::from(density == Density::Compact);
    setting("Chat density", "Spacing between messages.", cx)
        .child(controls().child(
            Segmented::new("density", ["Comfortable", "Compact"], density_ix).on_change(
                SettingsView::handler(cx, |this, ix: usize, cx| {
                    let density = if ix == 0 {
                        Density::Comfortable
                    } else {
                        Density::Compact
                    };
                    this.change(cx, |s| s.density = density);
                }),
            ),
        ))
        .into_any_element()
}

/// One row of the browse list: name, credit line, and Use or Install.
fn browse_row(
    ix: usize,
    entry: PickerEntry,
    hover: gpui::Hsla,
    cx: &mut Context<SettingsView>,
) -> impl IntoElement + use<> {
    let selector = format!("browse:{ix}");
    let (name, detail, action): (String, String, AnyElement) = match entry {
        PickerEntry::Installed(t) => {
            let detail = installed_detail(&t);
            let name = t.name.clone();
            let pick = Button::new(SharedString::from(format!("browse:{ix}:use")), "Use").on_click(
                cx.listener(move |this, _, _, cx| {
                    let name = name.clone();
                    this.change(cx, |s| s.theme = Some(name));
                }),
            );
            (t.name, detail, pick.into_any_element())
        }
        PickerEntry::Marketplace(ext) => {
            let detail = offered_detail(&ext);
            let name = ext.name.clone();
            let install = Button::new(
                SharedString::from(format!("browse:{ix}:install")),
                "Install",
            )
            .on_click(cx.listener(move |this, _, _, cx| this.install(ext.clone(), cx)));
            (name, detail, install.into_any_element())
        }
    };
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(|| selector)
        .flex()
        .items_center()
        .gap(SPACE_10)
        .py(SPACE_3)
        .px(SPACE_8)
        .rounded(RADIUS_MD)
        .hover(move |style| style.bg(hover))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(Label::new(name))
                .child(
                    Label::new(detail)
                        .size(LabelSize::Small)
                        .color(Color::Placeholder)
                        .wrap(),
                ),
        )
        .child(action)
}

/// One setting: its label and hint on the left; the caller adds controls.
fn setting(label: &'static str, hint: &'static str, cx: &App) -> Div {
    div()
        .w_full()
        .flex()
        .items_start()
        .gap(SPACE_16)
        .py(SPACE_12)
        .border_b(BORDER_WIDTH)
        .border_color(super::rule_color(cx))
        .child(
            div()
                .w(SETTING_LABEL_WIDTH)
                .flex_none()
                .flex()
                .flex_col()
                .gap(SPACE_3)
                .child(
                    Label::new(label)
                        .weight(FontWeight::MEDIUM)
                        .color(Color::TextMuted),
                )
                .child(
                    Label::new(hint)
                        .size(LabelSize::Small)
                        .color(Color::Placeholder)
                        .wrap(),
                ),
        )
}

/// The control column of a setting.
fn controls() -> Div {
    // Start-aligned, so a segmented control keeps its own width.
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_start()
        .gap(SPACE_8)
}

/// Controls side by side.
fn row() -> Div {
    div().flex().items_center().gap(SPACE_10)
}

/// A font size stepper whose value `set` stores.
fn size_stepper(
    id: &'static str,
    size: gpui::Pixels,
    cx: &Context<SettingsView>,
    set: fn(&mut AgmenSettings, f32),
) -> Stepper {
    // Sizes are whole pixels in the stepper; a fractional size from Zed's
    // settings shows rounded and steps from there.
    let value = size.as_f32().round().clamp(0., 1000.);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded and clamped to 0 to 1000 just above"
    )]
    let value = value as u32;
    Stepper::new(id, value).on_change(SettingsView::handler(cx, move |this, v: u32, cx| {
        #[expect(clippy::cast_precision_loss, reason = "6 to 72 is exact in f32")]
        let v = v as f32;
        this.change(cx, |s| set(s, v));
    }))
}

/// The icon theme preview: the mockup's eight files and folders.
fn icon_preview(cx: &mut App) -> impl IntoElement + use<> {
    const NAMES: [(&str, bool); 8] = [
        ("src", true),
        ("tests", true),
        ("main.rs", false),
        ("Cargo.toml", false),
        ("README.md", false),
        ("Cargo.lock", false),
        ("reviewer.xml", false),
        ("release.yml", false),
    ];
    let theme = &cx.global::<Appearance>().theme;
    let (bg, tint) = (theme.background, theme.icon_muted);
    let icons: Vec<AnyElement> = cx.try_global::<FileIcons>().map_or_else(Vec::new, |icons| {
        NAMES
            .iter()
            .map(|(name, is_dir)| {
                icons
                    .icon(Path::new(name), *is_dir, false)
                    .render(tint, ICON_PREVIEW)
            })
            .collect()
    });
    cx.defer(FileIcons::fetch_wanted);
    div()
        .debug_selector(|| "icon-preview".into())
        .flex()
        .items_center()
        .gap(SPACE_10)
        .py(SPACE_6)
        .px(SPACE_10)
        .rounded(RADIUS_MD)
        .bg(bg)
        .children(icons)
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, slug) = PAGES
            .iter()
            .find(|(page, ..)| *page == self.page)
            .map_or(("Appearance", "appearance"), |(_, title, slug, _)| {
                (*title, *slug)
            });
        let body: Vec<AnyElement> = match self.page {
            Page::Appearance => self.appearance_page(cx),
            Page::About => Self::about_page(),
            _ => vec![
                div()
                    .debug_selector(|| "settings-placeholder".into())
                    .py(SPACE_12)
                    .child(Label::new("Comes with agmend.").color(Color::Placeholder))
                    .into_any_element(),
            ],
        };
        let path = self.sources.settings_path();
        let open_file = Button::new("open-settings-toml", "Open settings.toml").on_click(
            cx.listener(move |_, _, _, cx| cx.emit(SettingsEvent::OpenFile(path.clone()))),
        );
        let error = self.error.clone().map(|err| {
            Label::new(err)
                .size(LabelSize::Small)
                .color(Color::Error)
                .wrap()
        });
        let theme = &cx.global::<Appearance>().theme;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .child(
                Breadcrumb::new(["settings".into(), SharedString::from(slug)]).end(
                    div()
                        .debug_selector(|| "open-settings-toml".into())
                        .child(open_file),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.page_list(cx))
                    .child(
                        div()
                            .id("settings-form")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .py(SPACE_8)
                            .px(SPACE_24)
                            .child(
                                Label::new(title)
                                    .size(LabelSize::Large)
                                    .weight(FontWeight::SEMIBOLD)
                                    .color(Color::TextMuted),
                            )
                            .children(body)
                            .children(error),
                    ),
            )
    }
}

#[cfg(test)]
#[path = "settings_view_tests.rs"]
mod tests;
