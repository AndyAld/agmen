//! The bordered box around a one-line text input, such as the theme search
//! in Settings (V100).

use gpui::{AnyElement, App, IntoElement, ParentElement, RenderOnce, Styled as _, Window, div};

use crate::appearance::Appearance;
use crate::ui::tokens::{BORDER_WIDTH, RADIUS_MD, SPACE_3, SPACE_8};

/// A 1 px border, 3 px above and below the input and 8 px at the sides.
#[derive(Default, IntoElement)]
pub(crate) struct InputFrame {
    children: Vec<AnyElement>,
}

impl std::fmt::Debug for InputFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputFrame").finish_non_exhaustive()
    }
}

impl InputFrame {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl ParentElement for InputFrame {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for InputFrame {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .py(SPACE_3)
            .px(SPACE_8)
            .rounded(RADIUS_MD)
            .border(BORDER_WIDTH)
            .border_color(cx.global::<Appearance>().theme.border)
            .children(self.children)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, px};

    use super::*;
    use crate::ui::test_support::rendered_size;

    #[gpui::test]
    fn frame_adds_padding_and_border(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            InputFrame::new()
                .child(div().w(px(100.)).h(px(14.)))
                .into_any_element()
        });
        assert_eq!(size.height, px(14. + 2. * 3. + 2.));
        assert_eq!(size.width, px(100. + 2. * 8. + 2.));
    }
}
