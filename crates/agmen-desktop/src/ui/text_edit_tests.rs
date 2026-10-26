use super::*;

fn buf(text: &str) -> TextBuffer {
    let mut b = TextBuffer::default();
    b.replace(None, text);
    b
}

#[test]
fn insert_moves_cursor_to_end() {
    let b = buf("hi");
    assert_eq!(b.text(), "hi");
    assert_eq!(b.selection(), 2..2);
}

#[test]
fn input_backspace_by_grapheme() {
    // "e" plus a combining acute, then a thumbs up with a skin tone.
    let mut b = buf("e\u{301}👍🏽");
    assert!(b.backspace());
    assert_eq!(b.text(), "e\u{301}");
    assert!(b.backspace());
    assert_eq!(b.text(), "");
    assert!(!b.backspace(), "backspace at the start does nothing");
}

#[test]
fn delete_forward_by_grapheme() {
    let mut b = buf("👍🏽x");
    b.move_to(0);
    assert!(b.delete());
    assert_eq!(b.text(), "x");
}

#[test]
fn input_utf16_ranges_map_to_utf8() {
    let b = buf("a😀b");
    // UTF-16: a=0, 😀=1..3, b=3. UTF-8: a=0, 😀=1..5, b=5.
    assert_eq!(b.range_from_utf16(&(1..3)), 1..5);
    assert_eq!(b.range_to_utf16(&(5..6)), 3..4);
    // A UTF-16 index inside the surrogate pair lands on the char's start.
    assert_eq!(b.range_from_utf16(&(2..2)), 1..1);
    // Past the end clamps.
    assert_eq!(b.range_from_utf16(&(0..99)), 0..6);
}

#[test]
fn input_ime_marked_text_commits() {
    let mut b = TextBuffer::default();
    b.replace_and_mark(None, "に", None);
    assert_eq!(b.marked(), Some(0.."に".len()));
    b.replace_and_mark(None, "にほ", None);
    assert_eq!(b.text(), "にほ");
    b.replace(None, "日本");
    assert_eq!(b.text(), "日本");
    assert_eq!(b.marked(), None);
    assert_eq!(b.selection(), "日本".len().."日本".len());
}

#[test]
fn select_all_then_type_replaces() {
    let mut b = buf("old");
    b.select_all();
    b.replace(None, "new");
    assert_eq!(b.text(), "new");
}

#[test]
fn left_and_right_collapse_selection() {
    let mut b = buf("abc");
    b.select_all();
    b.move_left();
    assert_eq!(b.selection(), 0..0);
    b.select_all();
    b.move_right();
    assert_eq!(b.selection(), 3..3);
    b.move_left();
    assert_eq!(b.selection(), 2..2);
}

#[test]
fn take_clears() {
    let mut b = buf("send me");
    assert_eq!(b.take(), "send me");
    assert_eq!(b.text(), "");
    assert_eq!(b.selection(), 0..0);
}
