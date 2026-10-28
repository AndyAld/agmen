//! Native notifications.
//!
//! GPUI posts through `UNUserNotificationCenter`, which macOS only delivers
//! for an app bundle with a bundle id. `cargo run` builds are bare binaries,
//! so they post through `notify-rust` (the deprecated
//! `NSUserNotificationCenter`, which still works without a bundle).

use std::path::Path;

use gpui::{App, SystemNotification};

/// Whether `exe` sits inside a macOS app bundle (`X.app/Contents/MacOS/x`).
fn in_app_bundle(exe: &Path) -> bool {
    let mut parts = exe.components().rev().skip(1);
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(macos), Some(contents), Some(app))
            if macos.as_os_str() == "MacOS"
                && contents.as_os_str() == "Contents"
                && Path::new(app.as_os_str()).extension().is_some_and(|e| e == "app")
    )
}

/// Shows a notification without blocking the UI thread. `tag` replaces an
/// earlier notification with the same tag where the platform can.
pub(crate) fn notify(tag: &str, title: &str, body: &str, cx: &App) {
    let bundled = std::env::current_exe().is_ok_and(|exe| in_app_bundle(&exe));
    if bundled {
        cx.show_system_notification(SystemNotification {
            tag: tag.to_owned().into(),
            title: title.to_owned().into(),
            body: body.to_owned().into(),
            actions: Vec::new(),
        });
        return;
    }
    post_unbundled(title.to_owned(), body.to_owned(), cx);
}

#[cfg(target_os = "macos")]
fn post_unbundled(title: String, body: String, cx: &App) {
    cx.background_executor()
        .spawn(async move {
            if let Err(err) = notify_rust::Notification::new()
                .summary(&title)
                .body(&body)
                .show()
            {
                eprintln!("agmen-desktop: notification failed: {err}");
            }
        })
        .detach();
}

#[cfg(not(target_os = "macos"))]
fn post_unbundled(title: String, body: String, cx: &App) {
    // shortcut: Windows and Linux use GPUI's notifications until S8 runs there.
    cx.show_system_notification(SystemNotification {
        tag: "agmen".into(),
        title: title.into(),
        body: body.into(),
        actions: Vec::new(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_detection() {
        assert!(in_app_bundle(Path::new(
            "/Applications/agmen.app/Contents/MacOS/agmen-desktop"
        )));
        assert!(!in_app_bundle(Path::new(
            "/Users/a/agmen/target/debug/agmen-desktop"
        )));
        assert!(!in_app_bundle(Path::new(
            "/x/agmen.app/Contents/Resources/agmen-desktop"
        )));
        assert!(!in_app_bundle(Path::new("agmen-desktop")));
    }
}
