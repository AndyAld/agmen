use super::*;
use crate::ui::test_support::rendered_size;
use gpui::TestAppContext;

#[gpui::test]
fn label_renders(cx: &mut TestAppContext) {
    let size = rendered_size(cx, || {
        Label::new("agmen")
            .size(LabelSize::Small)
            .color(Color::Muted)
            .weight(FontWeight::SEMIBOLD)
            .into_any_element()
    });
    assert!(size.width > gpui::px(0.) && size.height > gpui::px(0.));
}

#[gpui::test]
fn large_label_is_taller(cx: &mut TestAppContext) {
    let small = rendered_size(cx, || {
        Label::new("A").size(LabelSize::XSmall).into_any_element()
    });
    let mut cx2 = TestAppContext::single();
    let large = rendered_size(&mut cx2, || {
        Label::new("A").size(LabelSize::Large).into_any_element()
    });
    assert!(large.height > small.height);
}

#[gpui::test]
fn strikethrough_keeps_the_label_size(cx: &mut TestAppContext) {
    assert_ne!(Label::new("$5.00").strikethrough(), Label::new("$5.00"));
    let plain = rendered_size(cx, || Label::new("$5.00").code().into_any_element());
    let mut cx2 = TestAppContext::single();
    let struck = rendered_size(&mut cx2, || {
        Label::new("$5.00")
            .code()
            .strikethrough()
            .into_any_element()
    });
    assert_eq!(struck, plain);
}

#[gpui::test]
fn truncated_label_stays_on_one_line_in_its_row(cx: &mut TestAppContext) {
    use gpui::InteractiveElement as _;
    let long = "adam/coord: CI on main is red after #10 merged; the release tag is blocked.";
    let one_line = rendered_size(cx, || Label::new("adam").into_any_element());
    let mut cx2 = TestAppContext::single();
    let cx2 = crate::ui::test_support::draw(&mut cx2, move || {
        div()
            .debug_selector(|| "row".into())
            .w(gpui::px(100.))
            .flex()
            .child(Label::new(long).truncate())
            .child(
                div()
                    .debug_selector(|| "after".into())
                    .w(gpui::px(1.))
                    .h(gpui::px(1.)),
            )
            .into_any_element()
    });
    let row = cx2.debug_bounds("row").unwrap();
    let after = cx2.debug_bounds("after").unwrap();
    assert!(after.right() <= row.right(), "{after:?} past {row:?}");
    assert_eq!(row.size.height, one_line.height);
}

#[gpui::test]
fn wrapped_label_in_a_column_is_as_tall_as_its_text(cx: &mut TestAppContext) {
    // The plan panel's goal, note and lanes (nodes 8944, 9010, 9029) hug
    // their lines. flex_1 grew them into a definite column's free height.
    use gpui::InteractiveElement as _;
    let one_line = rendered_size(cx, || Label::new("agmen").into_any_element());
    let mut cx2 = TestAppContext::single();
    let cx2 = crate::ui::test_support::draw(&mut cx2, || {
        div()
            .debug_selector(|| "column".into())
            .w(gpui::px(200.))
            .h(gpui::px(300.))
            .flex()
            .flex_col()
            .child(Label::new("agmen").wrap())
            .child(
                div()
                    .debug_selector(|| "after".into())
                    .w(gpui::px(1.))
                    .h(gpui::px(1.)),
            )
            .into_any_element()
    });
    let column = cx2.debug_bounds("column").unwrap();
    let after = cx2.debug_bounds("after").unwrap();
    assert_eq!(after.top() - column.top(), one_line.height);
}

#[gpui::test]
fn wrapped_label_in_a_row_takes_the_rest_of_it_and_wraps(cx: &mut TestAppContext) {
    use gpui::InteractiveElement as _;
    let long = "adam/coord: CI on main is red after #10 merged; the release tag is blocked.";
    let one_line = rendered_size(cx, || Label::new("adam").into_any_element());
    let mut cx2 = TestAppContext::single();
    let cx2 = crate::ui::test_support::draw(&mut cx2, move || {
        div()
            .debug_selector(|| "row".into())
            .w(gpui::px(100.))
            .flex()
            .child(div().w(gpui::px(20.)).h(gpui::px(1.)).flex_none())
            .child(Label::new(long).wrap())
            .child(
                div()
                    .debug_selector(|| "after".into())
                    .w(gpui::px(1.))
                    .h(gpui::px(1.))
                    .flex_none(),
            )
            .into_any_element()
    });
    let row = cx2.debug_bounds("row").unwrap();
    let after = cx2.debug_bounds("after").unwrap();
    assert_eq!(after.right(), row.right(), "the label fills the row");
    assert!(row.size.height > one_line.height * 2., "{row:?} wraps");
}

#[test]
fn underline_is_a_distinct_style() {
    assert_ne!(Label::new("#9").underline(), Label::new("#9"));
}

#[gpui::test]
fn code_label_takes_a_size_from_the_ui_scale(cx: &mut TestAppContext) {
    // V93: the Git panel's mono 11 metadata is the code font at Small;
    // a code label at the default size keeps the code font's own size.
    const TEXT: &str = "+2 \u{2212}5";
    let code = rendered_size(cx, || Label::new(TEXT).code().into_any_element());
    let mut cx2 = TestAppContext::single();
    let small = rendered_size(&mut cx2, || {
        Label::new(TEXT)
            .code()
            .size(LabelSize::Small)
            .into_any_element()
    });
    let mut cx3 = TestAppContext::single();
    let ui = rendered_size(&mut cx3, || {
        Label::new(TEXT).size(LabelSize::Small).into_any_element()
    });
    assert!(small.height < code.height, "{small:?} {code:?}");
    // The test text system measures every font alike, so only the
    // size is checked here.
    assert_eq!(small.height, ui.height, "the same text size");
}

