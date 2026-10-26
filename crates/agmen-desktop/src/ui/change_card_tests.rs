use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Context, Render, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{Backend, ChangeId, ChangeRow, ChangeState, FixtureBackend, Screen};
use crate::ui::test_support::{click, init, redraw, rendered_size};
use crate::ui::tokens::{CHANGE_CARD_WIDTH, MOCKUP_REM};

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
    assert_eq!(title(ChangeState::Pending), "Waiting for you");
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
    assert_eq!(title(ChangeState::Applied), "Settings changed");
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

fn assert_fixture_card_renders(cx: &mut TestAppContext, screen: Screen) {
    let snapshot = FixtureBackend::for_screen(screen).unwrap().snapshot();
    let card = snapshot.changes.values().next().unwrap().clone();
    let roster = snapshot.roster;
    let size = rendered_size(cx, move || {
        ChangeCard::new(card.clone(), &roster).into_any_element()
    });
    assert_eq!(size.width, CHANGE_CARD_WIDTH, "{screen:?}");
    assert!(size.height > gpui::px(60.), "{screen:?}");
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
