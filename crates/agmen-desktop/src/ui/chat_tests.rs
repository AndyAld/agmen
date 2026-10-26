use std::rc::Rc;

use agmen_proto::{MAX_BODY_BYTES, StampedEnvelope};
use gpui::{TestAppContext, VisualTestContext, div, px};

use super::*;
use crate::backend::{Backend, ChangeState, FixtureBackend, Screen, chat_snapshot};
use crate::ui::message::is_ledger;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::text_input::bind_keys;
use crate::ui::tokens::MOCKUP_REM;

/// The window's root in these tests: the chat in an 800 by 900 box.
struct Host(Entity<ChatView>);

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div()
            .w(px(800.))
            .h(px(900.))
            .flex()
            .flex_col()
            .child(self.0.clone())
    }
}

/// A chat over `snapshot`, with the composer focused.
fn open_with(
    cx: &mut TestAppContext,
    snapshot: Snapshot,
) -> (Entity<ChatView>, &mut VisualTestContext) {
    init(cx);
    cx.update(bind_keys);
    let (host, cx) = cx.add_window_view(|window, cx| {
        let chat = cx.new(|cx| ChatView::new(snapshot, window, cx));
        Host(chat)
    });
    let chat = host.read_with(cx, |host, _| host.0.clone());
    chat.update_in(cx, |chat, window, cx| {
        chat.focus_handle(cx).focus(window, cx)
    });
    redraw(cx);
    (chat, cx)
}

/// A chat over `screen`'s fixture, sending and acting through it.
fn open_screen(
    cx: &mut TestAppContext,
    screen: Screen,
) -> (Entity<ChatView>, Rc<FixtureBackend>, &mut VisualTestContext) {
    let backend = Rc::new(FixtureBackend::for_screen(screen).unwrap());
    let handle: Rc<dyn Backend> = backend.clone();
    cx.update(|cx| cx.set_global(BackendHandle(handle)));
    let (chat, cx) = open_with(cx, backend.snapshot());
    (chat, backend, cx)
}

fn sel(env: &StampedEnvelope, part: &str) -> &'static str {
    Box::leak(format!("{}:{part}", env.client.id.as_str()).into_boxed_str())
}

/// A HEARTBEAT from andy/fx1, appended after the chat fixture's messages.
fn heartbeat(after: &[StampedEnvelope]) -> StampedEnvelope {
    let last = after.last().unwrap();
    let mut value = serde_json::to_value(last).unwrap();
    let seq = after.len() + 1;
    value["id"] = format!("01JC{seq:022}").into();
    value["seq"] = seq.into();
    value["verb"] = "HEARTBEAT".into();
    value["data"] = serde_json::json!({ "items": ["C-015"] });
    value["body"] = "".into();
    value["reply_to"] = serde_json::Value::Null;
    value["to"] = serde_json::json!([]);
    value["mentioned"] = serde_json::json!([]);
    value["refs"] = serde_json::json!([]);
    value["from"] = "andyfx10".into();
    value["from_handle"] = "andy/fx1".into();
    value["from_kind"] = "agent".into();
    serde_json::from_value(value).unwrap()
}

#[test]
fn presence_lines_hide_until_show_ledger() {
    let mut messages = chat_snapshot().messages;
    messages.push(heartbeat(&messages));
    let all: Vec<usize> = (0..messages.len()).collect();
    let mut without_heartbeat = all.clone();
    without_heartbeat.pop();
    assert_eq!(visible_rows(&messages, false), without_heartbeat);
    assert_eq!(visible_rows(&messages, true), all);
    assert!(is_ledger(&messages[5]), "the GRANT is a ledger line");
    assert!(!is_presence(&messages[5]), "and it shows by default");
}

#[gpui::test]
fn chat_hides_ledger_by_default(cx: &mut TestAppContext) {
    let mut snapshot = chat_snapshot();
    let beat = heartbeat(&snapshot.messages);
    snapshot.messages.push(beat.clone());
    let grant = snapshot.messages[5].clone();
    let (chat, cx) = open_with(cx, snapshot);
    assert!(cx.debug_bounds(sel(&grant, "ledger")).is_some());
    assert!(cx.debug_bounds(sel(&beat, "ledger")).is_none());

    chat.update(cx, |chat, cx| chat.set_show_ledger(true, cx));
    redraw(cx);
    assert!(cx.debug_bounds(sel(&grant, "ledger")).is_some());
    assert!(cx.debug_bounds(sel(&beat, "ledger")).is_some());

    chat.update(cx, |chat, cx| chat.set_show_ledger(false, cx));
    redraw(cx);
    assert!(cx.debug_bounds(sel(&beat, "ledger")).is_none());
}

