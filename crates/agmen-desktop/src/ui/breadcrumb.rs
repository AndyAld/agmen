//! The bar above a center tab's content: path parts split by chevrons, the
//! last one bright, with buttons at the right edge.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::components::{Color, Icon, IconSize, Label};
use super::tokens::{BORDER_WIDTH, BREADCRUMB_HEIGHT, SPACE_6, SPACE_14};
use crate::icons::UiIcon;

/// Tests find it as `breadcrumb`.
#[derive(IntoElement)]
pub(crate) struct Breadcrumb {
    parts: Vec<SharedString>,
    end: Option<AnyElement>,
}

impl std::fmt::Debug for Breadcrumb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Breadcrumb")
            .field("parts", &self.parts)
            .finish_non_exhaustive()
    }
}

impl Breadcrumb {
    #[must_use]
    pub(crate) fn new(parts: impl IntoIterator<Item = SharedString>) -> Self {
        Self {
            parts: parts.into_iter().collect(),
            end: None,
        }
    }

    /// Drawn at the bar's right edge.
    #[must_use]
    pub(crate) fn end(mut self, end: impl IntoElement) -> Self {
        self.end = Some(end.into_any_element());
        self
    }
}

impl RenderOnce for Breadcrumb {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let last = self.parts.len().saturating_sub(1);
        let mut row = div().flex().items_center().gap(SPACE_6);
        for (ix, part) in self.parts.into_iter().enumerate() {
            if ix > 0 {
                row = row.child(
                    Icon::new(UiIcon::ChevronRight)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                );
            }
            let color = if ix == last {
                Color::Default
            } else {
                Color::Placeholder
            };
            row = row.child(Label::new(part).code().color(color));
        }
        div()
            .debug_selector(|| "breadcrumb".into())
            .h(BREADCRUMB_HEIGHT)
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .px(SPACE_14)
            .border_b(BORDER_WIDTH)
            .border_color(super::rule_color(cx))
            .child(row)
            .child(div().flex_1())
            .children(self.end)
    }
}
