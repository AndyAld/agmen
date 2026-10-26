//! Task 13 in the chat: the alert row, the banner, Jump to it, Reply, the
//! sender's actions, Snooze, the toast, the chime and the notification.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use agmen_proto::{Payload, StampedEnvelope};
use gpui::{TestAppContext, VisualTestContext, div, px};

use super::*;
use crate::backend::{Backend, FixtureBackend, Screen};
use crate::ui::needs_you::{Alarm, AlarmHandle, Banner, Notice, SNOOZE_FOR, TOAST_FOR};
use crate::ui::test_support::{click, init, redraw};
use crate::ui::text_input::bind_keys;
use crate::ui::tokens::MOCKUP_REM;

/// The window's root in these tests: the chat in an 828 by 700 box, the
/// chat pane's width in the frame.
struct Host(Entity<ChatView>);

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div()
            .w(px(828.))
            .h(px(700.))
            .flex()
            .flex_col()
            .child(self.0.clone())
    }
}

/// What the chat asked to sound and post.
#[derive(Default)]
struct Recorder {
    chimes: Cell<usize>,
    notices: RefCell<Vec<Notice>>,
}

impl Alarm for Recorder {
    fn chime(&self) {
        self.chimes.set(self.chimes.get() + 1);
    }

    fn notify(&self, notice: &Notice, _cx: &gpui::App) {
        self.notices.borrow_mut().push(notice.clone());
    }
}

struct Opened<'a> {
    chat: Entity<ChatView>,
    backend: Rc<FixtureBackend>,
    alarm: Rc<Recorder>,
    cx: &'a mut VisualTestContext,
}

/// The chat over `backend`, with a recording alarm.
fn open_backend(cx: &mut TestAppContext, backend: FixtureBackend) -> Opened<'_> {
    init(cx);
    cx.update(bind_keys);
    let backend = Rc::new(backend);
    let alarm = Rc::new(Recorder::default());
    let (handle, sink): (Rc<dyn Backend>, Rc<dyn Alarm>) = (backend.clone(), alarm.clone());
    cx.update(|cx| {
        cx.set_global(BackendHandle(handle));
        cx.set_global(AlarmHandle(sink));
    });
    let snapshot = backend.snapshot();
    let (host, cx) = cx.add_window_view(|window, cx| {
        let chat = cx.new(|cx| ChatView::new(snapshot, window, cx));
        Host(chat)
    });
    let chat = host.read_with(cx, |host, _| host.0.clone());
    redraw(cx);
    Opened {
        chat,
        backend,
        alarm,
        cx,
    }
}

fn open(cx: &mut TestAppContext, screen: Screen) -> Opened<'_> {
    open_backend(cx, FixtureBackend::for_screen(screen).unwrap())
}

fn sel(env: &StampedEnvelope, part: &str) -> &'static str {
    Box::leak(format!("{}:{part}", env.client.id.as_str()).into_boxed_str())
}

/// The urgent alert from adam/coord.
fn urgent_alert(backend: &FixtureBackend) -> StampedEnvelope {
    backend
        .snapshot()
        .messages
        .into_iter()
        .find(|m| m.from_handle == "adam/coord")
        .unwrap()
}

fn banner_text(chat: &Entity<ChatView>, cx: &VisualTestContext) -> Option<Banner> {
    chat.read_with(cx, |chat, _| chat.banner())
}

/// Lets the backend's updates reach the chat, then draws.
fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    redraw(cx);
}

#[gpui::test]
fn counts_follow_the_alert_state(cx: &mut TestAppContext) {
    // Mockup fix 34: chat 1 alert; needs you 2 alerts and 1 needs you;
    // answered 1 alert.
    for (screen, counts) in [
        (Screen::Chat, (0, 1)),
        (Screen::NeedsYou, (1, 2)),
        (Screen::NeedsYouAnswered, (0, 1)),
    ] {
        let mut app = TestAppContext::single();
        let o = open(&mut app, screen);
        let shown = o.chat.read_with(o.cx, |chat, _| chat.alert_counts());
        assert_eq!((shown.needs_you, shown.alerts), counts, "{screen:?}");
    }
    init(cx);
}

