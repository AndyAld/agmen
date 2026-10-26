//! Label and value rows under an uppercase heading, as in the right dock.

use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
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
        let value = Label::new(self.value).color(self.color).wrap();
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
            .child(value.code())
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
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[test]
    fn section_title_is_uppercase() {
        assert_eq!(KvSection::new("Runs").title.as_ref(), "RUNS");
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
