use std::rc::Rc;

use agmen_proto::StampedEnvelope;
use gpui::{
    AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render,
    Styled as _, TestAppContext, VisualTestContext, Window, div, px,
};

use crate::backend::{Backend, BackendHandle, FixtureBackend, Screen, Snapshot, fixture_id};
use crate::ui::chat::ChatView;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::MOCKUP_REM;

/// The chat in a 1000 by 900 box, as the center of the 1440 window.
struct Host(Entity<ChatView>);

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div()
            .w(px(1000.))
            .h(px(900.))
            .flex()
            .flex_col()
            .child(self.0.clone())
    }
}

/// The chat over `screen`'s fixture, acting through it.
fn open(
    cx: &mut TestAppContext,
    screen: Screen,
) -> (Entity<ChatView>, Rc<FixtureBackend>, &mut VisualTestContext) {
    init(cx);
    // Every binding, so Escape reaches the menu and the dialog.
    cx.update(crate::keymap::bind_all);
    let backend = Rc::new(FixtureBackend::for_screen(screen).unwrap());
    let handle: Rc<dyn Backend> = backend.clone();
    cx.update(|cx| cx.set_global(BackendHandle(handle)));
    let snapshot = backend.snapshot();
    let (host, cx) = cx.add_window_view(|window, cx| {
        Host(cx.new(|cx| ChatView::new(std::rc::Rc::new(snapshot), window, cx)))
    });
    let chat = host.read_with(cx, |host, _| host.0.clone());
    settle(cx);
    (chat, backend, cx)
}

/// Lets the backend's updates and deferred focus land, then draws.
fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    redraw(cx);
    redraw(cx);
}

fn sel(env: &StampedEnvelope, part: &str) -> &'static str {
    Box::leak(format!("{}:{part}", env.client.id.as_str()).into_boxed_str())
}

fn fx1_done(snapshot: &Snapshot) -> StampedEnvelope {
    snapshot
        .messages
        .iter()
        .find(|m| m.from_handle == "andy/fx1")
        .unwrap()
        .clone()
}

#[gpui::test]
fn take_over_opens_terminal_between_chat_and_composer(cx: &mut TestAppContext) {
    let (chat, backend, cx) = open(cx, Screen::Chat);
    let done = fx1_done(&backend.snapshot());
    let last = backend.snapshot().messages.last().unwrap().clone();
    assert!(cx.debug_bounds("terminal").is_none());

    click(cx, sel(&done, "open-in-terminal"));
    settle(cx);
    let fx1 = fixture_id("andy/fx1");
    assert!(backend.snapshot().take_overs.contains_key(&fx1));
    let terminal = cx.debug_bounds("terminal").expect("the terminal opens");
    let row = cx.debug_bounds(sel(&last, "row")).unwrap();
    let composer = cx.debug_bounds("composer:to").unwrap();
    assert!(row.bottom() <= terminal.top(), "{row:?} over {terminal:?}");
    assert!(
        terminal.bottom() <= composer.top(),
        "{terminal:?} over the composer"
    );
    assert!(cx.debug_bounds("terminal:screen").is_some());
    // Taken over: the chip goes, and keys go to the terminal, not the hub.
    assert!(cx.debug_bounds(sel(&done, "open-in-terminal")).is_none());
    let view = chat
        .read_with(cx, |chat, _| chat.terminal().cloned())
        .unwrap();
    assert!(cx.update(|window, cx| view.focus_handle(cx).is_focused(window)));
    let before = backend.snapshot().messages.len();
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    settle(cx);
    assert_eq!(backend.snapshot().messages.len(), before);
}

#[gpui::test]
fn hand_back_removes_it(cx: &mut TestAppContext) {
    let (chat, backend, cx) = open(cx, Screen::ChatTerminal);
    assert!(cx.debug_bounds("terminal").is_some());
    assert_eq!(
        chat.read_with(cx, |chat, cx| chat.take_over_status(cx)),
        Some("andy/fx1 · taken over".into())
    );
    click(cx, "terminal:hand-back");
    settle(cx);
    assert!(cx.debug_bounds("terminal").is_none());
    assert!(backend.snapshot().take_overs.is_empty());
    assert_eq!(
        chat.read_with(cx, |chat, cx| chat.take_over_status(cx)),
        None
    );
    let done = fx1_done(&backend.snapshot());
    assert!(cx.debug_bounds(sel(&done, "open-in-terminal")).is_some());
}

