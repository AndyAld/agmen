//! A thin bar filled to a fraction, as the Agent panel's budget bar.

use gpui::{App, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div, relative};

use super::Color;
use crate::appearance::Appearance;
use crate::ui::tokens::{PROGRESS_HEIGHT, RADIUS_XS};

#[derive(Debug, IntoElement)]
pub(crate) struct ProgressBar {
    fraction: f32,
    color: Color,
}

impl ProgressBar {
    /// `fraction` is clamped to 0 to 1; anything not finite draws empty.
    #[must_use]
    pub(crate) fn new(fraction: f32) -> Self {
        let fraction = if fraction.is_finite() {
            fraction.clamp(0., 1.)
        } else {
            0.
        };
        Self {
            fraction,
            color: Color::Accent,
        }
    }
}

impl RenderOnce for ProgressBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        div()
            .w_full()
            .h(PROGRESS_HEIGHT)
            .rounded(RADIUS_XS)
            .bg(theme.element_selected)
            .child(
                div()
                    .h_full()
                    .w(relative(self.fraction))
                    .rounded(RADIUS_XS)
                    .bg(self.color.hsla(theme)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::TestAppContext;

    #[test]
    fn fraction_is_clamped() {
        assert!((ProgressBar::new(2.).fraction - 1.).abs() < f32::EPSILON);
        assert!(ProgressBar::new(-1.).fraction.abs() < f32::EPSILON);
        assert!(ProgressBar::new(f32::NAN).fraction.abs() < f32::EPSILON);
    }

    #[gpui::test]
    fn progress_bar_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || ProgressBar::new(0.24).into_any_element());
        assert_eq!(size.height, PROGRESS_HEIGHT);
    }
}
