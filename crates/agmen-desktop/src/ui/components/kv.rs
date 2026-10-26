//! Label and value rows under an uppercase heading, as in the right dock.

use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::code_view::cell_width;
use crate::ui::tokens::{KV_LABEL_WIDTH, SPACE_7, SPACE_10};

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
        let space = cell_width(window, &cx.global::<Appearance>().fonts);
        let label = self.label.clone();
        // Runs of spaces and edge spaces draw no empty words; tabs and
        // newlines also split, since a value is one line of text (C240).
        let words = self.value.split_whitespace().enumerate().map(|(i, word)| {
            let selector = format!("kv-word:{label}:{}:{i}", self.row);
            div().debug_selector(|| selector).max_w_full().child(
                Label::new(word.to_owned())
                    .color(self.color.clone())
                    .code()
                    .wrap(),
            )
        });
        let value = div()
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
                div().w(KV_LABEL_WIDTH).flex_none().child(
                    Label::new(self.label)
                        .size(LabelSize::Small)
                        .color(Color::Placeholder),
                ),
            )
            .child(value)
    }
}

/// A heading and its rows.
#[derive(Debug, IntoElement)]
pub(crate) struct KvSection {
    title: SharedString,
    rows: Vec<KvRow>,
}

impl KvSection {
    /// `title` is shown uppercased.
    #[must_use]
    pub(crate) fn new(title: &str) -> Self {
        Self {
            title: title.to_uppercase().into(),
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
            .child(
                Label::new(self.title)
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .weight(FontWeight::SEMIBOLD),
            )
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
        assert_eq!(KvSection::new("Runs").title.as_ref(), "RUNS");
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
