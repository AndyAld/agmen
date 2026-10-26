use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Context, Render, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{Backend, ChangeId, ChangeRow, ChangeState, FixtureBackend, Screen};
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{CHANGE_CARD_WIDTH, CHANGE_VALUE_MIN_WIDTH, MOCKUP_REM};

const ID: &str = "chg-fx1-lanes-budget";

/// Draws the backend's first change card every frame and sends its actions
/// to the backend, as the chat will.
struct Host {
    backend: Rc<FixtureBackend>,
    sent: Rc<RefCell<Vec<Action>>>,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        let snapshot = self.backend.snapshot();
        let card = snapshot.changes.values().next().unwrap().clone();
        let roster: Vec<_> = snapshot.roster;
        let (backend, sent) = (self.backend.clone(), self.sent.clone());
        div().child(
            ChangeCard::new(card, &roster).on_action(move |action, _, _| {
                sent.borrow_mut().push(action.clone());
                backend.act(action).unwrap();
            }),
        )
    }
}

fn host(
    cx: &mut TestAppContext,
    screen: Screen,
) -> (
    Rc<FixtureBackend>,
    Rc<RefCell<Vec<Action>>>,
    &mut VisualTestContext,
) {
    init(cx);
    let backend = Rc::new(FixtureBackend::for_screen(screen).unwrap());
    let sent = Rc::new(RefCell::new(Vec::new()));
    let (b, s) = (backend.clone(), sent.clone());
    let (_, cx) = cx.add_window_view(move |_, _| Host {
        backend: b,
        sent: s,
    });
    redraw(cx);
    (backend, sent, cx)
}

/// Draws one card built by `make` every frame.
struct Lone(Box<dyn Fn() -> gpui::AnyElement>);

impl Render for Lone {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div().child((self.0)())
    }
}

fn state(backend: &FixtureBackend) -> ChangeState {
    backend.snapshot().changes.values().next().unwrap().state
}