#[gpui::test]
fn composer_sends_to_listener_by_default(cx: &mut TestAppContext) {
    let (_chat, backend, cx) = open_screen(cx, Screen::Chat);
    let before = backend.snapshot().messages.len();
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    let snapshot = backend.snapshot();
    assert_eq!(snapshot.messages.len(), before + 1);
    let sent = snapshot.messages.last().unwrap();
    assert_eq!(sent.client.to, vec![snapshot.listener_id.clone()]);
    assert_eq!(sent.client.body.as_str(), "hi");
    assert_eq!(sent.from_handle, "andy");
}

#[gpui::test]
fn sent_message_appears_in_the_list(cx: &mut TestAppContext) {
    let (chat, backend, cx) = open_screen(cx, Screen::Chat);
    let before = chat.read_with(cx, |chat, _| chat.rows.len());
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    redraw(cx);
    let sent = backend.snapshot().messages.last().unwrap().clone();
    assert_eq!(chat.read_with(cx, |chat, _| chat.rows.len()), before + 1);
    assert!(
        cx.debug_bounds(sel(&sent, "row")).is_some(),
        "the sent message is drawn"
    );
}

#[gpui::test]
fn apply_on_the_hosted_card_changes_it(cx: &mut TestAppContext) {
    let (chat, backend, cx) = open_screen(cx, Screen::SettingsFromChat);
    let card = backend.snapshot().changes.into_values().next().unwrap();
    assert_eq!(card.state, ChangeState::Pending);
    let apply = Box::leak(format!("{}:apply", card.id).into_boxed_str());
    click(cx, apply);
    let state =
        |backend: &FixtureBackend| backend.snapshot().changes.values().next().unwrap().state;
    assert_eq!(state(&backend), ChangeState::Applied);

    cx.run_until_parked();
    redraw(cx);
    let shown = chat.read_with(cx, |chat, _| {
        chat.snapshot.changes.values().next().unwrap().state
    });
    assert_eq!(
        shown,
        ChangeState::Applied,
        "the chat hears the new snapshot"
    );
    let applied = Box::leak(format!("{}:applied", card.id).into_boxed_str());
    assert!(cx.debug_bounds(applied).is_some());
    assert!(cx.debug_bounds(apply).is_none());
}

#[gpui::test]
fn refused_send_shows_why_until_the_next_send(cx: &mut TestAppContext) {
    let (chat, backend, cx) = open_screen(cx, Screen::Chat);
    let to = vec![backend.snapshot().listener_id];
    let draft = |body: String| Draft {
        to: to.clone(),
        body,
        reply_to: None,
    };
    let big = draft("x".repeat(MAX_BODY_BYTES + 1));
    let refused = chat.update(cx, |chat, cx| chat.send(big, cx));
    assert!(matches!(refused, Err(BackendError::BodyTooLarge { .. })));
    redraw(cx);
    assert!(cx.debug_bounds("chat:error").is_some());

    let sent = chat.update(cx, |chat, cx| chat.send(draft("hi".into()), cx));
    assert!(sent.is_ok());
    redraw(cx);
    assert!(cx.debug_bounds("chat:error").is_none());
}

#[gpui::test]
fn send_without_a_backend_shows_why(cx: &mut TestAppContext) {
    let snapshot = chat_snapshot();
    let to = vec![snapshot.listener_id.clone()];
    let (chat, cx) = open_with(cx, snapshot);
    let refused = chat.update(cx, |chat, cx| {
        chat.send(
            Draft {
                to,
                body: "hi".into(),
                reply_to: None,
            },
            cx,
        )
    });
    assert!(refused.is_err());
    redraw(cx);
    assert!(cx.debug_bounds("chat:error").is_some());
}

#[gpui::test]
fn empty_chat_shows_the_composer(cx: &mut TestAppContext) {
    let mut snapshot = chat_snapshot();
    snapshot.messages.clear();
    let (chat, cx) = open_with(cx, snapshot);
    assert_eq!(chat.read_with(cx, |chat, _| chat.rows.len()), 0);
    assert!(cx.debug_bounds("composer:model").is_some());
}

#[gpui::test]
fn badge_counts_alerts_that_need_me(cx: &mut TestAppContext) {
    for screen in [Screen::Chat, Screen::NeedsYou, Screen::NeedsYouAnswered] {
        let mut app = TestAppContext::single();
        let (chat, backend, app) = open_screen(&mut app, screen);
        let needs_you = backend.snapshot().status.needs_you as usize;
        assert_eq!(
            chat.read_with(app, |chat, _| chat.badge()),
            needs_you,
            "{screen:?}"
        );
    }
    // The first context is unused past here; it only has to exist.
    init(cx);
}
