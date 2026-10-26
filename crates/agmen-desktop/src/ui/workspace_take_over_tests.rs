use std::rc::Rc;

use gpui::{Entity, SharedString, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{Backend, BackendHandle, FixtureBackend, Screen, fixture_id};
use crate::ui::keys::{OpenInTerminal, TakeOverAgent, ToggleTerminal};
use crate::ui::test_support::{init, redraw};

/// The workspace over `screen`'s fixture, acting through it, as `--screen`
/// opens it.
fn open(
    cx: &mut TestAppContext,
    screen: Screen,
) -> (
    Entity<Workspace>,
    Rc<FixtureBackend>,
    &mut VisualTestContext,
) {
    init(cx);
    let backend = Rc::new(FixtureBackend::for_screen(screen).unwrap());
    let handle: Rc<dyn Backend> = backend.clone();
    cx.update(|cx| cx.set_global(BackendHandle(handle)));
    let snapshot = backend.snapshot();
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(snapshot, window, cx);
        workspace.show_screen(screen, cx);
        workspace
    });
    workspace.update_in(cx, |w, window, cx| w.focus_handle.focus(window, cx));
    settle(cx);
    (workspace, backend, cx)
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        redraw(cx);
    }
}

fn status(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<SharedString> {
    workspace.read_with(cx, |w, cx| w.status_context(cx))
}

#[gpui::test]
fn take_over_screens_say_taken_over_in_the_status_bar(cx: &mut TestAppContext) {
    // V66: node 1265, "andy/fx1 · taken over".
    let (workspace, _, cx) = open(cx, Screen::ChatTerminal);
    assert_eq!(status(&workspace, cx), Some("andy/fx1 · taken over".into()));
    assert!(cx.debug_bounds("terminal").is_some());
}

#[gpui::test]
fn switch_agent_screen_opens_the_menu(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Screen::SwitchAgent);
    assert!(cx.debug_bounds("switch-menu").is_some());
    assert!(cx.debug_bounds("add-agent").is_none());
}

#[gpui::test]
fn add_agent_screen_opens_grok_over_the_menu(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Screen::AddAgent);
    assert!(cx.debug_bounds("switch-menu").is_some());
    assert!(cx.debug_bounds("add-agent:prompt").is_some());
}

#[gpui::test]
fn keys_take_over_toggle_and_the_panel_follows(cx: &mut TestAppContext) {
    let (workspace, backend, cx) = open(cx, Screen::Chat);
    let fx1 = fixture_id("andy/fx1");
    assert_eq!(status(&workspace, cx), None);

    cx.dispatch_action(OpenInTerminal);
    settle(cx);
    assert!(backend.snapshot().take_overs.contains_key(&fx1));
    assert!(cx.debug_bounds("terminal").is_some());
    assert_eq!(status(&workspace, cx), Some("andy/fx1 · taken over".into()));
    assert!(workspace.read_with(cx, |w, _| w.snapshot.take_overs.contains_key(&fx1)));

    cx.dispatch_action(ToggleTerminal);
    settle(cx);
    assert!(cx.debug_bounds("terminal").is_none());
    cx.dispatch_action(ToggleTerminal);
    settle(cx);
    assert!(cx.debug_bounds("terminal").is_some());
}

#[gpui::test]
fn the_agent_views_button_takes_over_in_the_chat(cx: &mut TestAppContext) {
    let (workspace, backend, cx) = open(cx, Screen::Agent);
    let fx1 = fixture_id("andy/fx1");
    cx.dispatch_action(TakeOverAgent(fx1.clone()));
    settle(cx);
    assert!(backend.snapshot().take_overs.contains_key(&fx1));
    let active = workspace.read_with(cx, |w, cx| w.pane_group.read(cx).active_item(cx));
    assert_eq!(active, Some(TabItem::Chat));
    assert!(cx.debug_bounds("terminal").is_some());
}
