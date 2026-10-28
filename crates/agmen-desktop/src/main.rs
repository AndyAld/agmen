//! The agmen desktop app: a GPUI console with Zed's layout.

mod alerts;
mod appearance;
mod assets;
mod backend;
mod bundle;
// Task 13: the urgent-alert chime.
mod chime;
mod cli_link;
mod icons;
#[cfg(unix)]
mod instance;
// Task 18: every key binding and the palette's entries.
mod keymap;
mod local_time;
mod notify;
mod plan;
mod project;
mod refs;
mod roles;
mod service;
mod spike;
mod ui;

use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext as _, Application, Bounds, Focusable as _, TitlebarOptions, WindowAppearance,
    WindowBounds, WindowOptions, point, px, size,
};

use appearance::{Appearance, SystemMode};

use ui::{Quit, Workspace};

const USAGE: &str = "usage: agmen-desktop [<path> | --version | --fixture | --screen <name> | --list-screens | --view <name> | --spike list [--autoscroll] | --spike notify]";

/// The views `--view` can show alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DevView {
    /// The code view on `roles/reviewer.xml`.
    Code,
    /// andy/fx1's session in the agent view.
    Agent,
    /// The Roles tab with reviewer.xml open.
    Roles,
    /// The Project tab with fx1's main.rs open.
    Project,
    /// The same with the tests folder open.
    ProjectTestsOpen,
    /// The Git tab with src/main.rs's diff open.
    Git,
    /// The same with the Changes section folded.
    GitCollapsed,
    /// Settings on Appearance.
    Settings,
}

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    /// Prints the workspace version and build without starting GPUI.
    Version,
    /// The normal app window, live through agmend (Task 7).
    App,
    /// An explicit folder, opened through the same path as the picker.
    OpenProject(PathBuf),
    /// The app window on the fixture's chat, as a plain launch opened it
    /// before agmend's local API existed.
    Fixture,
    /// Spike S8's list check, optionally scrolling by itself and quitting.
    ListSpike { autoscroll: bool },
    /// Spike S8's notification check: post one, then quit.
    NotifySpike,
    /// The app showing one mockup screen's fixture state, for screenshots.
    Screen(backend::Screen),
    /// Prints every screen name, one per line, for `scripts/shot.sh all`.
    ListScreens,
    /// One view alone in the window, to check it before a pane hosts it.
    View(DevView),
}

impl Launch {
    /// Resolve in the sender, before a running app with another cwd sees argv.
    fn forwarded_args(&self, args: &[String]) -> Result<Vec<String>> {
        match self {
            Self::OpenProject(path) if path.is_relative() => Ok(vec![
                std::env::current_dir()
                    .context("reading the folder to forward")?
                    .join(path)
                    .to_string_lossy()
                    .into_owned(),
            ]),
            _ => Ok(args.to_vec()),
        }
    }

    fn parse(args: &[String]) -> Result<Self> {
        match args {
            [] => Ok(Self::App),
            [flag] if flag == "--version" => Ok(Self::Version),
            [flag] if flag == "--fixture" => Ok(Self::Fixture),
            [path] if !path.starts_with('-') => Ok(Self::OpenProject(PathBuf::from(path))),
            [flag, name, rest @ ..] if flag == "--spike" && name == "list" => match rest {
                [] => Ok(Self::ListSpike { autoscroll: false }),
                [a] if a == "--autoscroll" => {
                    // Frame times from a debug build say nothing about the app.
                    anyhow::ensure!(
                        !cfg!(debug_assertions),
                        "--autoscroll measures frame times; run a release build"
                    );
                    Ok(Self::ListSpike { autoscroll: true })
                }
                _ => anyhow::bail!(USAGE),
            },
            [flag, name] if flag == "--spike" && name == "notify" => Ok(Self::NotifySpike),
            [flag] if flag == "--list-screens" => Ok(Self::ListScreens),
            [flag, name] if flag == "--view" => match name.as_str() {
                "code" => Ok(Self::View(DevView::Code)),
                "agent" => Ok(Self::View(DevView::Agent)),
                "roles" => Ok(Self::View(DevView::Roles)),
                "project" => Ok(Self::View(DevView::Project)),
                "project-tests-open" => Ok(Self::View(DevView::ProjectTestsOpen)),
                "git" => Ok(Self::View(DevView::Git)),
                "git-collapsed" => Ok(Self::View(DevView::GitCollapsed)),
                "settings" => Ok(Self::View(DevView::Settings)),
                _ => anyhow::bail!(
                    "views: code, agent, roles, project, project-tests-open, git, git-collapsed, settings"
                ),
            },
            [flag, name] if flag == "--screen" => name
                .parse()
                .map(Self::Screen)
                .map_err(|err: String| anyhow::anyhow!(err)),
            _ => anyhow::bail!(USAGE),
        }
    }
}

