//! A glyph on a solid disc: the needs-you banner's "!" and "✓" (nodes 5705
//! and 6258) and the toast's app mark (node 6006, a rounded square).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
};

use super::{Color, Icon, IconSize, Label, LabelSize};
use crate::appearance::Appearance;
use crate::icons::UiIcon;
use crate::ui::tokens::{GLYPH_DISC, RADIUS_MD};

/// What the disc shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Glyph {
    Text(SharedString),
    Icon(UiIcon),
}

#[derive(Debug, IntoElement)]
pub(crate) struct GlyphDisc {
    glyph: Glyph,
    color: Color,
    square: bool,
}

impl GlyphDisc {
    /// `glyph` in bold on a round fill of `color`.
    #[must_use]
    pub(crate) fn text(glyph: impl Into<SharedString>, color: Color) -> Self {
        Self {
            glyph: Glyph::Text(glyph.into()),
            color,
            square: false,
        }
    }

    /// A UI icon on a round fill of `color`, as the answered banner's check.
    #[must_use]
    pub(crate) const fn icon(icon: UiIcon, color: Color) -> Self {
        Self {
            glyph: Glyph::Icon(icon),
            color,
            square: false,
        }
    }

    /// Rounded corners instead of a circle, as an app icon (node 6006).
    #[must_use]
    pub(crate) const fn square(mut self) -> Self {
        self.square = true;
        self
    }
}

impl RenderOnce for GlyphDisc {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let fill = self.color.hsla(&cx.global::<Appearance>().theme);
        let glyph = match self.glyph {
            Glyph::Text(text) => Label::new(text)
                .size(LabelSize::Small)
                .weight(FontWeight::BOLD)
                .color(Color::Background)
                .into_any_element(),
            Glyph::Icon(icon) => Icon::new(icon)
                .size(IconSize::Small)
                .color(Color::Background)
                .into_any_element(),
        };
        div()
            .size(GLYPH_DISC)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .bg(fill)
            .map(|this| {
                if self.square {
                    this.rounded(RADIUS_MD)
                } else {
                    this.rounded_full()
                }
            })
            .child(glyph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use crate::ui::tokens::GLYPH_DISC;
    use gpui::{TestAppContext, size};

    #[gpui::test]
    fn disc_is_the_glyph_disc_size(cx: &mut TestAppContext) {
        let disc = rendered_size(cx, || GlyphDisc::text("!", Color::Error).into_any_element());
        assert_eq!(disc, size(GLYPH_DISC, GLYPH_DISC));
    }

    #[gpui::test]
    fn square_and_icon_discs_keep_the_size(cx: &mut TestAppContext) {
        let square = rendered_size(cx, || {
            GlyphDisc::text("!", Color::Error)
                .square()
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let icon = rendered_size(&mut cx2, || {
            GlyphDisc::icon(UiIcon::Check, Color::Success).into_any_element()
        });
        assert_eq!(square, size(GLYPH_DISC, GLYPH_DISC));
        assert_eq!(icon, size(GLYPH_DISC, GLYPH_DISC));
    }
}