#[gpui::test]
fn needs_you_screen_draws_the_alert_and_the_banner_from_history(cx: &mut TestAppContext) {
    let o = open(cx, Screen::NeedsYou);
    let alert = urgent_alert(&o.backend);
    let chat = o.cx.debug_bounds("chat").unwrap();
    let banner = o.cx.debug_bounds("banner").unwrap();
    assert_eq!(
        banner.top(),
        chat.top(),
        "the banner tops the chat (node 5704)"
    );
    assert!(o.cx.debug_bounds(sel(&alert, "needs-you")).is_some());
    assert!(o.cx.debug_bounds(sel(&alert, "alert:snooze")).is_some());
    // History neither chimes, notifies nor toasts.
    assert!(o.cx.debug_bounds("banner:chime").is_none());
    assert_eq!(o.alarm.chimes.get(), 0);
    assert!(o.alarm.notices.borrow().is_empty());
    assert!(o.chat.read_with(o.cx, |chat, _| chat.toast().is_none()));
}

#[gpui::test]
fn answered_screen_draws_the_answered_banner(cx: &mut TestAppContext) {
    let o = open(cx, Screen::NeedsYouAnswered);
    assert!(o.cx.debug_bounds("banner").is_some());
    assert!(o.cx.debug_bounds("banner:jump").is_none());
    assert!(matches!(
        banner_text(&o.chat, o.cx),
        Some(Banner::Answered { message }) if message.starts_with("adam (coordinator) answered")
    ));
}

#[gpui::test]
fn chat_screen_has_no_banner(cx: &mut TestAppContext) {
    let o = open(cx, Screen::Chat);
    assert!(o.cx.debug_bounds("banner").is_none());
}

#[gpui::test]
fn a_live_urgent_alert_chimes_notifies_and_toasts_once(cx: &mut TestAppContext) {
    let alert = urgent_alert(&FixtureBackend::for_screen(Screen::NeedsYou).unwrap());
    let o = open(cx, Screen::Chat);
    o.chat.update(o.cx, |chat, cx| {
        chat.apply(Update::Message(Box::new(alert.clone())), cx);
    });
    redraw(o.cx);
    assert_eq!(o.alarm.chimes.get(), 1);
    assert_eq!(
        o.alarm.notices.borrow()[0].title,
        "adam/coord needs you",
        "the notification says what the toast says"
    );
    let toast = o
        .chat
        .read_with(o.cx, |chat, _| chat.toast().cloned())
        .unwrap();
    assert_eq!(toast.title, "adam/coord needs you");
    assert_eq!(o.alarm.notices.borrow()[0].body, toast.body);
    assert!(o.cx.debug_bounds("banner:chime").is_some());
    assert!(o.cx.debug_bounds(sel(&alert, "needs-you")).is_some());
    // A redelivery is the same alert: no second chime.
    o.chat.update(o.cx, |chat, cx| {
        chat.apply(Update::Message(Box::new(alert.clone())), cx);
    });
    assert_eq!(o.alarm.chimes.get(), 1);
    assert_eq!(o.alarm.notices.borrow().len(), 1);
}

#[gpui::test]
fn the_toast_goes_after_its_time(cx: &mut TestAppContext) {
    let alert = urgent_alert(&FixtureBackend::for_screen(Screen::NeedsYou).unwrap());
    let o = open(cx, Screen::Chat);
    o.chat.update(o.cx, |chat, cx| {
        chat.apply(Update::Message(Box::new(alert)), cx);
    });
    assert!(o.chat.read_with(o.cx, |chat, _| chat.toast().is_some()));
    o.cx.executor().advance_clock(TOAST_FOR);
    o.cx.run_until_parked();
    assert!(o.chat.read_with(o.cx, |chat, _| chat.toast().is_none()));
}

#[gpui::test]
fn jump_to_it_scrolls_to_the_alert(cx: &mut TestAppContext) {
    // Sixty FYIs after the alert push it out of view.
    let backend = FixtureBackend::for_screen(Screen::NeedsYou).unwrap();
    let alert = urgent_alert(&backend);
    for i in 0..60 {
        backend
            .send(crate::backend::Draft {
                to: Vec::new(),
                body: format!("filler {i}"),
                reply_to: None,
            })
            .unwrap();
    }
    let o = open_backend(cx, backend);
    assert!(
        o.cx.debug_bounds(sel(&alert, "row")).is_none(),
        "scrolled away"
    );
    click(o.cx, "banner:jump");
    redraw(o.cx);
    let row = o.cx.debug_bounds(sel(&alert, "row")).unwrap();
    let chat = o.cx.debug_bounds("chat").unwrap();
    assert!(
        row.top() >= chat.top() && row.top() < chat.bottom(),
        "{row:?}"
    );
}

