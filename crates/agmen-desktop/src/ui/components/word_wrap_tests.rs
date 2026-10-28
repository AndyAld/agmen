use gpui::{AnyElement, Context, Render, Window, div};

use super::*;
use crate::appearance::Appearance;
#[cfg(target_os = "macos")]
use crate::ui::test_support::core_text_app;
use crate::ui::test_support::{init, redraw};

/// Draws `make`'s text inside a parent whose font is `parent_font`, to
/// show the gap does not come from the caller's text style.
struct Host {
    parent_font: SharedString,
    make: fn() -> WordWrap,
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let wrap: AnyElement = (self.make)().into_any_element();
        div()
            .w(gpui::px(2000.))
            .font_family(self.parent_font.clone())
            .debug_selector(|| "host".into())
            .child(wrap)
    }
}

/// Twenty eight-letter words: one line at 2000 px in either font.
#[cfg(target_os = "macos")]
const LETTERS: &str = "aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa \
    aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa \
    aaaaaaaa aaaaaaaa aaaaaaaa aaaaaaaa";

/// The mean gap between [`LETTERS`]'s words as drawn, and one space of the
/// words' font rounded to a whole pixel, as the drawn gaps are.
#[cfg(target_os = "macos")]
fn gap_and_space(
    make: fn() -> WordWrap,
    parent_is_code: bool,
    code_words: bool,
) -> (Pixels, Pixels) {
    let mut app = core_text_app();
    init(&mut app);
    app.update(|cx| crate::assets::load_fonts(cx).unwrap());
    let fonts = app.update(|cx| cx.global::<Appearance>().fonts.clone());
    let parent_font = if parent_is_code {
        fonts.code_family.clone()
    } else {
        fonts.ui_family.clone()
    };
    let (_, cx) = app.add_window_view(move |_, _| Host { parent_font, make });
    redraw(cx);
    let first = cx.debug_bounds("ww:0").unwrap();
    let last = cx.debug_bounds("ww:19").unwrap();
    assert_eq!(first.top(), last.top(), "the words share a line");
    // The words are alike, and the first starts on a whole pixel.
    let letter = first.size.width;
    let space = cx.update(|window, _| {
        let (family, size) = if code_words {
            (fonts.code_family.clone(), fonts.code_size)
        } else {
            (
                fonts.ui_family.clone(),
                LabelSize::Small.rems().to_pixels(window.rem_size()),
            )
        };
        let text = window.text_system();
        let font = text.resolve_font(&gpui::font(family));
        let advance = |c| text.advance(font, size, c).unwrap().width;
        // The words draw in that font too, not the parent's: eight
        // letters are some 4 px wider in the code font at the UI size.
        let word = advance('a') * 8.;
        assert!(
            (letter - word).abs() < gpui::px(1.5),
            "{letter:?} vs {word:?}: the words' own font"
        );
        advance(' ').round()
    });
    let gap = (last.left() - first.left() - letter * 19.) / 19.;
    (gap, space)
}

#[cfg(target_os = "macos")]
#[test]
fn word_gap_is_one_space_of_the_words_own_font() {
    // C321, V139: the gap comes from the Appearance fonts, not from the
    // caller's text style, so a UI hint under a code parent and a code
    // value under a UI parent each gap by their own font's space.
    let (gap, space) = gap_and_space(
        || WordWrap::new("ww", LETTERS).size(LabelSize::Small),
        true,
        false,
    );
    assert!((gap - space).abs() < gpui::px(0.06), "{gap:?} vs {space:?}");
    let (gap, space) = gap_and_space(|| WordWrap::new("ww", LETTERS).code(), false, true);
    assert!((gap - space).abs() < gpui::px(0.06), "{gap:?} vs {space:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn the_fonts_differ_so_the_gap_test_means_something() {
    // The UI and code fonts' spaces differ, so a gap measured from the
    // parent's font would fail the test above.
    let mut app = core_text_app();
    init(&mut app);
    app.update(|cx| crate::assets::load_fonts(cx).unwrap());
    let (_, cx) = app.add_window_view(|_, _| Host {
        parent_font: SharedString::default(),
        make: || WordWrap::new("ww", "a b"),
    });
    let (ui, code) = cx.update(|window, cx| {
        let fonts = cx.global::<Appearance>().fonts.clone();
        let small = LabelSize::Small.rems().to_pixels(window.rem_size());
        let text = window.text_system();
        let space = |family: SharedString, size| {
            text.advance(text.resolve_font(&gpui::font(family)), size, ' ')
                .unwrap()
                .width
        };
        (
            space(fonts.ui_family, small),
            space(fonts.code_family, small),
        )
    });
    assert!((ui - code).abs() > gpui::px(0.5), "{ui:?} vs {code:?}");
}

#[gpui::test]
fn words_are_found_by_their_place(cx: &mut gpui::TestAppContext) {
    // Runs of spaces and edge spaces draw no empty words (C240).
    init(cx);
    let (_, cx) = cx.add_window_view(|_, _| Host {
        parent_font: SharedString::default(),
        make: || WordWrap::new("ww", "  one\ttwo   three "),
    });
    redraw(cx);
    for i in 0..3 {
        let selector: &'static str = Box::leak(format!("ww:{i}").into_boxed_str());
        assert!(cx.debug_bounds(selector).is_some(), "word {i}");
    }
    assert!(cx.debug_bounds("ww:3").is_none());
}