impl Launch {
    /// The fixture screen this launch opens; `None` for the live app and
    /// the windows that are not the app.
    const fn fixture_screen(&self) -> Option<backend::Screen> {
        match self {
            Self::Fixture => Some(backend::Screen::Chat),
            Self::Screen(screen) => Some(*screen),
            Self::App
            | Self::OpenProject(_)
            | Self::Version
            | Self::ListSpike { .. }
            | Self::NotifySpike
            | Self::ListScreens
            | Self::View(_) => None,
        }
    }

    /// Whether this launch is the person's app, which runs once: spikes
    /// and screenshot screens run beside it.
    const fn takes_instance_lock(&self) -> bool {
        matches!(self, Self::App | Self::OpenProject(_) | Self::Fixture)
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    // shortcut: macOS only until spike S8 runs on Windows and Linux (ADR-0010).
    eprintln!("agmen-desktop: only macOS is supported so far");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch = Launch::parse(&args)?;
    if launch == Launch::Version {
        println!(
            "agmen-desktop {} ({})",
            env!("CARGO_PKG_VERSION"),
            option_env!("AGMEN_BUILD").unwrap_or("dev")
        );
        return Ok(());
    }
    if launch == Launch::ListScreens {
        for screen in backend::Screen::ALL {
            println!("{}", screen.name());
        }
        return Ok(());
    }
    // Spikes and screenshot screens run beside a normal app, so only the app
    // takes the instance lock.
    let listener = if launch.takes_instance_lock() {
        let dir = std::env::home_dir()
            .context("no home folder")?
            .join("Library/Application Support/agmen");
        // The first instance may have a different working folder.
        let forwarded_args = launch.forwarded_args(&args)?;
        match instance::acquire(&dir, &forwarded_args)
            .context("checking for a running agmen-desktop")?
        {
            instance::Instance::Primary(listener) => Some(listener),
            instance::Instance::Secondary => return Ok(()),
        }
    } else {
        None
    };
    let platform = Rc::new(gpui_macos::MacPlatform::new(false));
    let app = Application::with_platform(platform).with_assets(assets::Assets);
    let (mut opened, mut files) = futures::channel::mpsc::channel(10);
    app.on_open_urls(move |urls| {
        if let Some(path) = file_open_path(&urls)
            && let Err(error) = opened.try_send(path)
        {
            eprintln!("agmen-desktop: file open queue is unavailable: {error}");
        }
    });
    app.run(move |cx: &mut App| {
        if launch == Launch::NotifySpike {
            notify::notify("spike", "agmen", "adam/coord needs you", cx);
            // Leave time for the background post before quitting.
            cx.spawn(async |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(3))
                    .await;
                cx.update(|cx| cx.quit());
            })
            .detach();
            return;
        }
        if let Some(listener) = listener {
            watch_instance(listener, cx);
        }
        if let Err(err) = open_main_window(&launch, cx) {
            // The run loop owns the process from here, so report and exit 1.
            eprintln!("agmen-desktop: {err:#}");
            std::process::exit(1);
        }
        cx.spawn(async move |cx| {
            use futures::StreamExt as _;
            while let Some(path) = files.next().await {
                cx.update(|cx| open_in_live_window(path, cx));
            }
        })
        .detach();
    });
    Ok(())
}

fn load_appearance(os: WindowAppearance) -> Appearance {
    let system = match os {
        WindowAppearance::Light | WindowAppearance::VibrantLight => SystemMode::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => SystemMode::Dark,
    };
    let Some(home) = std::env::home_dir() else {
        let mut builtin = Appearance::builtin();
        builtin
            .warnings
            .push("no home folder; using the built-in theme".into());
        return builtin;
    };
    let appearance = appearance::resolve(&home, system);
    for warning in &appearance.warnings {
        eprintln!("agmen-desktop: {warning}");
    }
    appearance
}

