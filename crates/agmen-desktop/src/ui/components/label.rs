//! A run of text in one of Zed's text sizes and a theme color.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, Rems, RenderOnce, SharedString, Styled as _,
    Window, div, relative,
};

use super::Color;
use crate::appearance::Appearance;
use crate::ui::tokens::{
    LINE_HEIGHT, TEXT_DEFAULT, TEXT_LARGE, TEXT_SMALL, TEXT_TITLE, TEXT_XSMALL,
};

/// Zed's UI text sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LabelSize {
    XSmall,
    Small,
    #[default]
    Default,
    Large,
    /// A page title (V97), above Zed's text sizes.
    Title,
}

impl LabelSize {
    /// The size in rems, from `tokens`.
    #[must_use]
    pub(crate) const fn rems(self) -> Rems {
        match self {
            Self::XSmall => TEXT_XSMALL,
            Self::Small => TEXT_SMALL,
            Self::Default => TEXT_DEFAULT,
            Self::Large => TEXT_LARGE,
            Self::Title => TEXT_TITLE,
        }
    }
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
    truncate: bool,
    strikethrough: bool,
    underline: bool,
    /// Line height as a multiple of the size; `None` is [`LINE_HEIGHT`].
    line_height: Option<f32>,
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
            truncate: false,
            strikethrough: false,
            underline: false,
            line_height: None,
        }
    }

    /// Sets the line height as a multiple of the text size, for text the
    /// mockups draw at a font's own line height rather than 1.2.
    #[must_use]
    pub(crate) const fn line_height(mut self, ratio: f32) -> Self {
        self.line_height = Some(ratio);
        self
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

    /// Sets the text in the code font, for handles, branches and other
    /// values people type. At the default size it takes the code font's own
    /// size; another [`LabelSize`] takes that step of the UI scale, as Zed's
    /// `buffer_font` labels do (the Git panel's mono 11 metadata, V93).
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

    /// Keeps the text on one line, shrinking to the space left in its row
    /// and ending in an ellipsis when it does not fit.
    #[must_use]
    pub(crate) const fn truncate(mut self) -> Self {
        self.truncate = true;
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
        let base = div()
            .text_color(color)
            .line_height(relative(self.line_height.unwrap_or(LINE_HEIGHT)));
        let sized = match (self.code, self.size) {
            (true, LabelSize::Default) => base
                .font_family(appearance.fonts.code_family.clone())
                .text_size(appearance.fonts.code_size),
            (true, _) => base
                .font_family(appearance.fonts.code_family.clone())
                .text_size(self.size.rems()),
            (false, _) => base.text_size(self.size.rems()),
        };
        sized
            .when_some(self.weight, gpui::Styled::font_weight)
            .when(self.strikethrough, gpui::Styled::line_through)
            .when(self.underline, gpui::Styled::underline)
            .map(|this| {
                if self.wrap {
                    this.flex_1().min_w_0()
                } else if self.truncate {
                    this.flex_1().min_w_0().truncate()
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

    #[gpui::test]
    fn truncated_label_stays_on_one_line_in_its_row(cx: &mut TestAppContext) {
        use gpui::InteractiveElement as _;
        let long = "adam/coord: CI on main is red after #10 merged; the release tag is blocked.";
        let one_line = rendered_size(cx, || Label::new("adam").into_any_element());
        let mut cx2 = TestAppContext::single();
        let cx2 = crate::ui::test_support::draw(&mut cx2, move || {
            div()
                .debug_selector(|| "row".into())
                .w(gpui::px(100.))
                .flex()
                .child(Label::new(long).truncate())
                .child(
                    div()
                        .debug_selector(|| "after".into())
                        .w(gpui::px(1.))
                        .h(gpui::px(1.)),
                )
                .into_any_element()
        });
        let row = cx2.debug_bounds("row").unwrap();
        let after = cx2.debug_bounds("after").unwrap();
        assert!(after.right() <= row.right(), "{after:?} past {row:?}");
        assert_eq!(row.size.height, one_line.height);
    }

    #[test]
    fn underline_is_a_distinct_style() {
        assert_ne!(Label::new("#9").underline(), Label::new("#9"));
    }

    #[gpui::test]
    fn code_label_takes_a_size_from_the_ui_scale(cx: &mut TestAppContext) {
        // V93: the Git panel's mono 11 metadata is the code font at Small;
        // a code label at the default size keeps the code font's own size.
        const TEXT: &str = "+2 \u{2212}5";
        let code = rendered_size(cx, || Label::new(TEXT).code().into_any_element());
        let mut cx2 = TestAppContext::single();
        let small = rendered_size(&mut cx2, || {
            Label::new(TEXT)
                .code()
                .size(LabelSize::Small)
                .into_any_element()
        });
        let mut cx3 = TestAppContext::single();
        let ui = rendered_size(&mut cx3, || {
            Label::new(TEXT).size(LabelSize::Small).into_any_element()
        });
        assert!(small.height < code.height, "{small:?} {code:?}");
        // The test text system measures every font alike, so only the
        // size is checked here.
        assert_eq!(small.height, ui.height, "the same text size");
    }

    #[gpui::test]
    fn code_label_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || Label::new("andy/fx1").code().into_any_element());
        assert!(size.width > gpui::px(0.));
    }
}
