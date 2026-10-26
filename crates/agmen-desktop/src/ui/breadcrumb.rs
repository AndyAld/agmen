//! The bar above a center tab's content: path parts split by "›", the
//! last one bright, with buttons at the right edge.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::components::{Color, Label, LabelSize};
use super::tokens::{BORDER_WIDTH, BREADCRUMB_HEIGHT, SPACE_8, SPACE_14};

/// Tests find it as `breadcrumb`, and the separator before part `ix` as
/// `"breadcrumb:sep:{ix}"`.
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
        let mut row = div().flex().items_center().gap(SPACE_8);
        for (ix, part) in self.parts.into_iter().enumerate() {
            if ix > 0 {
                let selector = format!("breadcrumb:sep:{ix}");
                row = row.child(
                    div()
                        .debug_selector(|| selector)
                        .child(Label::new("›").size(LabelSize::Small).color(Color::Muted)),
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

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext};

    use super::*;
    use crate::ui::test_support::{init, redraw};
    use crate::ui::tokens::ICON_SM;

    struct Host;

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            Breadcrumb::new(["andy/fx1".into(), "C015".into(), "main".into()])
        }
    }

    #[gpui::test]
    fn separators_are_a_text_glyph(cx: &mut TestAppContext) {
        // V80: console.py's breadcrumb splits parts with a 12 px "›" in
        // icon.muted (nodes 609, 11630), not a 12 px chevron icon.
        init(cx);
        let (_, cx) = cx.add_window_view(|_, _| Host);
        redraw(cx);
        for ix in [1, 2] {
            let sel: &'static str = Box::leak(format!("breadcrumb:sep:{ix}").into_boxed_str());
            let sep = cx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel}"));
            assert!(sep.size.width < ICON_SM, "{sel}: {sep:?}");
        }
        assert!(cx.debug_bounds("breadcrumb:sep:3").is_none());
    }
}
