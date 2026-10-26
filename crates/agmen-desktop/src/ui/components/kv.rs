//! Label and value rows under an uppercase heading, as in the right dock.

use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, RenderOnce,
    SharedString, Styled as _, Window, div, px,
};

use super::{Color, Label, LabelSize, SectionHeading};
use crate::appearance::{Appearance, Fonts};
use crate::ui::code_view::cell_width;
use crate::ui::tokens::{AUTO_LINE_12, KV_LABEL_WIDTH, SPACE_7, SPACE_10, TEXT_SMALL};

/// A label in a fixed column and a wrapping value in the code font. Tests
/// find word `i` of the value as `"kv-word:{label}:{row}:{i}"`, where `row`
/// is the row's place in its [`KvSection`] (0 for a row on its own), so two
/// rows with equal labels keep apart.
#[derive(Debug, IntoElement)]
pub(crate) struct KvRow {
    label: SharedString,
    value: SharedString,
    color: Color,
    row: usize,
}

impl KvRow {
    #[must_use]
    pub(crate) fn new(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            color: Color::Default,
            row: 0,
        }
    }

    #[must_use]
    pub(crate) fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

impl RenderOnce for KvRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // GPUI's wrapper also breaks before '/', which splits a handle such
        // as "andy/fx1" (V77). Each word is its own flex item, so lines
        // break only at spaces; a word wider than the column still wraps.
        // The space is a one cell gap, which flex leaves out at a line's
        // end, so a value that fits exactly stays on one line (C219).
        let fonts = &cx.global::<Appearance>().fonts;
        let space = cell_width(window, fonts);
        // Both columns draw 14 px lines, as the mockup's 12 px text boxes
        // (nodes 397 and 398), and the higher baseline moves down to the
        // other, so label and value share one at any font.
        let shift = baseline_shift(window, fonts);
        let label = self.label.clone();
        let label_selector = format!("kv-label:{label}:{}", self.row);
        // Runs of spaces and edge spaces draw no empty words; tabs and
        // newlines also split, since a value is one line of text (C240).
        let words = self.value.split_whitespace().enumerate().map(|(i, word)| {
            let selector = format!("kv-word:{label}:{}:{i}", self.row);
            div().debug_selector(|| selector).max_w_full().child(
                Label::new(word.to_owned())
                    .color(self.color.clone())
                    .code()
                    .line_height(AUTO_LINE_12)
                    .wrap(),
            )
        });
        let value = div()
            .mt((-shift).max(px(0.)))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_wrap()
            .gap_x(space)
            .children(words);
        div()
            .w_full()
            .flex()
            .items_start()
            .gap(SPACE_10)
            .child(
                div()
                    .debug_selector(|| label_selector)
                    .mt(shift.max(px(0.)))
                    .w(KV_LABEL_WIDTH)
                    .flex_none()
                    .child(
                        Label::new(self.label)
                            .size(LabelSize::Small)
                            .line_height(AUTO_LINE_12)
                            .color(Color::Placeholder),
                    ),
            )
            .child(value)
    }
}

/// How far the value's first baseline sits below the label's, both in
/// [`AUTO_LINE_12`] lines: the label is the UI font at Small and the value
/// the code font at its own size. Negative when the label's is lower.
fn baseline_shift(window: &Window, fonts: &Fonts) -> Pixels {
    let text = window.text_system();
    let ui = text.resolve_font(&window.text_style().font());
    let code = text.resolve_font(&gpui::font(fonts.code_family.clone()));
    let small = TEXT_SMALL.to_pixels(window.rem_size());
    text.baseline_offset(code, fonts.code_size, AUTO_LINE_12)
        - text.baseline_offset(ui, small, AUTO_LINE_12)
}

/// A heading and its rows.
#[derive(Debug, IntoElement)]
pub(crate) struct KvSection {
    heading: SectionHeading,
    rows: Vec<KvRow>,
}