/// Brings the app forward when a second launch hands over.
fn watch_instance(listener: instance::Listener, cx: &mut App) {
    // shortcut: polls every 250 ms, so focus lags up to 250 ms and the app wakes 4 times a
    // second; switch to a channel the run loop wakes on when idle wakeups matter.
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(250))
                .await;
            if let Some(forwarded) = listener.try_recv()
                && forwarded.focus
            {
                cx.update(|cx| {
                    if let Some(path) = forwarded_folder(&forwarded.args) {
                        open_in_live_window(path, cx);
                    }
                    cx.activate(true);
                });
            }
        }
    })
    .detach();
}

/// The folder a second launch forwarded. Any same-user process can write to
/// the instance socket, so only an absolute folder argument opens anything.
fn forwarded_folder(args: &[String]) -> Option<PathBuf> {
    match Launch::parse(args) {
        Ok(Launch::OpenProject(path)) if path.is_absolute() => Some(path),
        _ => None,
    }
}

/// GPUI supplies file URLs for Dock folder drops. Use the first local file.
fn file_open_path(urls: &[String]) -> Option<PathBuf> {
    urls.iter()
        .take(10)
        .find_map(|url| reqwest::Url::parse(url).ok()?.to_file_path().ok())
}

fn open_in_live_window(path: PathBuf, cx: &mut App) {
    if let Some(window) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<ui::LiveRoot>())
        && let Err(error) = window.update(cx, |root, window, cx| root.open_folder(path, window, cx))
    {
        eprintln!("agmen-desktop: opening the folder failed: {error}");
    }
}

fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
    let background = appearance::window_background(&cx.global::<Appearance>().theme);
    WindowOptions {
        window_background: background,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("agmen".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(12.))),
        }),
        ..Default::default()
    }
}

/// Opens one view alone under the title bar, for `--view`.
fn open_dev_view(
    view: DevView,
    options: WindowOptions,
    cx: &mut App,
) -> Result<gpui::AnyWindowHandle> {
    use backend::Backend as _;
    let mut snapshot = backend::FixtureBackend::for_screen(backend::Screen::Chat)
        .context("building the fixture data")?
        .snapshot();
    let title = ui::TitleBar::new(&snapshot);
    let window = match view {
        DevView::Code => cx.open_window(options, |_, cx| {
            let code = cx.new(|_| {
                ui::CodeView::new(
                    backend::REVIEWER_XML,
                    ui::Language::for_path(std::path::Path::new("roles/reviewer.xml")),
                    vec![ui::GutterMark::Modified(11..12)],
                )
            });
            cx.new(|_| ui::DevFrame::new(title, code))
        }),
        DevView::Agent => {
            let (id, session) = snapshot
                .sessions
                .pop_first()
                .context("the fixture has no agent session")?;
            cx.open_window(options, |window, cx| {
                let agent = cx.new(|cx| ui::AgentView::new(&snapshot, &id, session, window, cx));
                cx.new(|_| ui::DevFrame::new(title, agent))
            })
        }
        DevView::Roles => {
            let roles = snapshot.roles;
            let reviewer = roles
                .iter()
                .find(|role| role.name == "reviewer")
                .context("the fixture has no reviewer role")?
                .clone();
            cx.open_window(options, |_, cx| {
                let view = cx.new(|cx| ui::RolesDev::new(roles, reviewer, cx));
                cx.new(|_| ui::DevFrame::new(title, view))
            })
        }
        DevView::Project | DevView::ProjectTestsOpen => {
            let tests = if view == DevView::Project {
                ui::ProjectTests::Folded
            } else {
                ui::ProjectTests::Open
            };
            cx.open_window(options, |_, cx| {
                let view = cx.new(|cx| ui::ProjectDev::new(snapshot, tests, cx));
                cx.new(|_| ui::DevFrame::new(title, view))
            })
        }
        DevView::Git | DevView::GitCollapsed => {
            let layout = if view == DevView::Git {
                ui::GitDevLayout::Open
            } else {
                ui::GitDevLayout::ChangesFolded
            };
            let scopes = ui::ScopeOption::for_worktrees(&snapshot.worktrees, &snapshot.agents);
            let (worktrees, branches) = (snapshot.worktrees, snapshot.git.branches);
            cx.open_window(options, |_, cx| {
                let view = cx.new(|cx| ui::GitDev::new(worktrees, branches, scopes, layout, cx));
                cx.new(|_| ui::DevFrame::new(title, view))
            })
        }
        DevView::Settings => {
            let home = std::env::home_dir().context("no home folder")?;
            let sources = ui::SettingsSources::for_home(&home);
            cx.open_window(options, |_, cx| {
                let view = cx.new(|cx| ui::SettingsView::new(sources, cx));
                cx.new(|_| ui::DevFrame::new(title, view))
            })
        }
    };
    Ok(window.context("opening the view window")?.into())
}

