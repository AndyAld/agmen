//! Settings (`desktop-settings.webp`): the page list, Appearance and About.
//! The other pages come with agmend. Every change saves `settings.toml` and
//! re-resolves the `Appearance` global, so every window repaints at once.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, WindowAppearance, div,
};

use self::rows::{
    browse_row, controls, density_row, font_label, icon_preview, row, setting, size_stepper, tilde,
};
use super::components::{
    Breadcrumb, Button, ButtonStyle, Color, Dropdown, Icon, IconSize, Indent, InputFrame, Label,
    LabelSize, Link, ListItem, Segmented, Toggle,
};
use super::text_input::{TextInput, TextInputEvent};
use super::tokens::{
    BORDER_WIDTH, RADIUS_MD, SETTINGS_PAGE_ROW_HEIGHT, SETTINGS_PAGES_WIDTH, SPACE_2, SPACE_3,
    SPACE_6, SPACE_8, SPACE_12, SPACE_24,
};
use crate::appearance::{
    API_BASE, AgmenSettings, Appearance, AppearanceSource, EXTENSIONS_PAGE, FontPrefs, Fonts,
    GITHUB_API, IconThemeChoice, InstalledTheme, MarketError, Marketplace, Mode, SystemMode,
    ThemeEntry, ThemeExtension, ThemeOrigin, agmen_settings_path, install_shared, installed_themes,
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
    /// GitHub's API, asked for licenses: [`GITHUB_API`] in the app.
    pub(crate) github: String,
}