fn shown(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

fn id() -> ChangeId {
    ChangeId(ID.into())
}

#[gpui::test]
fn widening_card_waits_for_apply(cx: &mut TestAppContext) {
    let (backend, _, cx) = host(cx, Screen::SettingsFromChat);
    assert_eq!(state(&backend), ChangeState::Pending);
    assert!(shown(cx, "chg-fx1-lanes-budget:waiting"));
    assert!(shown(cx, "chg-fx1-lanes-budget:apply"));
    assert!(shown(cx, "chg-fx1-lanes-budget:cancel"));
    assert!(!shown(cx, "chg-fx1-lanes-budget:undo"));
    let card = backend.snapshot().changes.values().next().unwrap().clone();
    assert_eq!(
        subline(&card),
        "requested by andy · widens lanes and budget"
    );
}

#[gpui::test]
fn apply_turns_card_into_applied_with_undo(cx: &mut TestAppContext) {
    let (backend, sent, cx) = host(cx, Screen::SettingsFromChat);
    click(cx, "chg-fx1-lanes-budget:apply");
    assert_eq!(*sent.borrow(), vec![Action::Apply(id())]);
    assert_eq!(state(&backend), ChangeState::Applied);
    assert!(shown(cx, "chg-fx1-lanes-budget:applied"));
    assert!(shown(cx, "chg-fx1-lanes-budget:undo"));
    assert!(shown(cx, "chg-fx1-lanes-budget:audit-log"));
    assert!(!shown(cx, "chg-fx1-lanes-budget:apply"));
    assert!(!shown(cx, "chg-fx1-lanes-budget:cancel"));
}

#[gpui::test]
fn cancel_shows_cancelled_card_without_buttons(cx: &mut TestAppContext) {
    let (backend, sent, cx) = host(cx, Screen::SettingsFromChat);
    click(cx, "chg-fx1-lanes-budget:cancel");
    assert_eq!(*sent.borrow(), vec![Action::Cancel(id())]);
    assert_eq!(state(&backend), ChangeState::Cancelled);
    assert!(shown(cx, "chg-fx1-lanes-budget:cancelled"));
    for button in [
        "chg-fx1-lanes-budget:apply",
        "chg-fx1-lanes-budget:cancel",
        "chg-fx1-lanes-budget:undo",
        "chg-fx1-lanes-budget:audit-log",
    ] {
        assert!(!shown(cx, button), "{button} is drawn on a cancelled card");
    }
    let card = backend.snapshot().changes.values().next().unwrap().clone();
    assert_eq!(subline(&card), "requested by andy · nothing changed");
}

#[gpui::test]
fn undo_returns_to_pending(cx: &mut TestAppContext) {
    let (backend, sent, cx) = host(cx, Screen::SettingsApplied);
    assert!(shown(cx, "chg-fx1-lanes-budget:applied"));
    click(cx, "chg-fx1-lanes-budget:undo");
    assert_eq!(*sent.borrow(), vec![Action::Undo(id())]);
    assert_eq!(state(&backend), ChangeState::Pending);
    assert!(shown(cx, "chg-fx1-lanes-budget:waiting"));
    assert!(shown(cx, "chg-fx1-lanes-budget:apply"));
    assert!(shown(cx, "chg-fx1-lanes-budget:cancel"));
    assert!(!shown(cx, "chg-fx1-lanes-budget:undo"));
}

#[gpui::test]
fn narrowing_card_shows_applied(cx: &mut TestAppContext) {
    init(cx);
    let card = crate::backend::ChangeCard {
        id: ChangeId("chg-narrow".into()),
        requested_by: "andy".into(),
        widens: false,
        rows: vec![ChangeRow {
            target: "andy/fx1".into(),
            setting: "budget".into(),
            before: "$8.00".into(),
            after: "$5.00".into(),
        }],
        state: ChangeState::Applied,
    };
    assert_eq!(subline(&card), "requested by andy · applied by agmend");
    let mut pending = card.clone();
    pending.state = ChangeState::Pending;
    assert_eq!(subline(&pending), "requested by andy · narrows budget");
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sink = sent.clone();
    let (_, cx) = cx.add_window_view(move |_, _| {
        Lone(Box::new(move || {
            let sink = sink.clone();
            ChangeCard::new(card.clone(), &[])
                .on_action(move |action, _, _| sink.borrow_mut().push(action))
                .into_any_element()
        }))
    });
    redraw(cx);
    assert!(shown(cx, "chg-narrow:applied"));
    assert!(!shown(cx, "chg-narrow:apply"));
    click(cx, "chg-narrow:undo");
    assert_eq!(
        *sent.borrow(),
        vec![Action::Undo(ChangeId("chg-narrow".into()))]
    );
}

#[gpui::test]
fn audit_log_is_not_a_link(cx: &mut TestAppContext) {
    let (backend, sent, cx) = host(cx, Screen::SettingsApplied);
    click(cx, "chg-fx1-lanes-budget:audit-log");
    assert!(sent.borrow().is_empty());
    assert_eq!(state(&backend), ChangeState::Applied);
    assert!(shown(cx, "chg-fx1-lanes-budget:applied"));
}

#[test]
fn before_value_struck_through() {
    let row = ChangeRow {
        target: "andy/fx1".into(),
        setting: "budget".into(),
        before: "$5.00".into(),
        after: "$8.00".into(),
    };
    assert_eq!(
        before_label(&row, ChangeState::Pending),
        Label::new("$5.00")
            .code()
            .color(Color::Error)
            .strikethrough()
    );
    assert_eq!(
        before_label(&row, ChangeState::Cancelled),
        Label::new("$5.00")
            .code()
            .color(Color::Placeholder)
            .strikethrough()
    );
}

/// The fixture card for `screen`, drawn in a column `width` wide.
fn card_window(
    cx: &mut TestAppContext,
    screen: Screen,
    width: gpui::Pixels,
) -> (crate::backend::ChangeCard, &mut VisualTestContext) {
    init(cx);
    let snapshot = FixtureBackend::for_screen(screen).unwrap().snapshot();
    let card = snapshot.changes.values().next().unwrap().clone();
    let roster = snapshot.roster;
    let drawn = card.clone();
    let (_, cx) = cx.add_window_view(move |_, _| {
        let (card, roster) = (drawn.clone(), roster.clone());
        Lone(Box::new(move || {
            div()
                .w(width)
                .child(ChangeCard::new(card.clone(), &roster))
                .into_any_element()
        }))
    });
    redraw(cx);
    (card, cx)
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> gpui::Bounds<gpui::Pixels> {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} not drawn"))
}

/// Asserts `screen`'s card fills a column `width` wide, every part of it
/// ends inside the card, and each old value keeps the width it has in a wide
/// card (the row wraps instead of squeezing it).
fn assert_card_fits(cx: &mut TestAppContext, screen: Screen, width: gpui::Pixels) {
    let mut wide_cx = TestAppContext::single();
    let (card, wide) = card_window(&mut wide_cx, screen, gpui::px(800.));
    let full: Vec<gpui::Pixels> = (0..card.rows.len())
        .map(|i| bounds(wide, &format!("{}:before:{i}", card.id)).size.width)
        .collect();
    let (card, cx) = card_window(cx, screen, width);
    let outer = bounds(cx, &format!("{}:card", card.id));
    assert_eq!(outer.size.width, width, "{screen:?}");
    for part in part_selectors(&card) {
        let inner = bounds(cx, &part);
        assert!(
            inner.right() <= outer.right(),
            "{screen:?} at {width:?}: {part} {inner:?} past {outer:?}"
        );
    }
    for (i, full) in full.into_iter().enumerate() {
        let before = bounds(cx, &format!("{}:before:{i}", card.id));
        assert_eq!(
            before.size.width, full,
            "{screen:?} at {width:?}: before:{i} squeezed"
        );
        let after = bounds(cx, &format!("{}:after:{i}", card.id));
        assert!(
            after.size.width >= CHANGE_VALUE_MIN_WIDTH,
            "{screen:?} at {width:?}: after:{i} is {after:?}"
        );
    }
}

fn assert_fixture_card_renders(cx: &mut TestAppContext, screen: Screen) {
    let (card, cx) = card_window(cx, screen, gpui::px(800.));
    let size = bounds(cx, &format!("{}:card", card.id)).size;
    assert_eq!(size.width, CHANGE_CARD_WIDTH, "{screen:?}");
    assert!(size.height > gpui::px(60.), "{screen:?}");
    // At full width each row stays on one line, and so does the head.
    for i in 0..card.rows.len() {
        let target = bounds(cx, &format!("{}:target:{i}", card.id));
        let after = bounds(cx, &format!("{}:after:{i}", card.id));
        assert!(after.top() < target.bottom(), "{screen:?} row {i} wrapped");
    }
    let title = bounds(cx, &format!("{}:title", card.id));
    let subline = bounds(cx, &format!("{}:subline", card.id));
    assert!(subline.top() < title.bottom(), "{screen:?} head wrapped");
}

#[gpui::test]
fn card_fits_a_narrow_pane(cx: &mut TestAppContext) {
    // 300 px: a two-pane split; 276 px: three panes in a 1440 px window
    // with both docks open.
    let mut screens = [Screen::SettingsFromChat, Screen::SettingsApplied]
        .into_iter()
        .flat_map(|screen| [(screen, gpui::px(300.)), (screen, gpui::px(276.))]);
    let (screen, width) = screens.next().unwrap();
    assert_card_fits(cx, screen, width);
    for (screen, width) in screens {
        assert_card_fits(&mut TestAppContext::single(), screen, width);
    }
}

#[gpui::test]
fn settings_from_chat_card_renders(cx: &mut TestAppContext) {
    assert_fixture_card_renders(cx, Screen::SettingsFromChat);
}

#[gpui::test]
fn settings_applied_card_renders(cx: &mut TestAppContext) {
    assert_fixture_card_renders(cx, Screen::SettingsApplied);
}

#[test]
fn settings_join_with_and() {
    assert_eq!(join_settings(["lanes"]), "lanes");
    assert_eq!(join_settings(["lanes", "budget"]), "lanes and budget");
    assert_eq!(
        join_settings(["lanes", "budget", "lanes", "tools"]),
        "lanes, budget and tools"
    );
}

#[gpui::test]
fn long_before_value_wraps_inside_the_card(cx: &mut TestAppContext) {
    init(cx);
    let snapshot = FixtureBackend::for_screen(Screen::SettingsFromChat)
        .unwrap()
        .snapshot();
    let mut card = snapshot.changes.values().next().unwrap().clone();
    card.rows[0].before = "andy, sean, adam, coordinators, ".repeat(12);
    let roster = snapshot.roster;
    let (_, cx) = cx.add_window_view(move |_, _| {
        let (card, roster) = (card.clone(), roster.clone());
        Lone(Box::new(move || {
            ChangeCard::new(card.clone(), &roster).into_any_element()
        }))
    });
    redraw(cx);
    let before = cx.debug_bounds("chg-fx1-lanes-budget:before:0").unwrap();
    let head = cx.debug_bounds("chg-fx1-lanes-budget:waiting").unwrap();
    assert!(before.right() <= head.right(), "{before:?} past {head:?}");
}

#[gpui::test]
fn undone_narrowing_card_has_no_buttons(cx: &mut TestAppContext) {
    init(cx);
    let card = crate::backend::ChangeCard {
        id: ChangeId("chg-narrow".into()),
        requested_by: "andy".into(),
        widens: false,
        rows: vec![ChangeRow {
            target: "andy/fx1".into(),
            setting: "budget".into(),
            before: "$8.00".into(),
            after: "$5.00".into(),
        }],
        state: ChangeState::Undone,
    };
    assert_eq!(subline(&card), "requested by andy · settings restored");
    let (_, cx) = cx.add_window_view(move |_, _| {
        let card = card.clone();
        Lone(Box::new(move || {
            ChangeCard::new(card.clone(), &[]).into_any_element()
        }))
    });
    redraw(cx);
    assert!(shown(cx, "chg-narrow:undone"));
    for button in ["chg-narrow:apply", "chg-narrow:cancel", "chg-narrow:undo"] {
        assert!(cx.debug_bounds(button).is_none(), "{button} shown");
    }
}