/// A plain launch restores its saved project. Explicit folders resolve
/// through the live window's folder opener.
fn live_launch(explicit: bool) -> Result<ui::LiveLaunch> {
    let socket = agmen_client::agmend_socket().context("finding agmend's socket")?;
    let settings =
        std::env::home_dir().map(|home| appearance::agmen_settings_path(&home.join(".config")));
    let (project, root) = if explicit {
        (project::ProjectChoice::None { why: None }, None)
    } else {
        project::choose_saved_project(settings.as_deref())
    };
    Ok(ui::LiveLaunch {
        socket,
        project,
        root,
        settings,
    })
}

fn open_main_window(launch: &Launch, cx: &mut App) -> Result<()> {
    keymap::bind_all(cx);
    cx.set_menus([gpui::Menu::new("agmen").items([gpui::MenuItem::action("Quit agmen", Quit)])]);
    if !matches!(launch, Launch::App | Launch::OpenProject(_)) {
        cx.on_action(|_: &Quit, cx| cx.quit());
    }
    assets::load_fonts(cx).context("loading the bundled fonts")?;
    let mut appearance = load_appearance(cx.window_appearance());
    let extensions = std::env::home_dir()
        .map(|home| appearance::zed_extensions_dir(&home))
        .unwrap_or_default();
    let (file_icons, warning) = icons::FileIcons::new(&appearance.icon_theme, &extensions);
    if let Some(warning) = warning {
        eprintln!("agmen-desktop: {warning}");
        appearance.warnings.push(warning);
    }
    cx.set_global(appearance);
    cx.set_global(file_icons);
    cx.set_global(local_time::LocalZone::system());
    // Without a home folder the Settings tab shows a placeholder.
    if let Some(home) = std::env::home_dir() {
        cx.set_global(ui::SettingsSources::for_home(&home));
    }

    let options = window_options(cx);
    let window = match launch {
        Launch::App | Launch::OpenProject(_) => {
            let path = match launch {
                Launch::OpenProject(path) => Some(path.clone()),
                _ => None,
            };
            let launch = live_launch(path.is_some())?;
            // Task 13: live alerts chime and post notifications.
            cx.set_global(ui::AlarmHandle::system());
            cx.open_window(options, |window, cx| {
                let root = cx.new(|cx| ui::LiveRoot::new(launch, path, window, cx));
                let weak = root.downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    weak.update(cx, |root, cx| {
                        if root.quit_approved {
                            return true;
                        }
                        root.request_quit(window, cx);
                        false
                    })
                    .unwrap_or(true)
                });
                let weak = root.downgrade();
                let handle = window.window_handle();
                cx.on_action(move |_: &Quit, cx| {
                    drop(handle.update(cx, |_, window, cx| {
                        drop(weak.update(cx, |root, cx| root.request_quit(window, cx)));
                    }));
                });
                window.focus(&root.focus_handle(cx), cx);
                root
            })
            .map(Into::into)
        }
        Launch::Fixture | Launch::Screen(_) => {
            let Some(screen) = launch.fixture_screen() else {
                return Ok(());
            };
            // The fixture serves --fixture and --screen, for tests and
            // screenshots (ADR-0011); a plain launch is live.
            let backend: Rc<dyn backend::Backend> = Rc::new(
                backend::FixtureBackend::for_screen(screen).context("building the fixture data")?,
            );
            let snapshot = backend.snapshot();
            cx.set_global(backend::BackendHandle(backend));
            // Task 13: live alerts chime and post notifications.
            cx.set_global(ui::AlarmHandle::system());
            cx.open_window(options, |window, cx| {
                let workspace = cx.new(|cx| {
                    let mut workspace = Workspace::new(snapshot, window, cx);
                    workspace.show_screen(screen, cx);
                    workspace
                });
                window.focus(&workspace.focus_handle(cx), cx);
                workspace
            })
            .map(Into::into)
        }
        Launch::Version | Launch::NotifySpike | Launch::ListScreens => return Ok(()),
        Launch::View(view) => Ok(open_dev_view(*view, options, cx)?),
        Launch::ListSpike { autoscroll } => {
            let autoscroll = *autoscroll;
            cx.open_window(options, |_, cx| {
                cx.new(|cx| spike::ListSpike::new(autoscroll, cx))
            })
            .map(Into::into)
        }
    };
    let window: gpui::AnyWindowHandle = window.context("opening the main window")?;
    window
        .update(cx, |_, window, _| window.activate_window())
        .context("activating the main window")?;
    cx.activate(true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parse_launch_args() {
        assert_eq!(Launch::parse(&[]).unwrap(), Launch::App);
        assert_eq!(
            Launch::parse(&args(&["--spike", "list"])).unwrap(),
            Launch::ListSpike { autoscroll: false }
        );
        assert_eq!(
            Launch::parse(&args(&["--spike", "list", "--autoscroll"])).is_ok(),
            !cfg!(debug_assertions),
            "autoscroll runs only in release builds"
        );
        assert_eq!(
            Launch::parse(&args(&["--spike", "notify"])).unwrap(),
            Launch::NotifySpike
        );
        assert_eq!(
            Launch::parse(&args(&["--screen", "needs-you"])).unwrap(),
            Launch::Screen(backend::Screen::NeedsYou)
        );
        assert!(Launch::parse(&args(&["--screen", "nope"])).is_err());
        for (name, view) in [
            ("code", DevView::Code),
            ("agent", DevView::Agent),
            ("roles", DevView::Roles),
            ("project", DevView::Project),
            ("project-tests-open", DevView::ProjectTestsOpen),
            ("git", DevView::Git),
            ("git-collapsed", DevView::GitCollapsed),
            ("settings", DevView::Settings),
        ] {
            assert_eq!(
                Launch::parse(&args(&["--view", name])).unwrap(),
                Launch::View(view)
            );
        }
        assert!(Launch::parse(&args(&["--view", "nope"])).is_err());
        assert!(USAGE.contains("--view <name>"));
        assert_eq!(
            Launch::parse(&args(&["--list-screens"])).unwrap(),
            Launch::ListScreens
        );
        assert!(Launch::parse(&args(&["--spike", "nope"])).is_err());
        assert!(Launch::parse(&args(&["--bogus"])).is_err());
    }

    #[test]
    fn open_project_launch_argument() {
        let launch = Launch::parse(&args(&["/tmp/a project"]));
        assert!(launch.is_ok(), "a folder is a live launch: {launch:?}");
        let launch = launch.unwrap();
        assert!(launch.takes_instance_lock());
        assert_eq!(launch.fixture_screen(), None);
    }

    #[test]
    fn relative_launch_is_absolute_before_instance_forwarding() {
        let dir = tempfile::tempdir().unwrap();
        let instance::Instance::Primary(listener) = instance::acquire(dir.path(), &[]).unwrap()
        else {
            panic!("expected primary");
        };
        let original = args(&["relative-folder"]);
        let launch = Launch::parse(&original).unwrap();
        let forwarded = launch.forwarded_args(&original).unwrap();
        assert!(std::path::Path::new(&forwarded[0]).is_absolute());
        assert!(matches!(
            instance::acquire(dir.path(), &forwarded).unwrap(),
            instance::Instance::Secondary
        ));
        let received = listener
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            Launch::parse(&received.args).unwrap(),
            Launch::OpenProject(std::env::current_dir().unwrap().join("relative-folder"))
        );
    }

    #[test]
    fn forwarded_folder_must_be_absolute() {
        assert_eq!(
            forwarded_folder(&args(&["/tmp/a project"])),
            Some(PathBuf::from("/tmp/a project"))
        );
        assert_eq!(forwarded_folder(&args(&["relative-folder"])), None);
        assert_eq!(forwarded_folder(&args(&["--fixture"])), None);
        assert_eq!(forwarded_folder(&args(&[])), None);
    }

    #[test]
    fn fixture_flag_keeps_the_fixture() {
        // Task 7: a plain launch is live; --fixture opens the fixture's
        // chat as a plain launch did before, and --screen is unchanged.
        assert_eq!(
            Launch::parse(&args(&["--fixture"])).unwrap(),
            Launch::Fixture
        );
        assert_eq!(
            Launch::Fixture.fixture_screen(),
            Some(backend::Screen::Chat)
        );
        assert_eq!(Launch::App.fixture_screen(), None);
        assert_eq!(
            Launch::Screen(backend::Screen::NeedsYou).fixture_screen(),
            Some(backend::Screen::NeedsYou)
        );
        assert!(USAGE.contains("--fixture"));
        // Only the windows a person opens take the instance lock.
        assert!(Launch::App.takes_instance_lock());
        assert!(Launch::Fixture.takes_instance_lock());
        assert!(!Launch::Screen(backend::Screen::Chat).takes_instance_lock());
    }
}
