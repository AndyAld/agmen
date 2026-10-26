//! A small filled circle in a theme color, as a role file's role dot.

use gpui::{App, IntoElement, RenderOnce, Styled as _, Window, div};

use super::Color;
use crate::appearance::Appearance;
use crate::ui::tokens::DOT_SIZE;

#[derive(Debug, IntoElement)]
pub(crate) struct Dot {
    color: Color,
}

impl Dot {
    #[must_use]
    pub(crate) const fn new(color: Color) -> Self {
        Self { color }
    }
}

impl RenderOnce for Dot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.hsla(&cx.global::<Appearance>().theme);
        div().size(DOT_SIZE).flex_none().rounded_full().bg(color)
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;
    use crate::ui::test_support::rendered_size;

    #[gpui::test]
    fn dot_is_dot_size(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || Dot::new(Color::Accent).into_any_element());
        assert_eq!(size, gpui::size(DOT_SIZE, DOT_SIZE));
    }
}
