//! The title and hint shared by Settings rows and About notices.

use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window, div,
};

use super::{Color, Label, LabelSize, WordWrap};
use crate::ui::tokens::{AUTO_LINE_11, SPACE_3};

#[derive(Debug, IntoElement)]
pub(crate) struct SettingText {
    title: &'static str,
    hint: &'static str,
    preserve_lines: bool,
}

impl SettingText {
    #[must_use]
    pub(crate) const fn new(title: &'static str, hint: &'static str) -> Self {
        Self {
            title,
            hint,
            preserve_lines: false,
        }
    }

    /// Keeps the paragraphs in a license notice.
    #[must_use]
    pub(crate) const fn preserve_lines(mut self) -> Self {
        self.preserve_lines = true;
        self
    }
}

impl RenderOnce for SettingText {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let selector = format!("setting-text:{}", self.title);
        let hint = if self.preserve_lines {
            Label::new(self.hint)
                .size(LabelSize::Small)
                .line_height(AUTO_LINE_11)
                .color(Color::Placeholder)
                .wrap()
                .into_any_element()
        } else {
            // Break only at spaces, keeping the Theme hint's path whole.
            WordWrap::new(format!("setting-hint:{}", self.title), self.hint)
                .size(LabelSize::Small)
                .line_height(AUTO_LINE_11)
                .color(Color::Placeholder)
                .w_full()
                .into_any_element()
        };
        div()
            .debug_selector(|| selector)
            .w_full()
            .flex()
            .flex_col()
            .gap(SPACE_3)
            .child(
                Label::new(self.title)
                    .weight(FontWeight::MEDIUM)
                    .color(Color::TextMuted),
            )
            .child(hint)
    }
}
