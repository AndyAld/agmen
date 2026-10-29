//! A form label, its control, and optional help and validation text.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::ui::tokens::SPACE_4;

/// Keeps the label 4 px above its control; the parent owns field spacing.
#[derive(IntoElement)]
pub(crate) struct Field {
    id: SharedString,
    label: SharedString,
    hint: Option<SharedString>,
    error: Option<SharedString>,
    children: Vec<AnyElement>,
}

impl std::fmt::Debug for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Field")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Field {
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            hint: None,
            error: None,
            children: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "validation text is part of the Field API")
    )]
    #[must_use]
    pub(crate) fn error(mut self, error: impl Into<SharedString>) -> Self {
        self.error = Some(error.into());
        self
    }
}

impl ParentElement for Field {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Field {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let label_selector = format!("{}:label", self.id);
        let hint_selector = format!("{}:hint", self.id);
        let error_selector = format!("{}:error", self.id);
        div()
            .debug_selector(|| self.id.to_string())
            .w_full()
            .flex()
            .flex_col()
            .gap(SPACE_4)
            .child(
                div()
                    .debug_selector(|| label_selector)
                    .child(Label::new(self.label)),
            )
            .children(self.children)
            .children(self.hint.map(|hint| {
                div().debug_selector(|| hint_selector).child(
                    Label::new(hint)
                        .size(LabelSize::Small)
                        .color(Color::Placeholder)
                        .wrap(),
                )
            }))
            .children(self.error.map(|error| {
                div().debug_selector(|| error_selector).child(
                    Label::new(error)
                        .size(LabelSize::Small)
                        .color(Color::Error)
                        .wrap(),
                )
            }))
    }
}
