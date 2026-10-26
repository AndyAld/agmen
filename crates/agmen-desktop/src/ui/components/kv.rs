//! Label and value rows under an uppercase heading, as in the right dock.

use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::ui::tokens::{KV_LABEL_WIDTH, SPACE_7, SPACE_10};

/// A label in a fixed column and a wrapping value in the code font.
#[derive(Debug, IntoElement)]
pub(crate) struct KvRow {
    label: SharedString,
    value: SharedString,
    color: Color,
}

impl KvRow {
    #[must_use]
    pub(crate) fn new(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            color: Color::Default,
        }
    }

    #[must_use]
    pub(crate) fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

impl RenderOnce for KvRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // GPUI's wrapper also breaks before '/', which splits a handle such
        // as "andy/fx1" (V77). Each word is its own flex item, so lines
        // break only at spaces; a word wider than the column still wraps.
        let label = self.label.clone();
        let words = self
            .value
            .split_inclusive(' ')
            .enumerate()
            .map(|(i, word)| {
                let selector = format!("kv-word:{label}:{i}");
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
            .children(words.collect::<Vec<_>>());
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
    pub(crate) fn row(mut self, row: KvRow) -> Self {
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
    use crate::ui::test_support::{init, redraw, rendered_size};
    use crate::ui::tokens::{RIGHT_DOCK_WIDTH, SPACE_14};
    use gpui::{AnyElement, Context, Render, TestAppContext, px};

    struct Wrapper(fn() -> AnyElement);

    impl Render for Wrapper {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            window.set_rem_size(crate::ui::tokens::MOCKUP_REM);
            (self.0)()
        }
    }

    #[test]
    fn section_title_is_uppercase() {
        assert_eq!(KvSection::new("Runs").title.as_ref(), "RUNS");
    }

    #[gpui::test]
    fn kv_values_wrap_between_words(cx: &mut TestAppContext) {
        // V77: frame 6674 breaks "waiting for Apply: andy/fx1 lanes and
        // budget" at a space, never inside the handle.
        init(cx);
        let (_, cx) = cx.add_window_view(|_, _| {
            Wrapper(|| {
                div()
                    .w(RIGHT_DOCK_WIDTH - SPACE_14 * 2.)
                    .child(KvRow::new(
                        "Recent",
                        "waiting for Apply: andy/fx1 lanes and budget",
                    ))
                    .into_any_element()
            })
        });
        redraw(cx);
        let mut word = |i: usize| {
            cx.debug_bounds(Box::leak(format!("kv-word:Recent:{i}").into_boxed_str()))
                .unwrap_or_else(|| panic!("word {i} is not drawn on its own"))
        };
        let (first, handle, last) = (word(0), word(3), word(6));
        assert_eq!(
            handle.size.height, first.size.height,
            "andy/fx1 stays on one line"
        );
        assert_eq!(handle.origin.x, first.origin.x, "andy/fx1 moves down whole");
        let second = word(1);
        // The code font is monospaced: "waiting " is 8 cells and "for " 4,
        // so the space that separates them is drawn.
        assert_eq!(first.size.width, second.size.width * 2.);
        assert_eq!(second.origin.x, first.origin.x + first.size.width);
        assert!(last.origin.y > first.origin.y, "the value still wraps");
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