#[gpui::test]
fn code_label_at_an_explicit_default_size_takes_text_default(cx: &mut TestAppContext) {
    // C252: only a code label with no size set takes the code font's
    // own size; Default asked for is TEXT_DEFAULT, as for UI text.
    let asked = rendered_size(cx, || {
        Label::new("andy/fx1")
            .code()
            .size(LabelSize::Default)
            .into_any_element()
    });
    let mut cx2 = TestAppContext::single();
    let unset = rendered_size(&mut cx2, || {
        Label::new("andy/fx1").code().into_any_element()
    });
    let want = TEXT_DEFAULT.to_pixels(crate::ui::tokens::MOCKUP_REM) * LINE_HEIGHT;
    // gpui rounds layout edges to device pixels (2 per px here).
    assert!((asked.height - want).abs() <= gpui::px(0.25), "{asked:?}");
    assert_ne!(asked.height, unset.height, "{asked:?} {unset:?}");
}

#[gpui::test]
fn code_label_renders(cx: &mut TestAppContext) {
    let size = rendered_size(cx, || Label::new("andy/fx1").code().into_any_element());
    assert!(size.width > gpui::px(0.));
}

/// A truncated label holding [`TALL_AND_DEEP`](crate::ui::test_support::TALL_AND_DEEP)
/// at a `rem` px UI font, in the UI family as the workspace sets it.
#[cfg(target_os = "macos")]
struct InkHost {
    rem: gpui::Pixels,
    label: fn() -> Label,
}

#[cfg(target_os = "macos")]
impl gpui::Render for InkHost {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        use gpui::InteractiveElement as _;
        window.set_rem_size(self.rem);
        let family = cx.global::<Appearance>().fonts.ui_family.clone();
        div().p(gpui::px(40.)).font_family(family).child(
            div()
                .debug_selector(|| "ink-row".into())
                .w(gpui::px(300.))
                .flex()
                // The strikethrough paints under the glyphs' mask.
                .child((self.label)().strikethrough()),
        )
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_truncated_label_shows_its_tallest_and_deepest_glyphs() {
    // C287: gpui's truncate clips to the label's box. The composer's chips
    // set the code font on a 14 px line, so the box cut the descenders by
    // 0.6 px at a 13 px code font and Ë's dots by 3.4 px and the
    // descenders by 4.7 px at 20 px.
    use crate::ui::test_support::{Ink, TALL_AND_DEEP, core_text_app, init, redraw, unscaled};
    use crate::ui::tokens::AUTO_LINE_12;
    // What, the UI and code font sizes, whether the label is code, and
    // the label.
    type Case = (&'static str, f32, f32, bool, fn() -> Label);
    let cases: [Case; 6] = [13., 16., 20.]
        .map(|size| -> [Case; 2] {
            [
                ("code on its 14 px line", 15., size, true, || {
                    Label::new(TALL_AND_DEEP)
                        .code()
                        .line_height(AUTO_LINE_12)
                        .truncate()
                }),
                ("UI on its 1.2 line", size, 15., false, || {
                    Label::new(TALL_AND_DEEP).truncate()
                }),
            ]
        })
        .concat()
        .try_into()
        .unwrap();
    for (what, rem, code, is_code, label) in cases {
        let at = format!("{what}, UI {rem} px, code {code} px");
        let mut app = core_text_app();
        init(&mut app);
        app.update(|cx| {
            crate::assets::load_fonts(cx).unwrap();
            let mut appearance = cx.global::<Appearance>().clone();
            appearance.fonts.code_size = gpui::px(code);
            cx.set_global(appearance);
        });
        let (_, cx) = app.add_window_view(move |_, _| InkHost {
            rem: gpui::px(rem),
            label,
        });
        redraw(cx);
        let row = cx.debug_bounds("ink-row").unwrap();
        let (mask, ink, device_px) = cx.update(|window, cx| {
            let fonts = cx.global::<Appearance>().fonts.clone();
            let (family, size) = if is_code {
                (fonts.code_family, fonts.code_size)
            } else {
                (fonts.ui_family, TEXT_DEFAULT.to_pixels(window.rem_size()))
            };
            let scale = window.scale_factor();
            let [struck] = window.painted_underlines()[..] else {
                panic!("{at}: one strikethrough");
            };
            let ink = Ink::of(
                window,
                TALL_AND_DEEP,
                &gpui::font(family),
                size,
                row.top(),
                row.size.height,
            );
            (
                unscaled(struck.content_mask.bounds, scale),
                ink,
                gpui::px(1. / scale),
            )
        });
        // A content mask snaps to device pixels.
        let near = gpui::px(0.01);
        // C299: the font's whole ascent and descent, not just this text's
        // glyphs.
        assert!(
            mask.top() <= ink.font.0 + near && mask.bottom() >= ink.font.1 - near,
            "{at}: the mask {mask:?} cuts the font from {:?} to {:?}",
            ink.font.0,
            ink.font.1
        );
        // As C264a and C273 grow TextInput's: by the font's overflow of the
        // line and no further, and not at all sideways.
        assert!(
            mask.top() >= row.top().min(ink.font.0) - device_px
                && mask.bottom() <= row.bottom().max(ink.font.1) + device_px
                && mask.left() >= row.left() - device_px
                && mask.right() <= row.right() + device_px,
            "{at}: the mask {mask:?} reaches past the label {row:?} and the font from {:?} to {:?}",
            ink.font.0,
            ink.font.1
        );
    }
}
