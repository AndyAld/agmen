use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AppContext as _, TestAppContext};

use super::*;
use crate::appearance::Appearance;

fn setup(cx: &mut TestAppContext) -> (gpui::Entity<TextInput>, &mut gpui::VisualTestContext) {
    cx.update(|cx| {
        cx.set_global(Appearance::builtin());
        bind_keys(cx);
    });
    let (input, cx) = cx.add_window_view(|_, cx| TextInput::new(cx).placeholder("Ask"));
    input.update_in(cx, |input, window, cx| {
        input.focus_handle(cx).focus(window, cx)
    });
    cx.run_until_parked();
    (input, cx)
}

#[gpui::test]
fn input_inserts_and_submits(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sink = sent.clone();
    cx.update(|_, cx| {
        cx.subscribe(&input, move |_, event: &TextInputEvent, _| {
            let TextInputEvent::Submit(text) = event;
            sink.borrow_mut().push(text.clone());
        })
        .detach();
    });

    cx.simulate_input("hi");
    input.read_with(cx, |input, _| assert_eq!(input.text(), "hi"));
    cx.simulate_keystrokes("enter");
    assert_eq!(*sent.borrow(), vec!["hi".to_owned()]);
    input.read_with(cx, |input, _| assert_eq!(input.text(), ""));

    cx.simulate_keystrokes("enter");
    assert_eq!(sent.borrow().len(), 1, "an empty input submits nothing");
}

#[gpui::test]
fn input_ime_marked_text_commits(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx);
    input.update_in(cx, |input, window, cx| {
        input.replace_and_mark_text_in_range(None, "に", None, window, cx);
        assert_eq!(input.marked_text_range(window, cx), Some(0..1));
        input.replace_text_in_range(None, "日本", window, cx);
        assert_eq!(input.marked_text_range(window, cx), None);
        assert_eq!(input.text(), "日本");
        assert_eq!(input.text_length_utf16(window, cx), Some(2));
    });
}

#[gpui::test]
fn input_backspace_key_deletes(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx);
    cx.simulate_input("ab");
    cx.simulate_keystrokes("backspace");
    input.read_with(cx, |input, _| assert_eq!(input.text(), "a"));
}

/// A 300 px wide window holding one input, so wrapping is testable. Its
/// font is set on the wrapper, as the composer's will be.
struct Host(gpui::Entity<TextInput>);

impl gpui::Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(300.))
            .font_family(".ZedMono")
            .child(self.0.clone())
    }
}

fn setup_with(
    cx: &mut TestAppContext,
    build: impl FnOnce(TextInput) -> TextInput + 'static,
) -> (gpui::Entity<TextInput>, &mut gpui::VisualTestContext) {
    cx.update(|cx| {
        cx.set_global(Appearance::builtin());
        bind_keys(cx);
    });
    let (host, cx) = cx.add_window_view(|_, cx| Host(cx.new(|cx| build(TextInput::new(cx)))));
    let input = host.read_with(cx, |host, _| host.0.clone());
    input.update_in(cx, |input, window, cx| {
        input.focus_handle(cx).focus(window, cx)
    });
    cx.run_until_parked();
    (input, cx)
}

#[gpui::test]
fn shift_enter_inserts_newline_and_enter_sends_all(cx: &mut TestAppContext) {
    let (input, cx) = setup_with(cx, |i| i.multiline(6));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sink = sent.clone();
    cx.update(|_, cx| {
        cx.subscribe(&input, move |_, event: &TextInputEvent, _| {
            let TextInputEvent::Submit(text) = event;
            sink.borrow_mut().push(text.clone());
        })
        .detach();
    });
    cx.simulate_input("a");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("b");
    input.read_with(cx, |input, _| assert_eq!(input.text(), "a\nb"));
    cx.simulate_keystrokes("enter");
    assert_eq!(*sent.borrow(), vec!["a\nb".to_owned()]);
}

#[gpui::test]
fn single_line_ignores_shift_enter(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx);
    cx.simulate_input("a");
    cx.simulate_keystrokes("shift-enter");
    input.read_with(cx, |input, _| assert_eq!(input.text(), "a"));
}

#[gpui::test]
fn set_text_replaces_and_puts_cursor_at_end(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx);
    input.update_in(cx, |input, window, cx| input.set_text("hello", window, cx));
    cx.simulate_input("!");
    input.read_with(cx, |input, _| assert_eq!(input.text(), "hello!"));
}

