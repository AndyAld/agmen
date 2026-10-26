use std::cell::RefCell;
use std::rc::Rc;

use gpui::{TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{BackendError, chat_snapshot};
use crate::ui::test_support::{click, clip_right, init, redraw};
use crate::ui::text_input::bind_keys;
use crate::ui::tokens::{AUTO_LINE_13, COMPOSER_INPUT_MIN_WIDTH, MOCKUP_REM};

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

#[gpui::test]
fn placeholder_is_the_mockups(cx: &mut TestAppContext) {
    // Node 384 keeps "· ⏎ send" in the placeholder, beside the ⏎ hint
    // (node 385).
    let (composer, _, cx) = open_composer(cx);
    composer.read_with(cx, |composer, cx| {
        assert_eq!(
            composer.input.read(cx).placeholder_text(),
            "Ask or tell your listener · @ to mention someone else · ⏎ send"
        );
    });
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

/// A focused DM composer for andy/fx1 (node 650) whose sends land in
/// `Sent`.
fn open_direct(cx: &mut TestAppContext) -> (Entity<Composer>, Sent, &mut VisualTestContext) {
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
    (composer, sent, cx)
}

#[gpui::test]
fn the_input_row_is_the_mockups_16_px_line(cx: &mut TestAppContext) {
    // Node 650: padding 8, a 16 px line for SF 13 and a 1 px border, 34 px
    // in all. With no line height set, gpui's default of 1.618 times the
    // size made the row 21 px and the box 39.
    let (composer, _, cx) = open_direct(cx);
    redraw(cx);
    let input = cx.debug_bounds("composer:input").unwrap();
    assert_eq!(input.size.height, AUTO_LINE_13);
    let frame = cx.debug_bounds("composer:box").unwrap();
    assert_eq!(
        frame.size.height,
        AUTO_LINE_13 + SPACE_8 * 2. + BORDER_WIDTH * 2.
    );
    let caret = composer.read_with(cx, |c, cx| c.input.read(cx).last_cursor.unwrap());
    assert_eq!(caret.size.height, AUTO_LINE_13);
    // Typed text keeps the same line.
    cx.simulate_input("rebase first");
    redraw(cx);
    let input = cx.debug_bounds("composer:input").unwrap();
    assert_eq!(input.size.height, AUTO_LINE_13);
}

#[gpui::test]
fn the_chat_composers_input_row_is_the_mockups_16_px_line(cx: &mut TestAppContext) {
    // Node 384: the placeholder beside the chips is a 16 px line too.
    let (_, _, cx) = open_composer(cx);
    redraw(cx);
    let input = cx.debug_bounds("composer:input").unwrap();
    assert_eq!(input.size.height, AUTO_LINE_13);
}

#[gpui::test]
fn direct_composer_sends_to_its_agent(cx: &mut TestAppContext) {
    let (composer, sent, cx) = open_direct(cx);
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.target(cx).handle, "andy/fx1");
    });
    cx.simulate_input("rebase first");
    cx.simulate_keystrokes("enter");
    // Node 650 draws no "to" chip, so the DM keeps its agent: a leading
    // @mention stays text in the DM.
    cx.simulate_input("@adam/coord ok?");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        *sent.borrow(),
        vec![
            Draft {
                to: vec![agent_id("andy/fx1")],
                body: "rebase first".into(),
                reply_to: None,
                model: None,
            },
            Draft {
                to: vec![agent_id("andy/fx1")],
                body: "@adam/coord ok?".into(),
                reply_to: None,
                model: None,
            },
        ]
    );
    assert!(cx.debug_bounds("composer:to").is_none());
    assert!(cx.debug_bounds("composer:input").is_some());
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

/// The alert `replying` answers.
fn alert() -> agmen_proto::MessageId {
    "01JD0000000000000000000010".parse().unwrap()
}

