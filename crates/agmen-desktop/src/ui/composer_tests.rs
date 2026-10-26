use std::cell::RefCell;
use std::rc::Rc;

use gpui::{TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{BackendError, chat_snapshot};
use crate::ui::test_support::{click, init};
use crate::ui::text_input::bind_keys;
use crate::ui::tokens::MOCKUP_REM;

type Sent = Rc<RefCell<Vec<Draft>>>;

/// The id of the roster agent with `handle`.
fn agent_id(handle: &str) -> IdentityId {
    chat_snapshot()
        .roster
        .iter()
        .flat_map(|g| &g.agents)
        .find(|a| a.handle == handle)
        .map(|a| a.id.clone())
        .unwrap_or_else(|| panic!("{handle} is in the fixture roster"))
}

/// A focused composer over the chat fixture whose sends land in `Sent`.
fn open_composer(cx: &mut TestAppContext) -> (Entity<Composer>, Sent, &mut VisualTestContext) {
    init(cx);
    cx.update(bind_keys);
    let sent = Sent::default();
    let sink = sent.clone();
    let snapshot = chat_snapshot();
    let (composer, cx) = cx.add_window_view(|window, cx| {
        window.set_rem_size(MOCKUP_REM);
        Composer::new(
            &snapshot,
            move |draft, _, _| {
                sink.borrow_mut().push(draft);
                Ok(())
            },
            window,
            cx,
        )
    });
    composer.update_in(cx, |composer, window, cx| {
        composer.input.focus_handle(cx).focus(window, cx);
    });
    cx.run_until_parked();
    (composer, sent, cx)
}

#[gpui::test]
fn composer_sends_to_listener_by_default(cx: &mut TestAppContext) {
    let (composer, sent, cx) = open_composer(cx);
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.target(cx).handle, "andy/listen");
    });
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        *sent.borrow(),
        vec![Draft {
            to: vec![agent_id("andy/listen")],
            body: "hi".into(),
            reply_to: None,
        }]
    );
}

#[gpui::test]
fn refused_draft_comes_back_into_the_input(cx: &mut TestAppContext) {
    init(cx);
    cx.update(bind_keys);
    let snapshot = chat_snapshot();
    let (composer, cx) = cx.add_window_view(|window, cx| {
        window.set_rem_size(MOCKUP_REM);
        Composer::new(
            &snapshot,
            |_, _, _| Err(BackendError::Unsupported("sending in this test")),
            window,
            cx,
        )
    });
    composer.update_in(cx, |composer, window, cx| {
        composer.input.focus_handle(cx).focus(window, cx);
    });
    cx.simulate_input("hi there");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.input.read(cx).text(), "hi there");
    });
}

#[gpui::test]
fn composer_at_mention_overrides_target(cx: &mut TestAppContext) {
    let (composer, sent, cx) = open_composer(cx);
    cx.simulate_input("@andy/fx1, rebase on main");
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.target(cx).handle, "andy/fx1");
    });
    cx.simulate_keystrokes("enter");
    assert_eq!(
        *sent.borrow(),
        vec![Draft {
            to: vec![agent_id("andy/fx1")],
            body: "@andy/fx1, rebase on main".into(),
            reply_to: None,
        }]
    );
    // The input is empty again, so the chip is back on the listener.
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.target(cx).handle, "andy/listen");
    });
}

#[gpui::test]
fn unknown_mention_stays_text_and_goes_to_listener(cx: &mut TestAppContext) {
    let (_, sent, cx) = open_composer(cx);
    cx.simulate_input("@nobody/x hello");
    cx.simulate_keystrokes("enter");
    let sent = sent.borrow();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, vec![agent_id("andy/listen")]);
    assert_eq!(sent[0].body, "@nobody/x hello");
}

#[gpui::test]
fn mention_after_the_start_does_not_retarget(cx: &mut TestAppContext) {
    let (_, sent, cx) = open_composer(cx);
    cx.simulate_input("ask @andy/fx1 about it");
    cx.simulate_keystrokes("enter");
    assert_eq!(sent.borrow()[0].to, vec![agent_id("andy/listen")]);
}

#[gpui::test]
fn model_chip_asks_for_the_picker(cx: &mut TestAppContext) {
    let (composer, _, cx) = open_composer(cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&composer, move |_, event: &ComposerEvent, _| {
            sink.borrow_mut().push(*event);
        })
        .detach();
    });
    click(cx, "composer:model");
    assert_eq!(*events.borrow(), vec![ComposerEvent::PickModel]);
}

#[test]
fn enter_hint_shows_once() {
    // The ⏎ hint follows the input, so the placeholder does not repeat it
    // (reference sheet mismatch 6).
    assert!(!PLACEHOLDER.contains('⏎'));
}

#[test]
fn mention_handle_reads_the_leading_token() {
    assert_eq!(mention_handle("@andy/fx1 go"), Some("andy/fx1"));
    assert_eq!(mention_handle("  @sean: hi"), Some("sean"));
    assert_eq!(mention_handle("@andy/fx1"), Some("andy/fx1"));
    assert_eq!(mention_handle("hi @andy/fx1"), None);
    assert_eq!(mention_handle("@ hi"), None);
    assert_eq!(mention_handle(""), None);
}