#[gpui::test]
fn long_token_keeps_cursor_in_bounds(cx: &mut TestAppContext) {
    for multiline in [false, true] {
        let (input, cx) = if multiline {
            setup_with(cx, |i| i.multiline(6))
        } else {
            setup(cx)
        };
        let token = "x".repeat(500);
        input.update_in(cx, |input, window, cx| input.set_text(&token, window, cx));
        cx.run_until_parked();
        input.read_with(cx, |input, _| {
            let (bounds, cursor) = (input.last_bounds.unwrap(), input.last_cursor.unwrap());
            assert!(
                bounds.contains(&cursor.origin),
                "multiline {multiline}: cursor {cursor:?} outside {bounds:?}"
            );
        });
        if multiline {
            input.read_with(cx, |input, _| {
                assert!(input.visual_lines > 1, "a long token wraps")
            });
        }
    }
}

#[gpui::test]
fn multiline_grows_to_max_lines(cx: &mut TestAppContext) {
    let (input, cx) = setup_with(cx, |i| i.multiline(3));
    let tall = (0..10)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    input.update_in(cx, |input, window, cx| input.set_text(&tall, window, cx));
    cx.run_until_parked();
    let lh = cx.update(|window, _| window.line_height());
    input.read_with(cx, |input, _| {
        let height = input.last_bounds.unwrap().size.height;
        assert!(
            (height - lh * 3.0).abs() < gpui::px(0.5),
            "height {height:?}, line {lh:?}"
        );
    });
}

#[gpui::test]
fn up_and_down_move_between_lines(cx: &mut TestAppContext) {
    let (input, cx) = setup_with(cx, |i| i.multiline(6));
    input.update_in(cx, |input, window, cx| {
        input.set_text("abc\ndef", window, cx)
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("up");
    cx.simulate_input("X");
    input.read_with(cx, |input, _| assert_eq!(input.text(), "abcX\ndef"));
    cx.simulate_keystrokes("down");
    cx.simulate_input("Y");
    input.read_with(cx, |input, _| assert_eq!(input.text(), "abcX\ndefY"));
}

#[gpui::test]
fn at_the_cap_a_too_long_insert_keeps_the_selection(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx);
    let full = "a".repeat(MAX_TEXT_BYTES);
    input.update_in(cx, |input, window, cx| {
        input.set_text(&full, window, cx);
        // Select nothing and paste: no room, so nothing changes.
        input.insert("bb", window, cx);
        assert_eq!(input.text().len(), MAX_TEXT_BYTES);
        // Select 1 byte and paste 2: still no room, and the selection stays.
        input.buffer.set_selection(0..1, false);
        input.insert("bb", window, cx);
        assert_eq!(input.text(), full, "the selected byte was not deleted");
    });
}

#[gpui::test]
fn ime_room_comes_from_the_replaced_range(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx);
    let full = "a".repeat(MAX_TEXT_BYTES);
    input.update_in(cx, |input, window, cx| {
        input.set_text(&full, window, cx);
        // The IME replaces 3 bytes with 3: that fits at the cap.
        input.replace_text_in_range(Some(0..3), "xyz", window, cx);
        assert!(input.text().starts_with("xyz"));
        assert_eq!(input.text().len(), MAX_TEXT_BYTES);
        // Marked text goes through the cap too.
        input.replace_and_mark_text_in_range(None, "toolong", None, window, cx);
        assert_eq!(input.text().len(), MAX_TEXT_BYTES);
    });
}

#[gpui::test]
fn layout_is_shaped_once_per_change(cx: &mut TestAppContext) {
    let (input, cx) = setup_with(cx, |i| i.multiline(6));
    input.update_in(cx, |input, window, cx| {
        input.set_text("hello world", window, cx)
    });
    cx.run_until_parked();
    let after_change = input.read_with(cx, |input, _| input.shapes);
    assert!(after_change >= 1);
    // Frames with no change reuse the layout.
    for _ in 0..3 {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }
    input.read_with(cx, |input, _| assert_eq!(input.shapes, after_change));
}

#[gpui::test]
fn layout_uses_the_inherited_font(cx: &mut TestAppContext) {
    // Review C108: measuring must not fall back to the default text style.
    let (input, cx) = setup_with(cx, |i| i.multiline(6));
    input.update_in(cx, |input, window, cx| input.set_text("abc", window, cx));
    cx.run_until_parked();
    input.read_with(cx, |input, _| {
        assert!(input.cached_fonts().next().is_some());
        for font in input.cached_fonts() {
            assert_eq!(font.family.as_ref(), ".ZedMono", "every cached layout");
        }
    });
}

#[gpui::test]
fn a_new_placeholder_color_reshapes(cx: &mut TestAppContext) {
    // Review C116: the empty input draws the placeholder color.
    let (input, cx) = setup_with(cx, |i| i.multiline(6).placeholder("Ask"));
    cx.run_until_parked();
    let before = input.read_with(cx, |input, _| input.shapes);
    cx.update(|_, cx| {
        let mut appearance = cx.global::<Appearance>().clone();
        appearance.theme.text_placeholder = gpui::red();
        cx.set_global(appearance);
    });
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    input.read_with(cx, |input, _| assert!(input.shapes > before));
}