/// A focused composer replying to adam/coord's alert, with what it sends.
fn replying(cx: &mut TestAppContext) -> (Entity<Composer>, Sent, &mut VisualTestContext, Target) {
    let (composer, sent, cx) = open_composer(cx);
    // Task 13: Escape ends the reply through the composer's binding.
    cx.update(|_, cx| cx.bind_keys(crate::keymap::bindings_in(crate::keymap::COMPOSER_CONTEXT)));
    let coord = Target {
        id: agent_id("adam/coord"),
        handle: "adam/coord".into(),
    };
    composer.update_in(cx, |composer, window, cx| {
        composer.reply(coord.clone(), alert(), window, cx);
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
fn a_mention_of_another_agent_drops_the_reply(cx: &mut TestAppContext) {
    // C232: "@sean/a1 status?" after Reply on adam/coord's alert goes to
    // sean/a1 as a plain message. Replying to the alert would make it an
    // ANSWER that closes the alert while adam/coord never sees it.
    let (_, sent, cx, _) = replying(cx);
    cx.simulate_input("@sean/a1 status?");
    crate::ui::test_support::redraw(cx);
    assert!(
        cx.debug_bounds("composer:cancel-reply").is_none(),
        "the chip shows the reply is dropped"
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(
        *sent.borrow(),
        vec![Draft {
            to: vec![agent_id("sean/a1")],
            body: "@sean/a1 status?".into(),
            reply_to: None,
            model: None,
        }]
    );
}

#[gpui::test]
fn a_mention_of_the_alerts_sender_keeps_the_reply(cx: &mut TestAppContext) {
    let (_, sent, cx, coord) = replying(cx);
    cx.simulate_input("@adam/coord ok");
    crate::ui::test_support::redraw(cx);
    assert!(cx.debug_bounds("composer:cancel-reply").is_some());
    cx.simulate_keystrokes("enter");
    assert_eq!(
        *sent.borrow(),
        vec![Draft {
            to: vec![coord.id],
            body: "@adam/coord ok".into(),
            reply_to: Some(alert()),
            model: None,
        }]
    );
}

#[gpui::test]
fn plain_text_after_a_reply_keeps_the_reply(cx: &mut TestAppContext) {
    let (_, sent, cx, coord) = replying(cx);
    cx.simulate_input("on it");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        *sent.borrow(),
        vec![Draft {
            to: vec![coord.id],
            body: "on it".into(),
            reply_to: Some(alert()),
            model: None,
        }]
    );
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

/// Asserts the composer's input is at least [`COMPOSER_INPUT_MIN_WIDTH`]
/// wide and paints its caret inside itself (C260).
fn assert_input_keeps_its_caret(composer: &Entity<Composer>, cx: &mut VisualTestContext) {
    let input = cx.debug_bounds("composer:input").unwrap();
    assert!(input.size.width >= COMPOSER_INPUT_MIN_WIDTH, "{input:?}");
    let (area, caret) = composer.read_with(cx, |c, cx| {
        let input = c.input.read(cx);
        (input.last_bounds.unwrap(), input.last_cursor.unwrap())
    });
    assert!(
        caret.left() >= area.left() && caret.right() <= area.right(),
        "{caret:?} outside {area:?}"
    );
    let painted = cx.update(|window, _| {
        let scale = window.scale_factor();
        window.painted_quads().into_iter().any(|quad| {
            (quad.bounds.origin.x.0 / scale - f32::from(caret.left())).abs() < 0.5
                && (quad.bounds.origin.y.0 / scale - f32::from(caret.top())).abs() < 0.5
                && (quad.bounds.size.width.0 / scale - f32::from(caret.size.width)).abs() < 0.5
        })
    });
    assert!(painted, "the caret at {caret:?} is not painted");
}

/// Asserts every part of the composer's row ends inside its box.
fn assert_parts_in_the_box(cx: &mut VisualTestContext) {
    let frame = cx.debug_bounds("composer:box").unwrap();
    for part in [
        "composer:to",
        "composer:model",
        "composer:input",
        "composer:send-hint",
    ] {
        let bounds = cx
            .debug_bounds(part)
            .unwrap_or_else(|| panic!("{part} is not drawn"));
        assert!(
            bounds.right() <= frame.right(),
            "{part} {bounds:?} past {frame:?}"
        );
    }
}

#[gpui::test]
fn a_narrow_reply_keeps_the_input_and_the_close_mark(cx: &mut TestAppContext) {
    // C260: at a 420 px window the chips took the whole row, so the input
    // was 0 px wide with no caret or text, and the box cut the reply chip's
    // close mark, leaving Escape as the only way out of a reply. At 320 px
    // the reply chip still cut 2.5 px off its close mark while the model
    // chip kept its width.
    let (composer, sent, cx, coord) = replying(cx);
    let whole = cx.debug_bounds("composer:cancel-reply").unwrap();
    for width in [320., 420., 500.] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(120.)));
        composer.update(cx, |composer, cx| composer.cancel_reply(cx));
        redraw(cx);
        assert!(cx.debug_bounds("composer:cancel-reply").is_none());
        assert_input_keeps_its_caret(&composer, cx);
        assert_parts_in_the_box(cx);
        composer.update_in(cx, |composer, window, cx| {
            composer.reply(coord.clone(), alert(), window, cx);
        });
        redraw(cx);
        assert_input_keeps_its_caret(&composer, cx);
        assert_parts_in_the_box(cx);
        let close = cx.debug_bounds("composer:cancel-reply").unwrap();
        let chip = cx.debug_bounds("composer:to").unwrap();
        let frame = cx.debug_bounds("composer:box").unwrap();
        assert_eq!(close.size, whole.size, "{width} px: {close:?}");
        assert!(
            close.left() >= chip.left() && close.right() <= chip.right(),
            "{width} px: {close:?} cut by {chip:?}"
        );
        assert!(
            close.left() >= frame.left() && close.right() <= frame.right(),
            "{width} px: {close:?} cut by {frame:?}"
        );
    }
    click(cx, "composer:cancel-reply");
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    assert_eq!(*sent.borrow(), vec![plain_hi()]);
}

#[gpui::test]
fn a_narrow_composer_keeps_its_chips_and_hint_in_the_box(cx: &mut TestAppContext) {
    // Node 375 clips its content. In a narrow window the chips and the ⏎
    // drew past the box's right edge, over the right dock.
    // C278: from 200 px the model chip hides; at 260 px both chips show,
    // cut.
    let (composer, _, cx) = open_composer(cx);
    resize(cx, 260.);
    assert_input_keeps_its_caret(&composer, cx);
    assert_parts_in_the_box(cx);
    // The wrappers above report their shrunken boxes, so each chip must
    // also be clipped there: its label and caret show no further than the
    // next part's left edge.
    for (chip, next) in [
        ("composer:to", "composer:model"),
        ("composer:model", "composer:input"),
    ] {
        let clip = clip_right(cx, chip).unwrap_or_else(|| panic!("{chip} paints no fill"));
        let next = cx.debug_bounds(next).unwrap();
        assert!(
            clip <= next.left(),
            "{chip} shows to {clip:?}, over {next:?}"
        );
    }
}

/// Resizes the window to `width` and draws twice: the first frame
/// measures the composer's box, the second lays the row out for it.
fn resize(cx: &mut VisualTestContext, width: f32) {
    cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(120.)));
    redraw(cx);
    redraw(cx);
}