#[gpui::test]
fn toggle_hides_and_shows_the_terminal(cx: &mut TestAppContext) {
    // ctrl-` (V86): the session stays taken over while hidden.
    let (chat, backend, cx) = open(cx, Screen::ChatTerminal);
    chat.update_in(cx, |chat, window, cx| chat.toggle_terminal(window, cx));
    settle(cx);
    assert!(cx.debug_bounds("terminal").is_none());
    assert_eq!(backend.snapshot().take_overs.len(), 1);
    // The composer has the keyboard while the terminal is hidden.
    assert!(cx.update(|window, cx| chat.focus_handle(cx).is_focused(window)));
    chat.update_in(cx, |chat, window, cx| chat.toggle_terminal(window, cx));
    settle(cx);
    assert!(cx.debug_bounds("terminal").is_some());
    let view = chat
        .read_with(cx, |chat, _| chat.terminal().cloned())
        .unwrap();
    assert!(cx.update(|window, cx| view.focus_handle(cx).is_focused(window)));
}

#[gpui::test]
fn open_in_terminal_takes_over_the_newest_offered_done(cx: &mut TestAppContext) {
    // ⌥O: the agent of the DONE row whose chip shows, here andy/fx1.
    let (chat, backend, cx) = open(cx, Screen::Chat);
    chat.update(cx, |chat, cx| chat.open_in_terminal(cx));
    settle(cx);
    assert!(
        backend
            .snapshot()
            .take_overs
            .contains_key(&fixture_id("andy/fx1"))
    );
    assert!(cx.debug_bounds("terminal").is_some());
}

#[gpui::test]
fn agent_chip_opens_the_menu_without_covering_it(cx: &mut TestAppContext) {
    let (_chat, _backend, cx) = open(cx, Screen::ChatTerminal);
    click(cx, "terminal:agent");
    settle(cx);
    let chip = cx.debug_bounds("terminal:agent").unwrap();
    let menu = cx.debug_bounds("switch-menu").expect("the menu opens");
    assert!(!menu.intersects(&chip), "fix 27: {menu:?} covers {chip:?}");
    assert!(menu.left() <= chip.left() + px(1.));
    assert!(cx.debug_bounds("switch-menu:claude:check").is_some());
    cx.simulate_keystrokes("escape");
    settle(cx);
    assert!(cx.debug_bounds("switch-menu").is_none());
}

#[gpui::test]
fn switching_from_the_menu_starts_a_handoff_session(cx: &mut TestAppContext) {
    let (chat, backend, cx) = open(cx, Screen::ChatTerminal);
    click(cx, "terminal:agent");
    settle(cx);
    click(cx, "switch-menu:codex");
    settle(cx);
    let fx1 = fixture_id("andy/fx1");
    assert_eq!(backend.snapshot().take_overs[&fx1].harness, "codex");
    assert!(cx.debug_bounds("switch-menu").is_none());
    let view = chat
        .read_with(cx, |chat, _| chat.terminal().cloned())
        .unwrap();
    assert_eq!(view.read_with(cx, |v, _| v.harness().to_owned()), "codex");
    // grok is missing: its row switches to nothing.
    click(cx, "terminal:agent");
    settle(cx);
    click(cx, "switch-menu:grok");
    settle(cx);
    assert_eq!(backend.snapshot().take_overs[&fx1].harness, "codex");
}

