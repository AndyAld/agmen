//! A 1 px rule in the chrome border color.

use gpui::{App, IntoElement, RenderOnce, Styled as _, Window, div};

use crate::appearance::Appearance;
use crate::ui::tokens::BORDER_WIDTH;

/// Which way the rule runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, IntoElement)]
pub(crate) struct Divider {
    axis: Axis,
}

impl Divider {
    /// A rule across the full width, between stacked regions.
    #[must_use]
    pub(crate) const fn horizontal() -> Self {
        Self {
            axis: Axis::Horizontal,
        }
    }

    /// A rule down the full height, between side by side regions.
    #[must_use]
    pub(crate) const fn vertical() -> Self {
        Self {
            axis: Axis::Vertical,
        }
    }
}

impl RenderOnce for Divider {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let rule = div()
            .flex_none()
            .bg(cx.global::<Appearance>().theme.border_variant);
        match self.axis {
            Axis::Horizontal => rule.h(BORDER_WIDTH).w_full(),
            Axis::Vertical => rule.w(BORDER_WIDTH).h_full(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::TestAppContext;

    #[gpui::test]
    fn horizontal_divider_is_one_px_tall(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || Divider::horizontal().into_any_element());
        assert_eq!(size.height, BORDER_WIDTH);
    }

    #[gpui::test]
    fn vertical_divider_is_one_px_wide(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || Divider::vertical().into_any_element());
        assert_eq!(size.width, BORDER_WIDTH);
    }
}