impl SettingsSources {
    /// The real folders under `home` and the real API.
    #[must_use]
    pub(crate) fn for_home(home: &Path) -> Self {
        Self {
            config: home.join(".config"),
            extensions: zed_extensions_dir(home),
            market: API_BASE.to_owned(),
            github: GITHUB_API.to_owned(),
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
    /// Installed, with a note: some or all of its theme names were taken.
    Installed(String),
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
    /// Counts searches, so only the newest one's answer is shown (C209).
    /// Kept here, not in `Browse`, so a search from before the panel was
    /// closed and opened again is still older.
    search_generation: u64,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

/// The Theme row's hint, as the mockup words it (console.py's
/// `build_d_settings`).
pub(super) const THEME_HINT: &str =
    "Any Zed theme: yours from ~/.config/zed/themes, or one from Zed's theme extensions.";

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
            search_generation: 0,
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

    /// Changes one of agmen's own settings, which Zed has no say in (the
    /// light or dark mode, chat density), so Follow Zed stays as it is
    /// (ruling D14).
    fn change_own(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut AgmenSettings)) {
        edit(&mut self.settings);
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
        let market = Arc::new(Mutex::new(Marketplace::new(
            self.sources.market.clone(),
            self.sources.github.clone(),
        )?));
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
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        if let Some(browse) = &mut self.browse {
            browse.query.clone_from(&query);
        }
        self.set_status(BrowseStatus::Loading, cx);
        let task = cx.background_executor().spawn(async move {
            market
                .lock()
                .map_err(|_| MarketError::Poisoned.to_string())?
                .search(&query, Instant::now())
                .map_err(|err| err.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| this.searched(generation, result, cx))
                // An error means Settings closed meanwhile: nothing to show.
                .unwrap_or(());
        })
        .detach();
    }

    /// Shows the answer to search number `generation`.
    fn searched(
        &mut self,
        generation: u64,
        result: Result<Vec<ThemeExtension>, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.search_generation {
            // An older search, or the panel closed: a newer answer counts.
            return;
        }
        match result {
            Ok(results) => {
                if let Some(browse) = &mut self.browse {
                    browse.results = results;
                }
                self.set_status(BrowseStatus::Ready, cx);
            }
            Err(err) => self.set_status(BrowseStatus::Failed(err), cx),
        }
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
            // C305: the lock is released while GitHub is asked. C311: the
            // names are read under it, after the write.
            install_shared(&market, &ext, &dir, Instant::now()).map_err(|err| err.to_string())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok((installed, names)) => this.installed(&installed, &names, cx),
                Err(err) => this.set_status(BrowseStatus::Failed(err), cx),
            })
            // An error means Settings closed meanwhile: nothing to show.
            .unwrap_or(());
        })
        .detach();
    }

    /// Lists a fresh install, whose theme files name `names`, and switches
    /// to its first listed theme.
    fn installed(&mut self, installed: &InstalledTheme, names: &[String], cx: &mut Context<Self>) {
        self.rescan();
        let (first, status) = Self::install_outcome(installed, names, &self.themes);
        self.set_status(status, cx);
        if let Some(name) = first {
            self.change(cx, |s| s.theme = Some(name));
        }
    }

    /// What an install shows once `themes` is rescanned: its first listed
    /// theme, if any, and the status. A name another theme already has is
    /// not listed for the install, so the note names those (C210, C236).
    fn install_outcome(
        installed: &InstalledTheme,
        names: &[String],
        themes: &[ThemeEntry],
    ) -> (Option<String>, BrowseStatus) {
        let ours = |t: &ThemeEntry| matches!(&t.origin, ThemeOrigin::Agmen(theme) if theme.id == installed.id);
        let first = themes.iter().find(|t| ours(t)).map(|t| t.name.clone());
        let taken: Vec<&ThemeEntry> = names
            .iter()
            .filter_map(|name| themes.iter().find(|t| &t.name == name && !ours(t)))
            .collect();
        let title = installed.name.as_deref().unwrap_or(&installed.id);
        let listed = taken
            .iter()
            .map(|t| format!("\u{201c}{}\u{201d} ({})", t.name, rows::installed_detail(t)))
            .collect::<Vec<_>>()
            .join(", ");
        let status = match (&first, taken.is_empty()) {
            (Some(_), true) => BrowseStatus::Ready,
            (Some(_), false) => BrowseStatus::Installed(format!(
                "Installed {title}; agmen keeps using the themes already named {listed}"
            )),
            (None, false) => BrowseStatus::Installed(format!(
                "Installed {title}, but every theme in it already names a theme agmen keeps using: {listed}"
            )),
            (None, true) => BrowseStatus::Failed(format!(
                "Installed {title}, but none of its themes could be read"
            )),
        };
        (first, status)
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
        let rows = PAGES.iter().map(|&(page, title, slug, icon)| {
            let on = page == self.page;
            let color = if on {
                Color::Default
            } else {
                Color::Placeholder
            };
            ListItem::new(format!("settings-page:{slug}"), Indent::Page)
                .height(SETTINGS_PAGE_ROW_HEIGHT)
                .selected(on)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.page = page;
                    this.menu = None;
                    cx.notify();
                }))
                .leading(Icon::new(icon).size(IconSize::Medium).color(color.clone()))
                .child(
                    Label::new(title)
                        .color(color)
                        .when(on, |label| label.weight(FontWeight::MEDIUM)),
                )
        });
        div()
            .debug_selector(|| "settings-pages".into())
            .w(SETTINGS_PAGES_WIDTH)
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .py(SPACE_12)
            .px(SPACE_8)
            .border_r(BORDER_WIDTH)
            // Node 8049: the page list's rule is a chrome rule (V99).
            .border_color(super::rule_color(cx))
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
        setting("Theme", THEME_HINT, cx)
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
                                        this.change_own(cx, |s| s.mode = mode);
                                    })),
                            ),
                    )
                    .child(
                        Link::new(
                            "browse-themes",
                            if self.browse.is_some() {
                                "Close Zed themes"
                            } else {
                                "Browse Zed themes\u{2026}"
                            },
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_browse(window, cx)),
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
        // Right under the last rule, as node 8214 (V102).
        div()
            .debug_selector(|| "settings-footer-text".into())
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
        let border = theme.border;
        let status = match &browse.status {
            BrowseStatus::Loading => {
                Some(Label::new("Asking Zed's extension list\u{2026}").color(Color::Placeholder))
            }
            BrowseStatus::Installing(name) => {
                Some(Label::new(format!("Installing {name}\u{2026}")).color(Color::Placeholder))
            }
            BrowseStatus::Installed(note) | BrowseStatus::Failed(note) => {
                Some(Label::new(note.clone()).color(Color::Warning).wrap())
            }
            BrowseStatus::Ready => None,
        };
        let entries = picker_entries(&self.themes, &browse.results);
        let more = entries.len().saturating_sub(MAX_BROWSE_ROWS);
        let rows = entries
            .into_iter()
            .take(MAX_BROWSE_ROWS)
            .enumerate()
            .map(|(ix, entry)| browse_row(ix, entry, cx));
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
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(SPACE_3)
                        .child(
                            Label::new("Themes from Zed extensions at")
                                .size(LabelSize::Small)
                                .color(Color::Placeholder),
                        )
                        .child(
                            Link::new("zed-extensions-link", EXTENSIONS_PAGE)
                                .on_click(|_, _, cx| cx.open_url(EXTENSIONS_PAGE)),
                        ),
                )
                .child(
                    Label::new(
                        "Each keeps its author and license; when agmen finds no license, it says so.",
                    )
                    .size(LabelSize::Small)
                    .color(Color::Placeholder)
                    .wrap(),
                )
                .child(InputFrame::new().child(browse.search.clone()))
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

/// The icon theme preview's files and folders: the mockup's eight.
pub(super) const ICON_PREVIEW_NAMES: [(&str, bool); 8] = [
    ("src", true),
    ("tests", true),
    ("main.rs", false),
    ("Cargo.toml", false),
    ("README.md", false),
    ("Cargo.lock", false),
    ("reviewer.xml", false),
    ("release.yml", false),
];

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
        let open_file = Button::new("open-settings-toml", "Open settings.toml")
            .style(ButtonStyle::Plain)
            .on_click(
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
                                div().debug_selector(|| "settings-title".into()).child(
                                    Label::new(title)
                                        .size(LabelSize::Title)
                                        .weight(FontWeight::SEMIBOLD)
                                        .color(Color::TextMuted),
                                ),
                            )
                            .children(body)
                            .children(error),
                    ),
            )
    }
}

#[path = "settings_rows.rs"]
mod rows;

#[cfg(test)]
#[path = "settings_view_tests.rs"]
mod tests;
