//! A run of text in one of Zed's text sizes and a theme color.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, Pixels, Rems, RenderOnce, SharedString,
    Styled as _, Window, div, relative,
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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Zed's scale; the Settings and dialog titles, its users, are Title now (V97, C247)"
        )
    )]
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
    /// `None` is [`LabelSize::Default`], or the code font's own size for
    /// code text (C252).
    size: Option<LabelSize>,
    color: Color,
    weight: Option<FontWeight>,
    code: bool,
    wrap: bool,
    truncate: bool,
    strikethrough: bool,
    underline: bool,
    /// A fixed line height; `None` is [`LINE_HEIGHT`] times the size.
    line_height: Option<Pixels>,
}

impl Label {
    #[must_use]
    pub(crate) fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            size: None,
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

    /// Sets a fixed line height, for text the mockups draw at a font's own
    /// line height rather than 1.2 times its size.
    #[must_use]
    pub(crate) const fn line_height(mut self, height: Pixels) -> Self {
        self.line_height = Some(height);
        self
    }

    #[must_use]
    pub(crate) const fn size(mut self, size: LabelSize) -> Self {
        self.size = Some(size);
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
    /// values people type. With no size set it takes the code font's own
    /// size; a [`LabelSize`] set with [`Label::size`], Default included,
    /// takes that step of the UI scale, as Zed's `buffer_font` labels do
    /// (the Git panel's mono 11 metadata, V93).
    #[must_use]
    pub(crate) const fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// Lets the text wrap and shrink to the space left in its row. In a
    /// column it is as tall as its lines.
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
        let base = div().text_color(color).line_height(
            self.line_height
                .map_or_else(|| relative(LINE_HEIGHT), gpui::DefiniteLength::from),
        );
        let size = self.size.unwrap_or_default().rems();
        let sized = match (self.code, self.size) {
            (true, None) => base
                .font_family(appearance.fonts.code_family.clone())
                .text_size(appearance.fonts.code_size),
            (true, Some(_)) => base
                .font_family(appearance.fonts.code_family.clone())
                .text_size(size),
            (false, _) => base.text_size(size),
        };
        sized
            .when_some(self.weight, gpui::Styled::font_weight)
            .when(self.strikethrough, gpui::Styled::line_through)
            .when(self.underline, gpui::Styled::underline)
            .map(|this| {
                if self.wrap {
                    // Full width, shrinking to what its row leaves: flex_1
                    // would also grow it down a column with free height.
                    this.w_full().min_w_0()
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
#[path = "label_tests.rs"]
mod tests;