impl KvSection {
    /// `title` is shown uppercased.
    #[must_use]
    pub(crate) fn new(title: &str) -> Self {
        Self {
            heading: SectionHeading::new(title),
            rows: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) fn row(mut self, mut row: KvRow) -> Self {
        row.row = self.rows.len();
        self.rows.push(row);
        self
    }
}

impl RenderOnce for KvSection {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(SPACE_7)
            .child(self.heading)
            .children(self.rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{draw, rendered_size};
    use crate::ui::tokens::{RIGHT_DOCK_WIDTH, SPACE_14};
    use gpui::{TestAppContext, px};

    #[test]
    fn section_title_is_uppercase() {
        assert_eq!(KvSection::new("Runs").heading.title(), "RUNS");
    }

    #[gpui::test]
    fn kv_values_wrap_between_words(cx: &mut TestAppContext) {
        // V77: frame 6674 breaks "waiting for Apply: andy/fx1 lanes and
        // budget" at a space, never inside the handle.
        let cx = draw(cx, || {
            div()
                .w(RIGHT_DOCK_WIDTH - SPACE_14 * 2.)
                .child(KvRow::new(
                    "Recent",
                    "waiting for Apply: andy/fx1 lanes and budget",
                ))
                .into_any_element()
        });
        let mut word = |i: usize| {
            cx.debug_bounds(Box::leak(format!("kv-word:Recent:0:{i}").into_boxed_str()))
                .unwrap_or_else(|| panic!("word {i} is not drawn on its own"))
        };
        let (first, handle, last) = (word(0), word(3), word(6));
        assert_eq!(
            handle.size.height, first.size.height,
            "andy/fx1 stays on one line"
        );
        assert_eq!(handle.origin.x, first.origin.x, "andy/fx1 moves down whole");
        let second = word(1);
        // C219: the space between words is a gap, one code cell wide: the
        // code font is monospaced, "waiting" is 7 cells and "for" 3.
        let cell = first.size.width / 7.;
        assert!(
            (second.size.width - cell * 3.).abs() < px(0.01),
            "{first:?} {second:?}"
        );
        assert!(
            (second.origin.x - (first.right() + cell)).abs() < px(0.01),
            "{first:?} {second:?}"
        );
        assert!(last.origin.y > first.origin.y, "the value still wraps");
    }

    #[gpui::test]
    fn kv_value_that_fits_exactly_stays_on_one_line(cx: &mut TestAppContext) {
        // C219: "aaaa bbbb" is 9 cells; a trailing space after "bbbb" must
        // not push it to the next line when "cc" follows.
        crate::ui::test_support::init(cx);
        let cell = cx.update(|cx| {
            let fonts = &cx.global::<crate::appearance::Appearance>().fonts;
            let text = cx.text_system();
            let font = text.resolve_font(&gpui::font(fonts.code_family.clone()));
            text.advance(font, fonts.code_size, 'a').unwrap().width
        });
        let width = KV_LABEL_WIDTH + SPACE_10 + cell * 9. + px(0.5);
        let cx = draw(cx, move || {
            div()
                .w(width)
                .child(KvRow::new("Recent", "aaaa bbbb cc"))
                .into_any_element()
        });
        let mut word = |i: usize| {
            cx.debug_bounds(Box::leak(format!("kv-word:Recent:0:{i}").into_boxed_str()))
                .unwrap_or_else(|| panic!("word {i}"))
        };
        assert_eq!(word(1).origin.y, word(0).origin.y, "bbbb fits after aaaa");
        assert!(word(2).origin.y > word(0).origin.y, "cc wraps");
    }

    #[gpui::test]
    fn kv_rows_with_equal_labels_keep_their_own_words(cx: &mut TestAppContext) {
        // C219: two Recent rows at the same time get their own selectors.
        let cx = draw(cx, || {
            KvSection::new("Recent")
                .row(KvRow::new("10:42", "first"))
                .row(KvRow::new("10:42", "second"))
                .into_any_element()
        });
        let first = cx.debug_bounds("kv-word:10:42:0:0").unwrap();
        let second = cx.debug_bounds("kv-word:10:42:1:0").unwrap();
        assert!(second.origin.y > first.origin.y, "{first:?} {second:?}");
    }

    #[gpui::test]
    fn kv_edge_spaces_draw_no_empty_words(cx: &mut TestAppContext) {
        // agmen-db: a leading or trailing space made an empty word and a gap.
        let cx = draw(cx, || {
            KvSection::new("Runs")
                .row(KvRow::new("State", " working "))
                .into_any_element()
        });
        let first = cx.debug_bounds("kv-word:State:0:0").unwrap();
        assert!(first.size.width > px(0.), "{first:?}");
        assert!(cx.debug_bounds("kv-word:State:0:1").is_none());
    }

    /// Where the text inside `bounds` puts its first baseline, for a line
    /// of `font` at `size` as tall as `line`.
    fn baseline(
        cx: &mut gpui::VisualTestContext,
        bounds: gpui::Bounds<gpui::Pixels>,
        font: &gpui::Font,
        size: gpui::Pixels,
        line: gpui::Pixels,
    ) -> gpui::Pixels {
        let text = cx.update(|window, _| window.text_system().clone());
        bounds.top() + text.baseline_offset(text.resolve_font(font), size, line)
    }

    /// The label's and the first value word's baselines, and the word
    /// bounds, for `value` in the right dock's column.
    fn kv_baselines(
        cx: &mut TestAppContext,
        value: &'static str,
    ) -> (gpui::Pixels, gpui::Pixels, Vec<gpui::Bounds<gpui::Pixels>>) {
        let cx = draw(cx, move || {
            div()
                .w(RIGHT_DOCK_WIDTH - SPACE_14 * 2.)
                .child(KvRow::new("Harness", value))
                .into_any_element()
        });
        let label = cx.debug_bounds("kv-label:Harness:0").unwrap();
        let words: Vec<_> = (0..)
            .map_while(|i| {
                cx.debug_bounds(Box::leak(format!("kv-word:Harness:0:{i}").into_boxed_str()))
            })
            .collect();
        let (ui, rem) = cx.update(|window, _| (window.text_style().font(), window.rem_size()));
        let fonts = cx.update(|_, cx| cx.global::<crate::appearance::Appearance>().fonts.clone());
        let code = gpui::font(fonts.code_family.clone());
        let label_line = label.size.height;
        let first = words[0];
        let label_at = baseline(cx, label, &ui, TEXT_SMALL.to_pixels(rem), label_line);
        let value_at = baseline(cx, first, &code, fonts.code_size, first.size.height);
        (label_at, value_at, words)
    }

    #[gpui::test]
    fn kv_label_and_value_share_a_baseline(cx: &mut TestAppContext) {
        // Nodes 397 and 398: "Harness" (UI 12) and its value (mono 12) end
        // on one baseline. The label is Small and the value the code font's
        // 13 px, so with each at 1.2 times its size the label sat 2 px high.
        let (label, value, words) = kv_baselines(cx, "Codex 0.164.0 over ACP");
        // gpui rounds layout edges to device pixels.
        assert!((label - value).abs() < px(0.1), "{label:?} {value:?}");
        assert_eq!(
            words[0].size.height, AUTO_LINE_12,
            "the mockup's 14 px line"
        );
    }

    #[gpui::test]
    fn a_wrapped_kv_value_keeps_the_baseline_on_its_first_line(cx: &mut TestAppContext) {
        let (label, value, words) = kv_baselines(
            cx,
            "waiting for Apply: andy/fx1 lanes and budget, then the review and the merge",
        );
        // gpui rounds layout edges to device pixels.
        assert!((label - value).abs() < px(0.1), "{label:?} {value:?}");
        let last = words.last().unwrap();
        assert!(last.top() > words[0].top(), "the value wraps");
        // Wrapped lines advance by the mockup's 14 px.
        let pitch = last.top() - words[0].top();
        assert!(
            (pitch / AUTO_LINE_12).fract().abs() < 0.001,
            "{pitch:?} is not whole 14 px lines"
        );
    }

    #[gpui::test]
    fn kv_section_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            KvSection::new("Runs")
                .row(KvRow::new("Harness", "Codex 0.164.0 over ACP"))
                .row(KvRow::new("Never", "Edit Write Bash").color(Color::Error))
                .into_any_element()
        });
        assert!(size.width > KV_LABEL_WIDTH && size.height > px(20.));
    }
}
