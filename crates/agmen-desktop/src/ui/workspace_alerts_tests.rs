use std::rc::Rc;

use agmen_proto::StampedEnvelope;
use gpui::{Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{Backend, BackendHandle, FixtureBackend, Screen};
use crate::ui::chat::AlertCounts;
use crate::ui::pane::TabItem;
use crate::ui::status_bar::StatusBar;
use crate::ui::test_support::{click, draw, init, redraw};
use crate::ui::tokens::{SPACE_16, TOAST_TOP};

/// The workspace `--screen` opens for `screen`, with the fixture as its
/// backend.
fn open(cx: &mut TestAppContext, screen: Screen) -> (Entity<Workspace>, &mut VisualTestContext) {
    init(cx);
    let backend = Rc::new(FixtureBackend::for_screen(screen).unwrap());
    let snapshot = backend.snapshot();
    let handle: Rc<dyn Backend> = backend;
    cx.update(|cx| cx.set_global(BackendHandle(handle)));
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(snapshot, window, cx);
        workspace.show_screen(screen, cx);
        workspace
    });
    redraw(cx);
    (workspace, cx)
}

fn urgent_alert() -> StampedEnvelope {
    FixtureBackend::for_screen(Screen::NeedsYou)
        .unwrap()
        .snapshot()
        .messages
        .into_iter()
        .find(|m| m.from_handle == "adam/coord")
        .unwrap()
}

fn sel(env: &StampedEnvelope, part: &str) -> &'static str {
    Box::leak(format!("{}:{part}", env.client.id.as_str()).into_boxed_str())
}

#[gpui::test]
fn needs_you_screen_shows_toast_chime_chip_pill_and_badge(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, Screen::NeedsYou);
    for part in [
        "toast",
        "banner",
        "banner:chime",
        "needs-you-pill",
        "pane-0:tab-0:badge",
    ] {
        assert!(cx.debug_bounds(part).is_some(), "{part} is drawn");
    }
}

#[gpui::test]
fn answered_screen_shows_the_answered_banner_only(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, Screen::NeedsYouAnswered);
    assert!(cx.debug_bounds("banner").is_some());
    for part in [
        "toast",
        "banner:jump",
        "needs-you-pill",
        "pane-0:tab-0:badge",
    ] {
        assert!(cx.debug_bounds(part).is_none(), "{part} is not drawn");
    }
    assert!(cx.debug_bounds("status-alerts").is_some());
}

#[gpui::test]
fn chat_screen_shows_no_alert_state(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, Screen::Chat);
    for part in ["toast", "banner", "needs-you-pill", "pane-0:tab-0:badge"] {
        assert!(cx.debug_bounds(part).is_none(), "{part} is not drawn");
    }
}

#[gpui::test]
fn toast_floats_at_the_windows_top_right(cx: &mut TestAppContext) {
    // Node 6004: 16 px from the right edge, its top at y 150.
    let (_, cx) = open(cx, Screen::NeedsYou);
    let toast = cx.debug_bounds("toast").unwrap();
    let window = cx.update(|window, _| window.viewport_size());
    assert_eq!(window.width - toast.right(), SPACE_16);
    assert_eq!(toast.top(), TOAST_TOP);
}

#[gpui::test]
fn the_status_bar_follows_the_alert_state(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, Screen::NeedsYou);
    assert!(cx.debug_bounds("needs-you-pill").is_some());
    click(cx, sel(&urgent_alert(), "alert:snooze"));
    redraw(cx);
    assert!(cx.debug_bounds("needs-you-pill").is_none());
    assert!(cx.debug_bounds("pane-0:tab-0:badge").is_none());
    assert!(
        cx.debug_bounds("toast").is_none(),
        "the toast goes with its alert"
    );
}

#[gpui::test]
fn clicking_the_toast_opens_the_chat_at_the_alert(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx, Screen::NeedsYou);
    // Another tab is active when the toast is clicked.
    workspace.update(cx, |w, cx| {
        w.pane_group.update(cx, |group, cx| {
            group.open(TabItem::Settings(Default::default()), cx)
        });
    });
    redraw(cx);
    assert!(
        cx.debug_bounds("banner").is_none(),
        "the chat is not showing"
    );
    click(cx, "toast");
    redraw(cx);
    let active = workspace.read_with(cx, |w, cx| w.pane_group.read(cx).active_item(cx));
    assert_eq!(active, Some(TabItem::Chat));
    assert!(cx.debug_bounds("toast").is_none());
    assert!(cx.debug_bounds(sel(&urgent_alert(), "row")).is_some());
}

#[gpui::test]
fn clicking_the_alert_count_opens_the_chat_at_the_alert(cx: &mut TestAppContext) {
    // Node 456 links the status bar's "2 alerts" to the needs-you frame.
    let (workspace, cx) = open(cx, Screen::NeedsYou);
    workspace.update(cx, |w, cx| {
        w.pane_group.update(cx, |group, cx| {
            group.open(TabItem::Settings(Default::default()), cx)
        });
    });
    redraw(cx);
    assert!(cx.debug_bounds(sel(&urgent_alert(), "row")).is_none());
    click(cx, "status-alerts");
    redraw(cx);
    let active = workspace.read_with(cx, |w, cx| w.pane_group.read(cx).active_item(cx));
    assert_eq!(active, Some(TabItem::Chat));
    assert!(
        cx.debug_bounds(sel(&urgent_alert(), "row")).is_some(),
        "the alert that needs me comes first"
    );
}

#[gpui::test]
fn large_counts_read_99_plus(cx: &mut TestAppContext) {
    let bar = |needs_you: usize| {
        move || {
            StatusBar::new(&crate::backend::chat_snapshot(), true)
                .alert_counts(AlertCounts {
                    needs_you,
                    alerts: needs_you,
                })
                .into_any_element()
        }
    };
    let pill = |cx: &mut TestAppContext, n| {
        let cx = draw(cx, bar(n));
        (
            cx.debug_bounds("needs-you-pill").unwrap().size.width,
            cx.debug_bounds("status-alerts").unwrap().size.width,
        )
    };
    let capped = pill(cx, 100);
    let mut cx2 = TestAppContext::single();
    let huge = pill(&mut cx2, 4_000);
    let mut cx3 = TestAppContext::single();
    let two_digits = pill(&mut cx3, 99);
    assert_eq!(huge, capped);
    assert!(capped.0 > two_digits.0);
}