#[gpui::test]
fn add_opens_the_dialog_and_escape_returns_to_the_menu(cx: &mut TestAppContext) {
    let (_chat, _backend, cx) = open(cx, Screen::ChatTerminal);
    click(cx, "terminal:agent");
    settle(cx);
    click(cx, "switch-menu:grok:add");
    settle(cx);
    assert!(cx.debug_bounds("add-agent").is_some());
    // The dialog's card is node 2418's width.
    let card = cx.debug_bounds("add-agent-dialog:card").unwrap();
    assert_eq!(card.size.width, crate::ui::tokens::DIALOG_WIDTH);
    assert!(
        cx.debug_bounds("switch-menu").is_some(),
        "the menu stays under it"
    );
    cx.simulate_keystrokes("escape");
    settle(cx);
    assert!(cx.debug_bounds("add-agent").is_none());
    assert!(cx.debug_bounds("switch-menu").is_some());
}

#[gpui::test]
fn check_again_says_the_agent_is_still_missing(cx: &mut TestAppContext) {
    let (_chat, _backend, cx) = open(cx, Screen::ChatTerminal);
    click(cx, "terminal:agent");
    settle(cx);
    click(cx, "switch-menu:grok:add");
    settle(cx);
    assert!(cx.debug_bounds("add-agent:note").is_none());
    click(cx, "add-agent:check");
    settle(cx);
    assert!(cx.debug_bounds("add-agent:note").is_some());
    assert!(cx.debug_bounds("add-agent").is_some());
}

#[gpui::test]
fn open_in_terminal_shows_a_hidden_terminal_again(cx: &mut TestAppContext) {
    // Review: ctrl-` hid it, then fx1's Open in terminal must bring it back.
    let (chat, _backend, cx) = open(cx, Screen::ChatTerminal);
    chat.update_in(cx, |chat, window, cx| chat.toggle_terminal(window, cx));
    settle(cx);
    assert!(cx.debug_bounds("terminal").is_none());
    chat.update(cx, |chat, cx| {
        chat.take_over_agent(fixture_id("andy/fx1"), cx);
    });
    settle(cx);
    assert!(cx.debug_bounds("terminal").is_some());
    let view = chat
        .read_with(cx, |chat, _| chat.terminal().cloned())
        .unwrap();
    assert!(cx.update(|window, cx| view.focus_handle(cx).is_focused(window)));
}

/// The chat filling a window of any size.
struct FullHost(Entity<ChatView>);

impl Render for FullHost {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div().size_full().flex().flex_col().child(self.0.clone())
    }
}

#[gpui::test]
fn the_menu_never_covers_the_chip_in_a_short_window(cx: &mut TestAppContext) {
    // Fix 27 in a window with room for the menu on neither side: the menu
    // is cut to its side's room instead of snapped over the chip.
    init(cx);
    // Every binding, so Escape reaches the menu and the dialog.
    cx.update(crate::keymap::bind_all);
    let backend = Rc::new(FixtureBackend::for_screen(Screen::ChatTerminal).unwrap());
    let handle: Rc<dyn Backend> = backend.clone();
    cx.update(|cx| cx.set_global(BackendHandle(handle)));
    let snapshot = backend.snapshot();
    let (_host, cx) = cx.add_window_view(|window, cx| {
        FullHost(cx.new(|cx| ChatView::new(std::rc::Rc::new(snapshot), window, cx)))
    });
    let viewport = gpui::size(px(1000.), px(300.));
    cx.simulate_resize(viewport);
    settle(cx);
    click(cx, "terminal:agent");
    settle(cx);
    redraw(cx);
    let chip = cx.debug_bounds("terminal:agent").unwrap();
    let menu = cx.debug_bounds("switch-menu").expect("the menu opens");
    assert!(!menu.intersects(&chip), "fix 27: {menu:?} covers {chip:?}");
    assert!(
        menu.bottom() <= viewport.height,
        "{menu:?} leaves the window"
    );
}

#[gpui::test]
fn check_again_says_why_the_scan_failed(cx: &mut TestAppContext) {
    // Review: a failed ScanAgents ends the check with its error.
    let (_chat, _backend, cx) = open(cx, Screen::ChatTerminal);
    click(cx, "terminal:agent");
    settle(cx);
    click(cx, "switch-menu:grok:add");
    settle(cx);
    cx.update(|_, cx| cx.remove_global::<BackendHandle>());
    click(cx, "add-agent:check");
    settle(cx);
    assert!(cx.debug_bounds("add-agent:note").is_some());
}
