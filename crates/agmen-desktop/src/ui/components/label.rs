//! A run of text in one of Zed's text sizes and a theme color.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div, relative,
};

use super::Color;
use crate::appearance::Appearance;
use crate::ui::tokens::{LINE_HEIGHT, TEXT_DEFAULT, TEXT_LARGE, TEXT_SMALL, TEXT_XSMALL};

/// Zed's UI text sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LabelSize {
    XSmall,
    Small,
    #[default]
    Default,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "headings in later tasks of the desktop plan use it"
        )
    )]
    Large,
}

/// One line of text, or a wrapping block with [`Label::wrap`].
#[derive(Debug, PartialEq, IntoElement)]
pub(crate) struct Label {
    text: SharedString,
    size: LabelSize,
    color: Color,
    weight: Option<FontWeight>,
    code: bool,
    wrap: bool,
    strikethrough: bool,
    underline: bool,
}

impl Label {
    #[must_use]
    pub(crate) fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            size: LabelSize::Default,
            color: Color::Default,
            weight: None,
            code: false,
            wrap: false,
            strikethrough: false,
            underline: false,
        }
    }

    #[must_use]
    pub(crate) const fn size(mut self, size: LabelSize) -> Self {
        self.size = size;
        self
    }

    #[must_use]
    pub(crate) fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    #[must_use]
    pub(crate) const fn weight(mut self, weight: FontWeight) -> Self {
        self.weight = Some(weight);
        self
    }

    /// Sets the text in the code font at the code font's size, for handles,
    /// branches and other values people type.
    #[must_use]
    pub(crate) const fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// Lets the text wrap and shrink to the space left in its row.
    #[must_use]
    pub(crate) const fn wrap(mut self) -> Self {
        self.wrap = true;
        self
    }

    /// Draws a line through the text, as a change card's old value.
    #[must_use]
    pub(crate) const fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }

    /// Underlines the text, as a reference chip with no fill.
    #[must_use]
    pub(crate) const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }
}

impl RenderOnce for Label {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        let color = self.color.hsla(&appearance.theme);
        let base = div().text_color(color).line_height(relative(LINE_HEIGHT));
        let sized = if self.code {
            base.font_family(appearance.fonts.code_family.clone())
                .text_size(appearance.fonts.code_size)
        } else {
            base.text_size(match self.size {
                LabelSize::XSmall => TEXT_XSMALL,
                LabelSize::Small => TEXT_SMALL,
                LabelSize::Default => TEXT_DEFAULT,
                LabelSize::Large => TEXT_LARGE,
            })
        };
        sized
            .when_some(self.weight, gpui::Styled::font_weight)
            .when(self.strikethrough, gpui::Styled::line_through)
            .when(self.underline, gpui::Styled::underline)
            .map(|this| {
                if self.wrap {
                    this.flex_1().min_w_0()
                } else {
                    this.flex_none().whitespace_nowrap()
                }
            })
            .child(self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::TestAppContext;

    #[gpui::test]
    fn label_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            Label::new("agmen")
                .size(LabelSize::Small)
                .color(Color::Muted)
                .weight(FontWeight::SEMIBOLD)
                .into_any_element()
        });
        assert!(size.width > gpui::px(0.) && size.height > gpui::px(0.));
    }

    #[gpui::test]
    fn large_label_is_taller(cx: &mut TestAppContext) {
        let small = rendered_size(cx, || {
            Label::new("A").size(LabelSize::XSmall).into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let large = rendered_size(&mut cx2, || {
            Label::new("A").size(LabelSize::Large).into_any_element()
        });
        assert!(large.height > small.height);
    }

    #[gpui::test]
    fn strikethrough_keeps_the_label_size(cx: &mut TestAppContext) {
        assert_ne!(Label::new("$5.00").strikethrough(), Label::new("$5.00"));
        let plain = rendered_size(cx, || Label::new("$5.00").code().into_any_element());
        let mut cx2 = TestAppContext::single();
        let struck = rendered_size(&mut cx2, || {
            Label::new("$5.00")
                .code()
                .strikethrough()
                .into_any_element()
        });
        assert_eq!(struck, plain);
    }

    #[test]
    fn underline_is_a_distinct_style() {
        assert_ne!(Label::new("#9").underline(), Label::new("#9"));
    }

    #[gpui::test]
    fn code_label_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || Label::new("andy/fx1").code().into_any_element());
        assert!(size.width > gpui::px(0.));
    }
}