/// Asserts nothing the composer paints shows outside its box: every
/// painted quad is clipped to the box or lies inside it.
fn assert_nothing_past_the_box(cx: &mut VisualTestContext, at: &str) {
    let frame = cx.debug_bounds("composer:box").unwrap();
    let outside: Vec<_> = cx.update(|window, _| {
        let scale = window.scale_factor();
        window
            .painted_quads()
            .into_iter()
            .filter_map(|quad| {
                let shown = quad.bounds.intersect(&quad.content_mask.bounds);
                let (left, right) = (
                    gpui::px(shown.left().0 / scale),
                    gpui::px(shown.right().0 / scale),
                );
                let inside = left >= frame.left() - gpui::px(0.01)
                    && right <= frame.right() + gpui::px(0.01);
                (right > left && !inside).then_some((left, right))
            })
            .collect()
    });
    assert!(outside.is_empty(), "{at}: {outside:?} shows past {frame:?}");
}

#[gpui::test]
fn a_200_px_composer_hides_the_model_chip_and_keeps_the_input(cx: &mut TestAppContext) {
    // C278: while replying, the reply chip's floor, the input's 80 px and
    // the ⏎ ran past the box below about 245 px, and the model chip was a
    // sliver with neither ◆ nor ▾. The model chip hides, the input keeps
    // its floor and the box clips the ⏎.
    let (composer, sent, cx, coord) = replying(cx);
    let whole = cx.debug_bounds("composer:cancel-reply").unwrap();
    for reply in [false, true] {
        if reply {
            composer.update_in(cx, |composer, window, cx| {
                composer.reply(coord.clone(), alert(), window, cx);
            });
        } else {
            composer.update(cx, |composer, cx| composer.cancel_reply(cx));
        }
        resize(cx, 200.);
        let at = if reply { "replying" } else { "not replying" };
        assert!(
            cx.debug_bounds("composer:model").is_none(),
            "{at}: the model chip is drawn"
        );
        assert_input_keeps_its_caret(&composer, cx);
        assert_nothing_past_the_box(cx, at);
        let frame = cx.debug_bounds("composer:box").unwrap();
        let window = cx.update(|window, _| window.viewport_size());
        assert!(
            frame.right() <= gpui::px(0.) + window.width,
            "{at}: {frame:?}"
        );
        if reply {
            let close = cx.debug_bounds("composer:cancel-reply").unwrap();
            let chip = cx.debug_bounds("composer:to").unwrap();
            assert_eq!(close.size, whole.size, "{close:?}");
            assert!(
                close.left() >= chip.left() && close.right() <= chip.right(),
                "{close:?} cut by {chip:?}"
            );
            assert!(close.right() <= frame.right(), "{close:?} cut by {frame:?}");
        }
    }
    click(cx, "composer:cancel-reply");
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    assert_eq!(*sent.borrow(), vec![plain_hi()]);
}

