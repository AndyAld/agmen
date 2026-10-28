//! A panel's plain empty state (Task 7): one line of placeholder text, as
//! "No worktrees yet", where a live panel has no data.

use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::ui::tokens::{SPACE_10, SPACE_14};

/// Tests find it as the `selector` it is made with.
#[derive(Debug, IntoElement)]
pub(crate) struct EmptyState {
    selector: &'static str,
    text: SharedString,
}

impl EmptyState {
    #[must_use]
    pub(crate) fn new(selector: &'static str, text: impl Into<SharedString>) -> Self {
        Self {
            selector,
            text: text.into(),
        }
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let selector = self.selector;
        div()
            .debug_selector(|| selector.into())
            .w_full()
            .px(SPACE_14)
            .py(SPACE_10)
            .child(
                Label::new(self.text)
                    .size(LabelSize::Small)
                    .color(Color::Placeholder)
                    .wrap(),
            )
    }
}
