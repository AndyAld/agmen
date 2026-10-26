//! Verb badges (DONE, ALERT, ...) and count pills.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{BADGE_TINT, RADIUS_SM, SPACE_1, SPACE_5, SPACE_7};

#[derive(Debug, PartialEq, IntoElement)]
pub(crate) struct Badge {
    label: SharedString,
    color: Color,
    solid: bool,
    code: bool,
    leading: Option<SharedString>,
}

impl Badge {
    /// A tinted badge in `color`.
    #[must_use]
    pub(crate) fn new(label: impl Into<SharedString>, color: Color) -> Self {
        Self {
            label: label.into(),
            color,
            solid: false,
            code: false,
            leading: None,
        }
    }

    /// Fills the badge with its color and sets the text on it in the
    /// background color.
    #[must_use]
    pub(crate) const fn solid(mut self) -> Self {
        self.solid = true;
        self
    }

    /// Sets the label in the code font at regular weight, as a count pill.
    #[must_use]
    pub(crate) const fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// A bold glyph before the label, as the "!" of "! 1 needs you".
    #[must_use]
    pub(crate) fn leading(mut self, glyph: impl Into<SharedString>) -> Self {
        self.leading = Some(glyph.into());
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let fill = self.color.hsla(theme);
        let (bg, text) = if self.solid {
            (fill, Color::Background)
        } else {
            (fill.opacity(BADGE_TINT), self.color.clone())
        };
        let label = Label::new(self.label)
            .size(LabelSize::Small)
            .color(text.clone());
        let label = if self.code {
            label.code()
        } else {
            label.weight(FontWeight::SEMIBOLD)
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_5)
            .py(SPACE_1)
            .px(if self.solid { SPACE_7 } else { SPACE_5 })
            .rounded(RADIUS_SM)
            .bg(bg)
            .when_some(self.leading, |this, glyph| {
                this.child(
                    Label::new(glyph)
                        .size(LabelSize::Small)
                        .color(text)
                        .weight(FontWeight::BOLD),
                )
            })
            .child(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn badge_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || Badge::new("DONE", Color::Success).into_any_element());
        assert!(size.width > px(10.));
    }

    #[gpui::test]
    fn code_badge_draws_a_small_code_label(cx: &mut TestAppContext) {
        // V94: the Git panel's count pill (node 5113) is mono 11, regular.
        let badge = rendered_size(cx, || {
            Badge::new("14", Color::Placeholder)
                .code()
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let label = rendered_size(&mut cx2, || {
            Label::new("14")
                .code()
                .size(LabelSize::Small)
                .into_any_element()
        });
        assert_eq!(badge.width, label.width + SPACE_5 * 2.);
        assert_eq!(badge.height, label.height + SPACE_1 * 2.);
    }

    #[gpui::test]
    fn solid_badge_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            Badge::new("1 needs you", Color::Error)
                .solid()
                .leading("!")
                .into_any_element()
        });
        assert!(size.width > px(14.));
    }
}