#[gpui::test]
fn a_narrowing_reply_cuts_its_handle_before_the_model_chip(cx: &mut TestAppContext) {
    // C277: removing the reply chip's shrink factor let the model chip
    // shrink first, and no test saw it. At 700 px nothing is cut; at
    // 300 px the reply chip is at its floor.
    let (_, _, cx, _) = replying(cx);
    resize(cx, 700.);
    let (to_full, model_full) = (
        cx.debug_bounds("composer:to").unwrap().size.width,
        cx.debug_bounds("composer:model").unwrap().size.width,
    );
    resize(cx, 300.);
    let to_floor = cx.debug_bounds("composer:to").unwrap().size.width;
    resize(cx, 450.);
    let to = cx.debug_bounds("composer:to").unwrap().size.width;
    let model = cx.debug_bounds("composer:model").unwrap().size.width;
    assert!(
        to < to_full && to > to_floor,
        "the reply chip {to:?} is cut, above its {to_floor:?} floor"
    );
    assert_eq!(model, model_full, "the model chip keeps its width");
}

#[gpui::test]
fn a_cut_model_chip_keeps_its_caret(cx: &mut TestAppContext) {
    // C277: removing the model chip's truncate left its label whole, so
    // a narrow box cut its ▾ off, and no test saw it.
    let (_, _, cx, _) = replying(cx);
    resize(cx, 700.);
    let full = cx.debug_bounds("composer:model").unwrap().size.width;
    let caret = cx.debug_bounds("composer:model-caret").unwrap().size;
    resize(cx, 300.);
    let chip = cx.debug_bounds("composer:model").unwrap();
    let cut = cx.debug_bounds("composer:model-caret").unwrap();
    assert!(chip.size.width < full, "the chip {chip:?} is not cut");
    assert_eq!(cut.size, caret);
    assert!(
        cut.left() >= chip.left() && cut.right() <= chip.right(),
        "the ▾ {cut:?} is cut by the chip {chip:?}"
    );
}

/// The window width at which the composer's parts all sit at their floors:
/// the "to" chip, the model chip, the input and the ⏎, with the gaps
/// between them, plus the composer's padding, border and box padding,
/// measured from a wide window.
fn threshold(composer: &Entity<Composer>, cx: &mut VisualTestContext) -> (f32, Floors) {
    resize(cx, 900.);
    let replying = composer.read_with(cx, |composer, cx| {
        composer.reply_for(composer.input.read(cx).text()).is_some()
    });
    let lead = if replying { "reply to" } else { "to" };
    let floors = cx.update(|window, cx| {
        let fonts = cx.global::<Appearance>().fonts.clone();
        Floors::measure(lead, replying, &fonts, window)
    });
    let to = cx.debug_bounds("composer:to").unwrap();
    let hint = cx.debug_bounds("composer:send-hint").unwrap();
    let chrome = gpui::px(900.) - (hint.right() - to.left());
    let parts = floors.to
        + SPACE_8
        + floors.model
        + SPACE_8
        + COMPOSER_INPUT_MIN_WIDTH
        + SPACE_8
        + floors.hint;
    (f32::from(chrome + parts), floors)
}

