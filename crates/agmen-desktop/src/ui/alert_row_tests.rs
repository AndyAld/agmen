use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use agmen_proto::StampedEnvelope;
use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, div, px,
};

use crate::backend::{Action, Backend as _, FixtureBackend, Screen};
use crate::ui::chat_context::ChatContext;
use crate::ui::message::MessageRow;
use crate::ui::needs_you::AlertLook;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{ALERT_BAR_WIDTH, MOCKUP_REM, SNOOZED_OPACITY};

use super::row_opacity;

type Heard<T> = Rc<RefCell<Vec<T>>>;

/// The needs-you screen's rows from the alert on, in the frame's 804 px
/// stream, with `look` on the alert.
struct Host {
    context: Rc<ChatContext>,
    looks: BTreeMap<usize, AlertLook>,
    actions: Heard<Action>,
    replies: Heard<usize>,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        let count = self.context.snapshot().messages.len();
        let rows: Vec<AnyElement> = (count - 2..count)
            .map(|index| {
                let (actions, replies) = (self.actions.clone(), self.replies.clone());
                let row = MessageRow::new(index, self.context.clone())
                    .on_action(move |action, _, _| actions.borrow_mut().push(action))
                    .on_reply(move |index, _, _| replies.borrow_mut().push(index));
                match self.looks.get(&index) {
                    Some(look) => row.alert(*look),
                    None => row,
                }
                .into_any_element()
            })
            .collect();
        div().w(px(804.)).flex().flex_col().children(rows)
    }
}

struct Opened<'a> {
    cx: &'a mut VisualTestContext,
    alert: StampedEnvelope,
    actions: Heard<Action>,
    replies: Heard<usize>,
}

fn open(cx: &mut TestAppContext, screen: Screen, look: Option<AlertLook>) -> Opened<'_> {
    init(cx);
    let snapshot = FixtureBackend::for_screen(screen).unwrap().snapshot();
    let index = snapshot
        .messages
        .iter()
        .rposition(|m| m.from_handle == "adam/coord")
        .unwrap();
    let alert = snapshot.messages[index].clone();
    let context = Rc::new(ChatContext::new(Rc::new(snapshot)));
    let looks = look.map(|look| (index, look)).into_iter().collect();
    let (actions, replies): (Heard<Action>, Heard<usize>) = (Rc::default(), Rc::default());
    let (a, r) = (actions.clone(), replies.clone());
    let (_, cx) = cx.add_window_view(move |_, _| Host {
        context,
        looks,
        actions: a,
        replies: r,
    });
    redraw(cx);
    Opened {
        cx,
        alert,
        actions,
        replies,
    }
}

fn sel(env: &StampedEnvelope, part: &str) -> &'static str {
    Box::leak(format!("{}:{part}", env.client.id.as_str()).into_boxed_str())
}

#[gpui::test]
fn alert_that_needs_me_is_red_with_needs_you_and_four_buttons(cx: &mut TestAppContext) {
    let o = open(cx, Screen::NeedsYou, Some(AlertLook::NeedsMe));
    let (cx, alert) = (o.cx, &o.alert);
    let bar = cx.debug_bounds(sel(alert, "alert-bar")).unwrap();
    assert_eq!(bar.size.width, ALERT_BAR_WIDTH);
    assert!(cx.debug_bounds(sel(alert, "mention-bar")).is_none());
    // NEEDS YOU sits between the verb and the time (node 5960).
    let verb = cx.debug_bounds(sel(alert, "verb")).unwrap();
    let needs = cx.debug_bounds(sel(alert, "needs-you")).unwrap();
    let time = cx.debug_bounds(sel(alert, "time")).unwrap();
    assert!(verb.right() <= needs.left() && needs.right() <= time.left());
    // Reply, Hotfix, Revert #10, then one Snooze 15 min (node 5995).
    let names = [
        "alert:reply",
        "alert:action:0",
        "alert:action:1",
        "alert:snooze",
    ];
    let buttons: Vec<_> = names
        .iter()
        .map(|name| cx.debug_bounds(sel(alert, name)).unwrap())
        .collect();
    assert!(
        cx.debug_bounds(sel(alert, "alert:action:2")).is_none(),
        "Snooze shows once"
    );
    for pair in buttons.windows(2) {
        assert!(pair[0].right() < pair[1].left());
        assert_eq!(pair[0].size.height, pair[1].size.height);
        assert_eq!(pair[0].top(), pair[1].top());
    }
    let body = cx.debug_bounds(sel(alert, "body")).unwrap();
    assert!(
        buttons[0].top() > body.bottom() - px(1.),
        "the buttons sit under the text"
    );
}

#[gpui::test]
fn alert_buttons_report_what_they_do(cx: &mut TestAppContext) {
    let o = open(cx, Screen::NeedsYou, Some(AlertLook::NeedsMe));
    let id = o.alert.client.id.clone();
    click(o.cx, sel(&o.alert, "alert:action:1"));
    click(o.cx, sel(&o.alert, "alert:snooze"));
    click(o.cx, sel(&o.alert, "alert:reply"));
    assert_eq!(
        *o.actions.borrow(),
        [
            Action::AnswerAlert {
                alert: id.clone(),
                choice: "Revert #10".into()
            },
            Action::SnoozeAlert(id),
        ]
    );
    assert_eq!(o.replies.borrow().len(), 1);
}

#[gpui::test]
fn snoozed_alert_keeps_its_look_without_the_badge_or_buttons(cx: &mut TestAppContext) {
    let o = open(cx, Screen::NeedsYou, Some(AlertLook::Snoozed));
    let (cx, alert) = (o.cx, &o.alert);
    assert!(cx.debug_bounds(sel(alert, "alert-bar")).is_some());
    assert!(cx.debug_bounds(sel(alert, "needs-you")).is_none());
    assert!(cx.debug_bounds(sel(alert, "alert-actions")).is_none());
    assert_eq!(row_opacity(Some(AlertLook::Snoozed)), SNOOZED_OPACITY);
    assert_eq!(row_opacity(Some(AlertLook::NeedsMe)), 1.);
    assert_eq!(row_opacity(None), 1.);
}

#[gpui::test]
fn answered_alert_draws_as_a_mention(cx: &mut TestAppContext) {
    // Frame 6122, node 6499: the alert keeps its ALERT verb and turns into
    // the yellow mention row.
    let o = open(cx, Screen::NeedsYouAnswered, None);
    let (cx, alert) = (o.cx, &o.alert);
    assert!(cx.debug_bounds(sel(alert, "mention-bar")).is_some());
    assert!(cx.debug_bounds(sel(alert, "alert-bar")).is_none());
    assert!(cx.debug_bounds(sel(alert, "needs-you")).is_none());
    assert!(cx.debug_bounds(sel(alert, "alert-actions")).is_none());
}
