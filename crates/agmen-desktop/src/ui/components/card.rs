//! A popover surface: a reference's hover card, a change card, a menu
//! (console.py's change card, node 1373, and the switch menu, node 1263).

use gpui::prelude::Refineable as _;
use gpui::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, StyleRefinement, Styled, Window, div,
};

use crate::appearance::Appearance;
use crate::ui::tokens::{
    BORDER_WIDTH, RADIUS_LG, SHADOW_BLUR, SHADOW_OFFSET_Y, SPACE_6, SPACE_10, SPACE_12, drop_shadow,
};

/// A column on the elevated surface with the theme's `border`, rounded, padded
/// 10 by 12 with 6 between its children. Callers change the width, padding
/// or border color through [`Styled`].
#[derive(IntoElement)]
pub(crate) struct Card {
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl std::fmt::Debug for Card {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Card")
            .field("children", &self.children.len())
            .finish_non_exhaustive()
    }
}

impl Card {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            style: StyleRefinement::default(),
            children: Vec::new(),
        }
    }
}

impl Card {
    /// Lifts the card above the window with the mockup's drop shadow, as
    /// the command palette (node 11473): 10 px down, blurred 30 px
    /// ([`SHADOW_BLUR`] says how gpui takes that).
    #[must_use]
    pub(crate) fn elevated(mut self) -> Self {
        self.style.box_shadow = Some(vec![drop_shadow(SHADOW_OFFSET_Y, SHADOW_BLUR)]);
        self
    }
}

impl Styled for Card {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Card {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Card {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let mut card = div()
            .flex()
            .flex_col()
            .gap(SPACE_6)
            .py(SPACE_10)
            .px(SPACE_12)
            .rounded(RADIUS_LG)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .bg(theme.elevated_surface);
        // The caller's styles win over the defaults.
        card.style().refine(&self.style);
        card.children(self.children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px, size};

    #[gpui::test]
    fn card_pads_its_content(cx: &mut TestAppContext) {
        let card = rendered_size(cx, || {
            Card::new()
                .child(div().w(px(100.)).h(px(20.)))
                .into_any_element()
        });
        // 12 and 10 px of padding and a 1 px border on each side.
        assert_eq!(card, size(px(126.), px(42.)));
    }

    #[gpui::test]
    fn caller_styles_override_the_defaults(cx: &mut TestAppContext) {
        let card = rendered_size(cx, || {
            Card::new()
                .p(px(2.))
                .w(px(300.))
                .child(div().h(px(20.)))
                .into_any_element()
        });
        assert_eq!(card, size(px(300.), px(26.)));
    }

    #[test]
    fn elevated_card_casts_the_palette_shadow() {
        let mut card = Card::new().elevated();
        let shadows = card.style().box_shadow.clone().unwrap();
        assert_eq!(shadows.len(), 1);
        // Node 11473: 0 by 10, blur radius 30. gpui blurs with
        // `blur_radius` as the Gaussian's sigma, and a Figma blur radius is
        // two sigmas, so the shadow takes 15.
        assert_eq!(shadows[0].offset.x, px(0.));
        assert_eq!(shadows[0].offset.y, px(10.));
        assert_eq!(shadows[0].blur_radius, px(15.));
        assert!((shadows[0].color.a - 170. / 255.).abs() < 0.001);
        assert!(Card::new().style().box_shadow.is_none());
    }
}