#[gpui::test]
fn revert_answers_the_alert_for_everyone(cx: &mut TestAppContext) {
    // Mockup fix 43: an action leads to the answered state.
    let o = open(cx, Screen::NeedsYou);
    let alert = urgent_alert(&o.backend);
    click(o.cx, sel(&alert, "alert:action:1"));
    settle(o.cx);
    let answer = o.backend.snapshot().messages.last().unwrap().clone();
    assert!(matches!(answer.client.payload, Payload::Answer {}));
    assert_eq!(answer.client.reply_to.as_ref(), Some(&alert.client.id));
    assert_eq!(answer.client.body.as_str(), "Revert #10");
    assert_eq!(o.chat.read_with(o.cx, |chat, _| chat.badge()), 0);
    assert!(matches!(
        banner_text(&o.chat, o.cx),
        Some(Banner::Answered { message }) if message.starts_with("You answered adam/coord's alert")
    ));
    assert!(o.cx.debug_bounds(sel(&alert, "needs-you")).is_none());
}

#[gpui::test]
fn reply_answers_the_alert_from_the_composer(cx: &mut TestAppContext) {
    let o = open(cx, Screen::NeedsYou);
    let alert = urgent_alert(&o.backend);
    click(o.cx, sel(&alert, "alert:reply"));
    o.cx.simulate_input("Revert it, I will look after");
    o.cx.simulate_keystrokes("enter");
    settle(o.cx);
    let answer = o.backend.snapshot().messages.last().unwrap().clone();
    assert!(matches!(answer.client.payload, Payload::Answer {}));
    assert_eq!(answer.client.reply_to.as_ref(), Some(&alert.client.id));
    assert_eq!(answer.client.to, vec![alert.from.clone()]);
    assert_eq!(o.chat.read_with(o.cx, |chat, _| chat.badge()), 0);
    assert!(matches!(
        banner_text(&o.chat, o.cx),
        Some(Banner::Answered { .. })
    ));
}

#[gpui::test]
fn snooze_dims_the_alert_then_it_needs_me_again(cx: &mut TestAppContext) {
    let o = open(cx, Screen::NeedsYou);
    let alert = urgent_alert(&o.backend);
    click(o.cx, sel(&alert, "alert:snooze"));
    settle(o.cx);
    assert_eq!(o.chat.read_with(o.cx, |chat, _| chat.badge()), 0);
    assert!(
        o.cx.debug_bounds("banner").is_none(),
        "no banner while snoozed"
    );
    assert!(
        o.cx.debug_bounds(sel(&alert, "alert-bar")).is_some(),
        "the row stays"
    );
    assert!(o.cx.debug_bounds(sel(&alert, "needs-you")).is_none());
    assert!(o.cx.debug_bounds(sel(&alert, "alert-actions")).is_none());

    o.cx.executor().advance_clock(SNOOZE_FOR);
    settle(o.cx);
    assert_eq!(o.chat.read_with(o.cx, |chat, _| chat.badge()), 1);
    assert!(o.cx.debug_bounds("banner").is_some());
    assert!(o.cx.debug_bounds(sel(&alert, "needs-you")).is_some());
    assert_eq!(o.alarm.chimes.get(), 0, "waking does not chime");
}

#[gpui::test]
fn show_arrival_draws_the_toast_and_chime_chip_silently(cx: &mut TestAppContext) {
    let o = open(cx, Screen::NeedsYou);
    o.chat.update(o.cx, |chat, cx| chat.show_arrival(cx));
    redraw(o.cx);
    assert!(o.chat.read_with(o.cx, |chat, _| chat.toast().is_some()));
    assert!(o.cx.debug_bounds("banner:chime").is_some());
    assert_eq!(o.alarm.chimes.get(), 0);
    assert!(o.alarm.notices.borrow().is_empty());
}
