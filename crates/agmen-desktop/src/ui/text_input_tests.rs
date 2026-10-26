use std::cell::RefCell;
use std::rc::Rc;

use gpui::TestAppContext;

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
