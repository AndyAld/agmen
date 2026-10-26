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
            model: None,
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
            model: None,
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
fn whitespace_only_enter_sends_nothing(cx: &mut TestAppContext) {
    let (composer, sent, cx) = open_composer(cx);
    cx.simulate_input("   ");
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_keystrokes("enter");
    assert!(sent.borrow().is_empty(), "{:?}", sent.borrow());
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.input.read(cx).text(), "   \n");
    });
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

#[gpui::test]
fn direct_composer_sends_to_its_agent(cx: &mut TestAppContext) {
    init(cx);
    cx.update(bind_keys);
    let sent = Sent::default();
    let sink = sent.clone();
    let snapshot = chat_snapshot();
    let fx1 = Target {
        id: agent_id("andy/fx1"),
        handle: "andy/fx1".into(),
    };
    let (composer, cx) = cx.add_window_view(|window, cx| {
        window.set_rem_size(MOCKUP_REM);
        Composer::direct(
            &snapshot,
            fx1,
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
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.target(cx).handle, "andy/fx1");
    });
    cx.simulate_input("rebase first");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        *sent.borrow(),
        vec![Draft {
            to: vec![agent_id("andy/fx1")],
            body: "rebase first".into(),
            reply_to: None,
            model: None,
        }]
    );
}

#[gpui::test]
fn reply_goes_to_the_alerts_sender_once(cx: &mut TestAppContext) {
    // Task 13: an alert's Reply addresses its sender and replies to it, then
    // the composer goes back to the listener.
    let (composer, sent, cx) = open_composer(cx);
    // Task 13: Escape ends the reply through the composer's binding.
    cx.update(|_, cx| cx.bind_keys(crate::keymap::bindings_in(crate::keymap::COMPOSER_CONTEXT)));
    let alert: agmen_proto::MessageId = "01JD0000000000000000000010".parse().unwrap();
    let coord = Target {
        id: agent_id("adam/coord"),
        handle: "adam/coord".into(),
    };
    cx.update(|window, cx| window.blur(cx));
    composer.update_in(cx, |composer, window, cx| {
        composer.reply(coord.clone(), alert.clone(), window, cx);
    });
    composer.update_in(cx, |composer, window, cx| {
        assert!(composer.input.focus_handle(cx).is_focused(window));
        assert_eq!(composer.target(cx), &coord);
    });
    cx.simulate_input("Revert #10 now");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        *sent.borrow(),
        vec![
            Draft {
                to: vec![coord.id.clone()],
                body: "Revert #10 now".into(),
                reply_to: Some(alert),
                model: None,
            },
            Draft {
                to: vec![agent_id("andy/listen")],
                body: "hi".into(),
                reply_to: None,
                model: None,
            },
        ]
    );
}

/// A focused composer replying to adam/coord's alert, with what it sends.
fn replying(cx: &mut TestAppContext) -> (Entity<Composer>, Sent, &mut VisualTestContext, Target) {
    let (composer, sent, cx) = open_composer(cx);
    // Task 13: Escape ends the reply through the composer's binding.
    cx.update(|_, cx| cx.bind_keys(crate::keymap::bindings_in(crate::keymap::COMPOSER_CONTEXT)));
    let alert: agmen_proto::MessageId = "01JD0000000000000000000010".parse().unwrap();
    let coord = Target {
        id: agent_id("adam/coord"),
        handle: "adam/coord".into(),
    };
    composer.update_in(cx, |composer, window, cx| {
        composer.reply(coord.clone(), alert, window, cx);
    });
    crate::ui::test_support::redraw(cx);
    (composer, sent, cx, coord)
}

/// The draft that sends "hi" to the listener, replying to nothing.
fn plain_hi() -> Draft {
    Draft {
        to: vec![agent_id("andy/listen")],
        body: "hi".into(),
        reply_to: None,
        model: None,
    }
}

#[gpui::test]
fn escape_cancels_a_reply(cx: &mut TestAppContext) {
    // Review: a Reply the person changed their mind about must not turn the
    // next message into an ANSWER.
    let (composer, sent, cx, coord) = replying(cx);
    composer.read_with(cx, |composer, cx| assert_eq!(composer.target(cx), &coord));
    cx.simulate_keystrokes("escape");
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.target(cx).handle, "andy/listen");
    });
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    assert_eq!(*sent.borrow(), vec![plain_hi()]);
}

#[gpui::test]
fn the_reply_chips_close_mark_cancels_the_reply(cx: &mut TestAppContext) {
    let (_, sent, cx, _) = replying(cx);
    click(cx, "composer:cancel-reply");
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    assert_eq!(*sent.borrow(), vec![plain_hi()]);
}

#[gpui::test]
fn a_reply_to_a_closed_alert_is_dropped(cx: &mut TestAppContext) {
    // Review: adam answered first; a stale reply would be a second ANSWER.
    let (composer, sent, cx, _) = replying(cx);
    composer.update(cx, |composer, cx| {
        composer.drop_reply_unless(|_| false, cx);
    });
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    assert_eq!(*sent.borrow(), vec![plain_hi()]);
}

#[gpui::test]
fn a_reply_to_an_open_alert_stays(cx: &mut TestAppContext) {
    let (composer, _, cx, coord) = replying(cx);
    composer.update(cx, |composer, cx| {
        composer.drop_reply_unless(|_| true, cx);
    });
    composer.read_with(cx, |composer, cx| assert_eq!(composer.target(cx), &coord));
    assert!(cx.debug_bounds("composer:cancel-reply").is_some());
}

// Task 19: the planning room's composer talks to the planner (node 8927).
#[gpui::test]
fn planning_composer_targets_the_planner(cx: &mut TestAppContext) {
    use crate::backend::{Backend as _, FixtureBackend, Screen};
    init(cx);
    let snapshot = FixtureBackend::for_screen(Screen::Plan).unwrap().snapshot();
    let (composer, cx) =
        cx.add_window_view(|window, cx| Composer::new(&snapshot, |_, _, _| Ok(()), window, cx));
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.target(cx).handle, "sean/plan");
        assert_eq!(composer.model_choice(), None);
        assert_eq!(
            composer.input.read(cx).placeholder_text(),
            "Plan with sean/plan · it runs on sean's machine with their keys"
        );
    });
}
