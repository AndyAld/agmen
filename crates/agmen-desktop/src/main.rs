//! The agmen desktop app: a GPUI console with Zed's layout.

mod ui;

use std::rc::Rc;

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext as _, Application, Bounds, Focusable as _, KeyBinding, TitlebarOptions,
    WindowBounds, WindowOptions, point, px, size,
};

use ui::{Quit, ToggleLeftDock, ToggleRightDock, Workspace};

#[cfg(not(target_os = "macos"))]
fn main() {
    // shortcut: macOS only until spike S8 runs on Windows and Linux (ADR-0010).
    eprintln!("agmen-desktop: only macOS is supported so far");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    let platform = Rc::new(gpui_macos::MacPlatform::new(false));
    Application::with_platform(platform).run(|cx: &mut App| {
        if let Err(err) = open_main_window(cx) {
            // The run loop owns the process from here, so report and quit.
            eprintln!("agmen-desktop: {err:#}");
            cx.quit();
        }
    });
}

fn open_main_window(cx: &mut App) -> Result<()> {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleLeftDock, None),
        KeyBinding::new("cmd-r", ToggleRightDock, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());

    let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
    let window = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("agmen".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(12.), px(12.))),
                }),
                ..Default::default()
            },
            |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(window, cx));
                window.focus(&workspace.focus_handle(cx), cx);
                workspace
            },
        )
        .context("opening the main window")?;
    window
        .update(cx, |_, window, _| window.activate_window())
        .context("activating the main window")?;
    cx.activate(true);
    Ok(())
}
