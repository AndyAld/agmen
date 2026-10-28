use std::rc::Rc;

use agmen_proto::{MAX_BODY_BYTES, StampedEnvelope};
use gpui::{TestAppContext, VisualTestContext, div, px};

use super::*;
use crate::backend::{Backend, ChangeState, FixtureBackend, Screen, chat_snapshot};
use crate::ui::composer::Target;
use crate::ui::message::is_ledger;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::text_input::bind_keys;
use crate::ui::tokens::{MOCKUP_REM, SPACE_12};

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
        let chat = cx.new(|cx| ChatView::new(std::rc::Rc::new(snapshot), window, cx));
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
    assert_eq!(
        sent.client.to,
        vec![snapshot.listener_id().cloned().unwrap()]
    );
    assert_eq!(sent.client.body.as_str(), "hi");
    assert_eq!(sent.from_handle, "andy");
}

#[gpui::test]
fn sent_message_appears_in_the_list(cx: &mut TestAppContext) {
    let (chat, backend, cx) = open_screen(cx, Screen::Chat);
    let before = chat.read_with(cx, |chat, cx| chat.stream.read(cx).rows.len());
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    redraw(cx);
    let sent = backend.snapshot().messages.last().unwrap().clone();
    assert_eq!(
        chat.read_with(cx, |chat, cx| chat.stream.read(cx).rows.len()),
        before + 1
    );
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
    let to = vec![backend.snapshot().listener_id().cloned().unwrap()];
    let draft = |body: String| Draft {
        to: to.clone(),
        body,
        reply_to: None,
        model: None,
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

// Task 6: the live backend refuses some posts only after send returned,
// when the composer has already cleared.
/// A NotSent for a reply to the first message not from andy, and that
/// message's sender.
fn refused_reply(snapshot: &Snapshot, body: &str) -> (Update, Target) {
    let parent = snapshot
        .messages
        .iter()
        .find(|m| m.from_handle != snapshot.me)
        .unwrap();
    let update = Update::NotSent {
        draft: Draft {
            to: vec![parent.from.clone()],
            body: body.into(),
            reply_to: Some(parent.client.id.clone()),
            model: None,
        },
        why: "the hub refused it: forbidden_mention".into(),
    };
    let sender = Target {
        id: parent.from.clone(),
        handle: parent.from_handle.clone(),
    };
    (update, sender)
}

#[gpui::test]
fn a_post_refused_later_comes_back_into_the_composer(cx: &mut TestAppContext) {
    let (chat, backend, cx) = open_screen(cx, Screen::Chat);
    let (update, sender) = refused_reply(&backend.snapshot(), "yes, rebase");
    chat.update_in(cx, |chat, window, cx| chat.apply_in(update, window, cx));
    redraw(cx);
    assert!(cx.debug_bounds("chat:error").is_some());
    chat.read_with(cx, |chat, cx| {
        assert_eq!(
            chat.error(),
            Some("Not sent: the hub refused it: forbidden_mention")
        );
        let composer = chat.composer.read(cx);
        assert_eq!(composer.text(cx), "yes, rebase");
        // The reply comes back too, so Enter answers the same message.
        assert_eq!(composer.target(cx), Some(&sender));
    });
}

#[gpui::test]
fn a_post_refused_later_keeps_newer_text(cx: &mut TestAppContext) {
    let (chat, backend, cx) = open_screen(cx, Screen::Chat);
    let listener = chat.read_with(cx, |chat, cx| chat.composer.read(cx).target(cx).cloned());
    cx.simulate_input("something newer");
    let (update, _) = refused_reply(&backend.snapshot(), "yes, rebase");
    chat.update_in(cx, |chat, window, cx| chat.apply_in(update, window, cx));
    redraw(cx);
    assert!(cx.debug_bounds("chat:error").is_some());
    chat.read_with(cx, |chat, cx| {
        let composer = chat.composer.read(cx);
        // What the person typed since stays, and so does its target; the
        // error still says the earlier message was not sent.
        assert_eq!(composer.text(cx), "something newer");
        assert_eq!(composer.target(cx), listener.as_ref());
    });
}

#[gpui::test]
fn send_without_a_backend_shows_why(cx: &mut TestAppContext) {
    let snapshot = chat_snapshot();
    let to = vec![snapshot.listener_id().cloned().unwrap()];
    let (chat, cx) = open_with(cx, snapshot);
    let refused = chat.update(cx, |chat, cx| {
        chat.send(
            Draft {
                to,
                body: "hi".into(),
                reply_to: None,
                model: None,
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
    assert_eq!(
        chat.read_with(cx, |chat, cx| chat.stream.read(cx).rows.len()),
        0
    );
    assert!(cx.debug_bounds("composer:model").is_some());
}

#[gpui::test]
fn badge_counts_alerts_that_need_me(cx: &mut TestAppContext) {
    for (screen, needs_you) in [
        (Screen::Chat, 0),
        (Screen::NeedsYou, 1),
        (Screen::NeedsYouAnswered, 0),
    ] {
        let mut app = TestAppContext::single();
        let (chat, _backend, app) = open_screen(&mut app, screen);
        assert_eq!(
            chat.read_with(app, |chat, _| chat.badge()),
            needs_you,
            "{screen:?}"
        );
    }
    // The first context is unused past here; it only has to exist.
    init(cx);
}

#[gpui::test]
fn rows_sit_inside_the_streams_side_padding(cx: &mut TestAppContext) {
    // Node 350, the mention row, is inset 12 px from the center column on
    // both sides (stream node 137 pads [10, 12, 6, 12]).
    let snapshot = chat_snapshot();
    let mention = snapshot.messages[8].clone();
    let (_chat, cx) = open_with(cx, snapshot);
    let chat = cx.debug_bounds("chat").unwrap();
    let row = cx.debug_bounds(sel(&mention, "row")).unwrap();
    assert_eq!(row.left() - chat.left(), SPACE_12);
    assert_eq!(chat.right() - row.right(), SPACE_12);
}

#[gpui::test]
fn show_ledger_keeps_the_reading_position(cx: &mut TestAppContext) {
    // 120 more messages, every other one a HEARTBEAT, so the chat scrolls
    // and the toggle changes which rows exist.
    let mut snapshot = chat_snapshot();
    let review = snapshot.messages[7].clone();
    for i in 0..60 {
        let mut env = review.clone();
        env.client.id = format!("01JE{i:022}").parse().unwrap();
        snapshot.messages.push(env);
        let beat = heartbeat(&snapshot.messages);
        snapshot.messages.push(beat);
    }
    let (chat, cx) = open_with(cx, snapshot);
    chat.read_with(cx, |chat, cx| {
        chat.stream.read(cx).list.scroll_to(ListOffset {
            item_ix: 20,
            offset_in_item: px(0.),
        });
    });
    redraw(cx);
    let top_message = |chat: &ChatView, cx: &gpui::App| {
        let stream = chat.stream.read(cx);
        stream.rows[stream.list.logical_scroll_top().item_ix]
    };
    let before = chat.read_with(cx, top_message);

    chat.update(cx, |chat, cx| chat.set_show_ledger(true, cx));
    redraw(cx);
    assert_eq!(chat.read_with(cx, top_message), before);

    chat.update(cx, |chat, cx| chat.set_show_ledger(false, cx));
    redraw(cx);
    assert_eq!(chat.read_with(cx, top_message), before);
}

/// Draws the window without [`redraw`]'s refresh, so cached views are
/// reused as in a real frame.
fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| drop(window.draw(cx)));
}

fn rows_rendered() -> usize {
    crate::ui::message::ROWS_RENDERED.with(std::cell::Cell::get)
}

#[gpui::test]
fn typing_in_the_composer_does_not_rebuild_the_rows(cx: &mut TestAppContext) {
    let snapshot = chat_snapshot();
    let mention = snapshot.messages[8].clone();
    let (chat, cx) = open_with(cx, snapshot);
    // The first key switches the window to keyboard input, which GPUI
    // answers with one full refresh; the keys after it are what we measure.
    cx.simulate_input("h");
    draw(cx);
    let before = rows_rendered();
    assert!(before > 0, "the first frames drew rows");
    cx.simulate_input("ello");
    draw(cx);
    assert_eq!(rows_rendered(), before, "a keystroke rebuilt the rows");
    assert!(cx.debug_bounds(sel(&mention, "row")).is_some());

    // A change to what the list shows does rebuild them, with no refresh.
    chat.update(cx, |chat, cx| chat.set_show_ledger(true, cx));
    draw(cx);
    assert!(
        rows_rendered() > before,
        "the ledger toggle kept stale rows"
    );
}

#[gpui::test]
fn snoozed_alert_stays_snoozed_across_snapshots(cx: &mut TestAppContext) {
    // C229: a snapshot updates the alert state from its new messages only,
    // so it does not bring back an alert the person snoozed.
    let (chat, backend, cx) = open_screen(cx, Screen::NeedsYou);
    let alert = chat.read_with(cx, |chat, _| chat.alerts.needing_me()[0].clone());
    chat.update(cx, |chat, _| assert!(chat.alerts.snooze(&alert)));
    let snapshot = backend.snapshot();
    chat.update(cx, |chat, cx| {
        chat.apply(Update::Snapshot(Box::new(snapshot.clone())), cx);
    });
    assert_eq!(chat.read_with(cx, |chat, _| chat.badge()), 0);

    // A new urgent alert in a later snapshot still needs me.
    let mut later = snapshot;
    let mut fresh = later
        .messages
        .iter()
        .find(|m| m.client.id == alert)
        .unwrap()
        .clone();
    fresh.client.id = "01JE0000000000000000000NEW".parse().unwrap();
    fresh.seq = agmen_proto::Seq(fresh.seq.0 + 1000);
    later.messages.push(fresh.clone());
    chat.update(cx, |chat, cx| {
        chat.apply(Update::Snapshot(Box::new(later)), cx);
    });
    assert_eq!(
        chat.read_with(cx, |chat, _| chat.alerts.needing_me().to_vec()),
        [fresh.client.id]
    );
}

#[gpui::test]
fn a_snapshot_reaches_the_composers_model_chip(cx: &mut TestAppContext) {
    // Task 15: a new listener agent shows on the chip.
    let (chat, cx) = open_with(cx, chat_snapshot());
    let mut later = chat_snapshot();
    later.listener.as_mut().unwrap().agent = "claude".into();
    chat.update(cx, |chat, cx| {
        chat.apply(Update::Snapshot(Box::new(later)), cx);
    });
    let chip = chat.read_with(cx, |chat, cx| {
        chat.composer
            .read(cx)
            .model_choice()
            .map(crate::backend::ModelChoice::label)
    });
    assert_eq!(chip.as_deref(), Some("claude · default"));
}

// Final review fix 1: older messages page in on scroll-back.

/// Scrolls the chat's message list up until its first row shows. The list
/// measures rows as they come into view, so one wheel event reaches only
/// as far as the rows measured so far.
fn scroll_to_top(chat: &Entity<ChatView>, cx: &mut VisualTestContext) {
    let bounds = cx.debug_bounds("chat").unwrap();
    for _ in 0..200 {
        cx.simulate_event(gpui::ScrollEvent {
            position: bounds.origin + gpui::point(px(100.), px(100.)),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(100_000.))),
            ..Default::default()
        });
        redraw(cx);
        if chat.read_with(cx, |chat, _| chat.at_top) {
            return;
        }
    }
    panic!("the chat never reached its first row");
}

/// Runs the window until `done`, failing after 10 s: the live backend's
/// threads answer from outside the test's executor.
fn wait_until(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        redraw(cx);
        if done(cx) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[gpui::test]
fn scrolling_to_the_top_loads_older_messages(cx: &mut TestAppContext) {
    use crate::backend::fake_agmend::{FakeAgmend, PROJECT};
    use crate::backend::{AgmendBackend, Older};

    let fake = FakeAgmend::with_history(1200);
    cx.executor().allow_parking();
    let backend: Rc<dyn Backend> =
        Rc::new(AgmendBackend::connect(&fake.path(), PROJECT, None).unwrap());
    cx.update(|cx| cx.set_global(BackendHandle(backend.clone())));
    let (chat, cx) = open_with(cx, backend.snapshot());
    let count = |cx: &mut VisualTestContext| chat.read_with(cx, |chat, _| chat.message_count());
    assert_eq!(count(cx), 500);
    // The newest message shows, not the top of the page.
    assert!(cx.debug_bounds("chat:history").is_none());

    scroll_to_top(&chat, cx);
    wait_until(cx, |cx| count(cx) == 1000);
    // The message the person scrolled to stays at the top of the view.
    let top = chat.read_with(cx, |chat, cx| {
        let stream = chat.stream.read(cx);
        let row = stream.rows[stream.list.logical_scroll_top().item_ix];
        chat.snapshot.messages[row].seq.0
    });
    assert_eq!(top, 701);

    scroll_to_top(&chat, cx);
    wait_until(cx, |cx| count(cx) == 1200);
    scroll_to_top(&chat, cx);
    chat.read_with(cx, |chat, _| {
        assert_eq!(chat.snapshot.older, Older::Start);
        assert_eq!(
            chat.history_note().as_deref(),
            Some("This is the start of the chat.")
        );
    });
    assert!(cx.debug_bounds("chat:history").is_some());
    assert_eq!(fake.state.lock().unwrap().history_params.len(), 2);
}

#[test]
fn history_notes_word_each_state() {
    use crate::backend::Older;

    let note = |older: &Older, at_top| history_note(older, at_top).map(|n| n.to_string());
    assert_eq!(
        note(&Older::Loading, false).as_deref(),
        Some("Loading older messages…")
    );
    assert_eq!(
        note(&Older::Failed("agmend is busy".into()), true).as_deref(),
        Some("Older messages did not load: agmend is busy. Scroll up to try again.")
    );
    assert_eq!(
        note(&Older::Start, true).as_deref(),
        Some("This is the start of the chat.")
    );
    // Only at the top; and More turns into Loading at once.
    assert_eq!(note(&Older::Start, false), None);
    assert_eq!(note(&Older::More, true), None);
}

#[gpui::test]
fn reading_back_past_the_cap_offers_the_newest(cx: &mut TestAppContext) {
    let mut snapshot = chat_snapshot();
    snapshot.newer = true;
    let (chat, cx) = open_with(cx, snapshot);
    assert!(cx.debug_bounds("chat:newest").is_some());
    chat.update(cx, |chat, cx| {
        let mut s = (*chat.snapshot).clone();
        s.newer = false;
        chat.apply(Update::Snapshot(Box::new(s)), cx);
    });
    redraw(cx);
    assert!(cx.debug_bounds("chat:newest").is_none());
}
