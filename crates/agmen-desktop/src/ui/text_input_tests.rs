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
fn long_placeholder_stays_on_one_line(cx: &mut TestAppContext) {
    // The composer's placeholder is clipped, not wrapped, so an empty
    // composer keeps its one line height (Zed truncates placeholders).
    let placeholder = "Ask or tell your listener · @ to mention someone else ".repeat(3);
    let (input, cx) = setup_with(cx, move |i| i.placeholder(placeholder).multiline(6));
    let lh = cx.update(|window, _| window.line_height());
    input.read_with(cx, |input, _| {
        let height = input.last_bounds.unwrap().size.height;
        assert!(
            (height - lh).abs() < gpui::px(0.5),
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
        assert!(!input.cache.is_empty());
        for key in &input.cache {
            assert_eq!(key.font.family.as_ref(), ".ZedMono", "every cached layout");
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

/// A composer-like input at a UI font of the given rem: the UI family at
/// `TEXT_DEFAULT` with the composer's [`AUTO_LINE_13`] line.
#[cfg(target_os = "macos")]
struct UiFontHost(gpui::Entity<TextInput>, Pixels);

#[cfg(target_os = "macos")]
impl gpui::Render for UiFontHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(self.1);
        let family = cx.global::<Appearance>().fonts.ui_family.clone();
        div()
            .p(px(40.))
            .w(px(400.))
            .font_family(family)
            .text_size(crate::ui::tokens::TEXT_DEFAULT)
            .child(self.0.clone())
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_paint_mask_keeps_descenders_at_a_large_ui_font() {
    // C264a: at a 20 px UI font the text outgrows the 16 px line, which
    // keeps its pitch, and a mask cut to the line box clipped g, j, p, q
    // and y.
    use crate::ui::tokens::AUTO_LINE_13;
    let mut app = crate::ui::test_support::core_text_app();
    crate::ui::test_support::init(&mut app);
    app.update(|cx| {
        crate::assets::load_fonts(cx).unwrap();
        bind_keys(cx);
    });
    let text = "gjpqy jumpy query";
    let (host, cx) = app.add_window_view(|_, cx| {
        UiFontHost(
            cx.new(|cx| TextInput::new(cx).line_height(AUTO_LINE_13)),
            px(20.),
        )
    });
    let input = host.read_with(cx, |host, _| host.0.clone());
    input.update_in(cx, |input, window, cx| {
        input.focus_handle(cx).focus(window, cx);
        input.set_text(text, window, cx);
    });
    cx.run_until_parked();
    crate::ui::test_support::redraw(cx);
    let (area, caret) = input.read_with(cx, |input, _| {
        (input.last_bounds.unwrap(), input.last_cursor.unwrap())
    });
    assert_eq!(area.size.height, AUTO_LINE_13, "the line keeps its pitch");
    let (line, mask, device_px) = cx.update(|window, cx| {
        let font_size = crate::ui::tokens::TEXT_DEFAULT.to_pixels(window.rem_size());
        let family = cx.global::<Appearance>().fonts.ui_family.clone();
        let run = gpui::TextRun {
            len: text.len(),
            font: gpui::font(family),
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(text.into(), font_size, &[run], None);
        let scale = window.scale_factor();
        // The caret is painted under the same mask as the text.
        let mask = window
            .painted_quads()
            .into_iter()
            .find(|quad| {
                (quad.bounds.origin.y.0 / scale - f32::from(caret.top())).abs() < 0.5
                    && (quad.bounds.size.width.0 / scale - f32::from(caret.size.width)).abs() < 0.5
            })
            .map(|quad| quad.content_mask.bounds)
            .unwrap();
        let mask = Bounds::new(
            point(px(mask.origin.x.0 / scale), px(mask.origin.y.0 / scale)),
            size(
                px(mask.size.width.0 / scale),
                px(mask.size.height.0 / scale),
            ),
        );
        (line, mask, px(1. / scale))
    });
    let descent = line.descent.abs();
    assert!(
        line.ascent + descent > AUTO_LINE_13,
        "the text {:?} + {descent:?} fits the line, so nothing to show",
        line.ascent
    );
    // Where gpui's paint puts the baseline: half the spare height plus the
    // ascent, as kv.rs's painted_baseline reads it.
    let baseline = area.top() + (area.size.height - line.ascent - line.descent) / 2. + line.ascent;
    let (top, bottom) = (baseline - line.ascent, baseline + descent);
    assert!(
        mask.top() <= top + px(0.01) && mask.bottom() >= bottom - px(0.01),
        "the mask {mask:?} cuts the glyphs from {top:?} to {bottom:?}"
    );
    // C273: and it grows no further than the glyphs overflow the line,
    // give or take the device pixel a content mask snaps out to.
    assert!(
        mask.top() >= top - device_px
            && mask.bottom() <= bottom + device_px
            && mask.left() >= area.left() - device_px
            && mask.right() <= area.right() + device_px,
        "the mask {mask:?} reaches past the input {area:?} and the glyphs from {top:?} to {bottom:?}"
    );
}

/// Eight lines with descenders, for a three-line input to scroll through.
#[cfg(target_os = "macos")]
const EIGHT_LINES: &str = "gjpqy one\ngjpqy two\ngjpqy three\ngjpqy four\ngjpqy five\ngjpqy six\ngjpqy seven\ngjpqy eight";

/// Runs `check` on a focused three-line input on the composer's line at a
/// `rem` px UI font, under CoreText, holding nothing yet.
#[cfg(target_os = "macos")]
fn with_three_line_input(
    rem: f32,
    check: impl FnOnce(&gpui::Entity<TextInput>, &mut gpui::VisualTestContext),
) {
    use crate::ui::tokens::AUTO_LINE_13;
    let mut app = crate::ui::test_support::core_text_app();
    crate::ui::test_support::init(&mut app);
    app.update(|cx| {
        crate::assets::load_fonts(cx).unwrap();
        bind_keys(cx);
    });
    let (host, cx) = app.add_window_view(move |_, cx| {
        UiFontHost(
            cx.new(|cx| TextInput::new(cx).line_height(AUTO_LINE_13).multiline(3)),
            px(rem),
        )
    });
    let input = host.read_with(cx, |host, _| host.0.clone());
    input.update_in(cx, |input, window, cx| {
        input.focus_handle(cx).focus(window, cx);
    });
    cx.run_until_parked();
    check(&input, cx);
}

/// The parts of painted quads that show outside `area`, in window pixels.
#[cfg(target_os = "macos")]
fn quads_outside(cx: &mut gpui::VisualTestContext, area: Bounds<Pixels>) -> Vec<String> {
    cx.update(|window, _| {
        let scale = window.scale_factor();
        window
            .painted_quads()
            .into_iter()
            .filter_map(|quad| {
                let (bounds, mask) = (quad.bounds, quad.content_mask.bounds);
                let top = bounds.origin.y.0.max(mask.origin.y.0) / scale;
                let bottom = (bounds.origin.y.0 + bounds.size.height.0)
                    .min(mask.origin.y.0 + mask.size.height.0)
                    / scale;
                (bottom > top
                    && (top < f32::from(area.top()) - 0.01
                        || bottom > f32::from(area.bottom()) + 0.01))
                    .then(|| format!("{top:.2} to {bottom:.2}"))
            })
            .collect()
    })
}

#[cfg(target_os = "macos")]
#[test]
fn a_scrolled_input_paints_no_selection_past_its_edges() {
    // C273: the mask grown for C264a let the selection of lines scrolled
    // out of view show beyond the input's top and bottom.
    for rem in [15., 20.] {
        for (keys, edge) in [
            // Select all: the input scrolls to the end, lines hide above.
            ("cmd-a", "top"),
            // From line 5 select up to line 2: line 5 hides below.
            ("up up up shift-up shift-up shift-up", "bottom"),
        ] {
            with_three_line_input(rem, |input, cx| {
                input.update_in(cx, |input, window, cx| {
                    input.set_text(EIGHT_LINES, window, cx);
                });
                crate::ui::test_support::redraw(cx);
                cx.simulate_keystrokes(keys);
                crate::ui::test_support::redraw(cx);
                let (area, scroll) =
                    input.read_with(cx, |input, _| (input.last_bounds.unwrap(), input.scroll));
                assert!(scroll.y > px(0.), "{keys}: the input scrolled");
                let outside = quads_outside(cx, area);
                assert!(
                    outside.is_empty(),
                    "at a {rem} px rem, after {keys}, a selection shows past the {edge} of {area:?}: {outside:?}"
                );
            });
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_scrolled_input_paints_only_the_lines_in_view() {
    // C273: a line scrolled out of view painted its glyphs into the band
    // the C264a mask adds. The text is marked as an input method would
    // mark it, so every painted line draws an underline the scene keeps.
    for rem in [15., 20.] {
        with_three_line_input(rem, |input, cx| {
            // The cursor on line 5 scrolls lines 1 and 2 out above and
            // lines 6 to 8 out below.
            let line_5 = EIGHT_LINES.match_indices('\n').nth(3).unwrap().0 + 1;
            input.update_in(cx, |input, window, cx| {
                input.replace_and_mark_text_in_range(
                    None,
                    EIGHT_LINES,
                    Some(line_5..line_5),
                    window,
                    cx,
                );
            });
            crate::ui::test_support::redraw(cx);
            let (scroll, visual_lines) =
                input.read_with(cx, |input, _| (input.scroll, input.visual_lines));
            assert_eq!(visual_lines, 8);
            assert_eq!(
                scroll.y,
                crate::ui::tokens::AUTO_LINE_13 * 2.,
                "lines 1 and 2 are above"
            );
            let underlines = cx.update(|window, _| window.painted_underlines().len());
            assert_eq!(
                underlines, 3,
                "at a {rem} px rem only lines 3 to 5 paint, not the lines scrolled out"
            );
        });
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_line_cut_by_the_edges_paints_nothing_past_them() {
    // C273: one long line wraps to more rows than the input shows. With
    // the cursor in the middle, rows hide above and below, and the mask
    // the line and the caret paint under stays at the input's edges.
    with_three_line_input(20., |input, cx| {
        input.update_in(cx, |input, window, cx| {
            input.set_text(&"gjpqy ".repeat(60), window, cx);
        });
        crate::ui::test_support::redraw(cx);
        cx.simulate_keystrokes("up up up");
        crate::ui::test_support::redraw(cx);
        let (area, caret, scroll, rows) = input.read_with(cx, |input, _| {
            (
                input.last_bounds.unwrap(),
                input.last_cursor.unwrap(),
                input.scroll,
                input.visual_lines,
            )
        });
        let row = crate::ui::tokens::AUTO_LINE_13;
        assert!(scroll.y > px(0.), "rows hide above");
        assert!(
            scroll.y + area.size.height < row * rows as f32,
            "rows hide below"
        );
        let (mask, device_px) = cx.update(|window, _| {
            let scale = window.scale_factor();
            let mask = window
                .painted_quads()
                .into_iter()
                .find(|quad| (quad.bounds.origin.y.0 / scale - f32::from(caret.top())).abs() < 0.5)
                .map(|quad| quad.content_mask.bounds)
                .unwrap();
            (
                Bounds::new(
                    point(px(mask.origin.x.0 / scale), px(mask.origin.y.0 / scale)),
                    size(
                        px(mask.size.width.0 / scale),
                        px(mask.size.height.0 / scale),
                    ),
                ),
                px(1. / scale),
            )
        });
        assert!(
            mask.top() >= area.top() - device_px && mask.bottom() <= area.bottom() + device_px,
            "the cut line paints under {mask:?}, past the input {area:?}"
        );
    });
}
