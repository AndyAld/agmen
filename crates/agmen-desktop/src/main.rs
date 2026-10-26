//! The agmen desktop app: a GPUI console with Zed's layout.

mod appearance;
mod assets;
#[expect(
    dead_code,
    reason = "views send and act through Backend from Task 9 (chat composer) of the desktop plan"
)]
mod backend;
#[expect(
    dead_code,
    reason = "file icons get their first view in Task 11 (Project panel) of the desktop plan"
)]
mod icons;
#[cfg(unix)]
mod instance;
mod notify;
mod spike;
mod ui;

use std::rc::Rc;

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext as _, Application, Bounds, Focusable as _, KeyBinding, TitlebarOptions,
    WindowAppearance, WindowBounds, WindowOptions, point, px, size,
};

use appearance::{Appearance, SystemMode};
use backend::Backend as _;

use ui::{Quit, ToggleLeftDock, ToggleRightDock, Workspace};

const USAGE: &str =
    "usage: agmen-desktop [--screen <name> | --spike list [--autoscroll] | --spike notify]";

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    /// The normal app window.
    App,
    /// Spike S8's list check, optionally scrolling by itself and quitting.
    ListSpike { autoscroll: bool },
    /// Spike S8's notification check: post one, then quit.
    NotifySpike,
    /// The app showing one mockup screen's fixture state, for screenshots.
    Screen(backend::Screen),
}

impl Launch {
    fn parse(args: &[String]) -> Result<Self> {
        match args {
            [] => Ok(Self::App),
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
            [flag, name] if flag == "--screen" => name
                .parse()
                .map(Self::Screen)
                .map_err(|err: String| anyhow::anyhow!(err)),
            _ => anyhow::bail!(USAGE),
        }
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
    // Spikes and screenshot screens run beside a normal app, so only the app
    // takes the instance lock.
    let listener = if launch == Launch::App {
        let dir = std::env::home_dir()
            .context("no home folder")?
            .join("Library/Application Support/agmen");
        match instance::acquire(&dir, &args).context("checking for a running agmen-desktop")? {
            instance::Instance::Primary(listener) => Some(listener),
            instance::Instance::Secondary => return Ok(()),
        }
    } else {
        None
    };
    let platform = Rc::new(gpui_macos::MacPlatform::new(false));
    Application::with_platform(platform)
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
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
                cx.update(|cx| cx.activate(true));
            }
        }
    })
    .detach();
}

fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("agmen".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(12.))),
        }),
        ..Default::default()
    }
}

fn open_main_window(launch: &Launch, cx: &mut App) -> Result<()> {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleLeftDock, None),
        KeyBinding::new("cmd-r", ToggleRightDock, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
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
    ui::bind_keys(cx);

    let options = window_options(cx);
    let window = match launch {
        Launch::App | Launch::Screen(_) => {
            let screen = match launch {
                Launch::Screen(screen) => *screen,
                _ => backend::Screen::Chat,
            };
            // shortcut: the fixture stands in for agmend until its local API exists (ADR-0011).
            let snapshot = backend::FixtureBackend::for_screen(screen).snapshot();
            cx.open_window(options, |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(snapshot, window, cx));
                window.focus(&workspace.focus_handle(cx), cx);
                workspace
            })
            .map(Into::into)
        }
        Launch::NotifySpike => return Ok(()),
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
        assert!(Launch::parse(&args(&["--spike", "nope"])).is_err());
        assert!(Launch::parse(&args(&["--bogus"])).is_err());
    }
}
