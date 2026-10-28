//! Native notifications.
//!
//! GPUI posts through `UNUserNotificationCenter`, which macOS only delivers
//! for an app bundle with a bundle id. `cargo run` builds are bare binaries,
//! so they post through `notify-rust` (the deprecated
//! `NSUserNotificationCenter`, which still works without a bundle).

use gpui::{App, SystemNotification};

use crate::bundle::in_app_bundle;

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