#[gpui::test]
fn the_model_chip_shows_from_the_width_its_floor_fits(cx: &mut TestAppContext) {
    // C281: the box probe measures inside the border and model_fits took
    // the border off again, so the chip hid 2 px early. At the width where
    // every part fits at its floor the chip shows, at its floor; 1 px
    // narrower it hides.
    let (composer, _, cx, coord) = replying(cx);
    for reply in [false, true] {
        if reply {
            composer.update_in(cx, |composer, window, cx| {
                composer.reply(coord.clone(), alert(), window, cx);
            });
        } else {
            composer.update(cx, |composer, cx| composer.cancel_reply(cx));
        }
        let (exact, floors) = threshold(&composer, cx);
        let at = if reply { "replying" } else { "not replying" };
        resize(cx, exact.ceil());
        let close = |a: gpui::Pixels, b: gpui::Pixels| (a - b).abs() < gpui::px(1.);
        let model = cx
            .debug_bounds("composer:model")
            .unwrap_or_else(|| panic!("{at}: no model chip at {exact} px"));
        let to = cx.debug_bounds("composer:to").unwrap();
        assert!(close(model.size.width, floors.model), "{at}: {model:?}");
        assert!(close(to.size.width, floors.to), "{at}: {to:?}");
        // The first frame after a narrowing still lays the row out for the
        // box width the last frame measured, so it can draw the chip in
        // less room than its floor; the floor keeps it whole and the box
        // clips the ⏎. The probe's next frame hides the chip within the
        // same update, so this reads that first frame's paint.
        let fill = chip_fills(cx, model)
            .first()
            .map(|(fill, _)| *fill)
            .unwrap();
        resize(cx, exact.ceil() - 3.);
        let shown = cx.update(|window, cx| {
            composer.update(cx, |composer, _| {
                composer.box_width.set(Some(gpui::px(900.)));
            });
            window.refresh();
            drop(window.draw(cx));
            visible_widths(window, fill)
        });
        assert_eq!(shown.len(), 2, "{at}: the to and model chips {shown:?}");
        assert!(
            shown[1] >= floors.model - gpui::px(0.25),
            "{at}: the stale model chip shows {:?} of its {:?} floor",
            shown[1],
            floors.model
        );
        resize(cx, exact.ceil() - 1.);
        assert!(
            cx.debug_bounds("composer:model").is_none(),
            "{at}: the model chip shows below {exact} px"
        );
    }
}

#[gpui::test]
fn a_resize_lays_the_row_out_for_the_new_width_without_a_refresh(cx: &mut TestAppContext) {
    // C281: the box probe learns the box's width while painting, after
    // the row was laid out for the old one, and asks for one more frame.
    // Without that frame a narrowed composer kept a model chip it no
    // longer has room for.
    let (_, _, cx) = open_composer(cx);
    resize(cx, 600.);
    assert!(cx.debug_bounds("composer:model").is_some());
    cx.simulate_resize(gpui::size(gpui::px(200.), gpui::px(120.)));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("composer:model").is_none(),
        "the row kept the model chip it laid out for 600 px"
    );
}

/// The fills painted inside `bounds`, with the visible width of each.
fn chip_fills(
    cx: &mut VisualTestContext,
    bounds: gpui::Bounds<gpui::Pixels>,
) -> Vec<(gpui::Hsla, gpui::Pixels)> {
    cx.update(|window, _| {
        let scale = window.scale_factor();
        window
            .painted_quads()
            .into_iter()
            .filter(|quad| {
                let x = gpui::px(quad.bounds.origin.x.0 / scale);
                x >= bounds.left() - gpui::px(0.5) && x < bounds.right()
            })
            .filter_map(|quad| {
                let fill = quad.background.as_solid()?;
                let shown = quad.bounds.intersect(&quad.content_mask.bounds);
                Some((fill, gpui::px(shown.size.width.0 / scale)))
            })
            .collect()
    })
}

/// The visible widths of the quads filled with `fill`, left to right.
fn visible_widths(window: &mut gpui::Window, fill: gpui::Hsla) -> Vec<gpui::Pixels> {
    let scale = window.scale_factor();
    let mut quads: Vec<_> = window
        .painted_quads()
        .into_iter()
        .filter(|quad| quad.background.as_solid() == Some(fill))
        .collect();
    quads.sort_by(|a, b| a.bounds.origin.x.0.total_cmp(&b.bounds.origin.x.0));
    quads
        .iter()
        .map(|quad| {
            let shown = quad.bounds.intersect(&quad.content_mask.bounds);
            gpui::px(shown.size.width.0 / scale)
        })
        .collect()
}

#[gpui::test]
fn a_hidden_model_chip_closes_its_picker_so_escape_cancels_the_reply(cx: &mut TestAppContext) {
    // C282: the picker stayed open, unseen, when its chip hid in a narrow
    // composer, so the first Escape closed it and the reply stayed.
    let (composer, sent, cx, _) = replying(cx);
    resize(cx, 700.);
    click(cx, "composer:model");
    let open = |composer: &Entity<Composer>, cx: &mut VisualTestContext| {
        composer.read_with(cx, |composer, _| {
            composer.models.as_ref().is_some_and(ModelMenu::is_open)
        })
    };
    assert!(open(&composer, cx), "the picker opens");
    resize(cx, 200.);
    assert!(cx.debug_bounds("composer:model").is_none());
    assert!(!open(&composer, cx), "the hidden chip's picker is open");
    cx.simulate_keystrokes("escape");
    composer.read_with(cx, |composer, cx| {
        assert_eq!(composer.target(cx).handle, "andy/listen");
    });
    cx.simulate_input("hi");
    cx.simulate_keystrokes("enter");
    assert_eq!(*sent.borrow(), vec![plain_hi()]);
}
