//! Verb badges (DONE, ALERT, ...) and count pills.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{
    RADIUS_PILL, RADIUS_SM, SPACE_1, SPACE_5, SPACE_6, SPACE_7, TINT_FILL, tint,
};

#[derive(Debug, IntoElement)]
pub(crate) struct Badge {
    label: SharedString,
    color: Color,
    solid: bool,
    pill: bool,
    marker: bool,
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
            pill: false,
            marker: false,
            code: false,
            leading: None,
        }
    }

    /// Sets the label in the code font at regular weight, as a message's
    /// verb (node 145).
    #[must_use]
    pub(crate) const fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// Fills the badge with its color and sets the text on it in the
    /// background color.
    #[must_use]
    pub(crate) const fn solid(mut self) -> Self {
        self.solid = true;
        self
    }

    /// A solid count pill: fully rounded, padded only at the sides, with a
    /// bold extra small label, as the chat tab's alert count (console.py
    /// `tab_bar`).
    #[must_use]
    pub(crate) const fn pill(mut self) -> Self {
        self.solid = true;
        self.pill = true;
        self
    }

    /// A solid marker in a message's head, as an alert's NEEDS YOU (node
    /// 5960): padded 1 by 6, with a bold extra small label.
    #[must_use]
    pub(crate) const fn marker(mut self) -> Self {
        self.solid = true;
        self.marker = true;
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
            (tint(fill, TINT_FILL), self.color.clone())
        };
        let label = Label::new(self.label).color(text.clone());
        let label = if self.pill || self.marker {
            label.size(LabelSize::XSmall).weight(FontWeight::BOLD)
        } else if self.code {
            label.code()
        } else {
            label.size(LabelSize::Small).weight(FontWeight::SEMIBOLD)
        };
        let base = div().flex().flex_none().items_center().gap(SPACE_5).bg(bg);
        let shaped = if self.pill {
            base.px(SPACE_5).rounded(RADIUS_PILL)
        } else {
            base.py(SPACE_1)
                .px(if self.solid && !self.marker {
                    SPACE_7
                } else {
                    SPACE_6
                })
                .rounded(RADIUS_SM)
        };
        shaped
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
    fn code_badge_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            Badge::new("DONE", Color::Success).code().into_any_element()
        });
        assert!(size.width > px(10.));
    }

    #[gpui::test]
    fn tinted_badge_pads_six_each_side(cx: &mut TestAppContext) {
        // Node 145: the verb badge pads [1, 6, 1, 6].
        let badge = rendered_size(cx, || {
            Badge::new("DONE", Color::Success).code().into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let text = rendered_size(&mut cx2, || Label::new("DONE").code().into_any_element());
        assert_eq!(badge.width - text.width, px(12.));
        assert_eq!(badge.height - text.height, px(2.));
    }

    #[gpui::test]
    fn pill_pads_five_each_side_and_none_above(cx: &mut TestAppContext) {
        // console.py `tab_bar`: the count pads [0, 5, 0, 5] around 10 px bold.
        let pill = rendered_size(cx, || {
            Badge::new("1", Color::Error).pill().into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let text = rendered_size(&mut cx2, || {
            Label::new("1")
                .size(LabelSize::XSmall)
                .weight(FontWeight::BOLD)
                .into_any_element()
        });
        assert_eq!(pill.width - text.width, px(10.));
        assert_eq!(pill.height, text.height);
    }

    #[gpui::test]
    fn marker_pads_six_by_one_around_bold_extra_small_text(cx: &mut TestAppContext) {
        // Node 5960: NEEDS YOU pads [1, 6, 1, 6] around 10 px bold.
        let marker = rendered_size(cx, || {
            Badge::new("NEEDS YOU", Color::Error)
                .marker()
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let text = rendered_size(&mut cx2, || {
            Label::new("NEEDS YOU")
                .size(LabelSize::XSmall)
                .weight(FontWeight::BOLD)
                .into_any_element()
        });
        assert_eq!(marker.width - text.width, px(12.));
        assert_eq!(marker.height - text.height, px(2.));
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
