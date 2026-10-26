//! A Lucide UI icon in a theme color.

use gpui::{App, IntoElement, RenderOnce, Window};

use super::Color;
use crate::appearance::Appearance;
use crate::icons::UiIcon;
use crate::ui::tokens::{ICON_MD, ICON_SM};

/// Icon sizes from the mockup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum IconSize {
    /// 12 px: tree chevrons.
    Small,
    /// 14 px: tabs and bar buttons.
    #[default]
    Medium,
}

#[derive(Debug, IntoElement)]
pub(crate) struct Icon {
    icon: UiIcon,
    color: Color,
    size: IconSize,
}

impl Icon {
    #[must_use]
    pub(crate) fn new(icon: UiIcon) -> Self {
        Self {
            icon,
            color: Color::Default,
            size: IconSize::Medium,
        }
    }

    #[must_use]
    pub(crate) fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    #[must_use]
    pub(crate) const fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.hsla(&cx.global::<Appearance>().theme);
        let size = match self.size {
            IconSize::Small => ICON_SM,
            IconSize::Medium => ICON_MD,
        };
        self.icon.render(color, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn icon_renders_at_its_size(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            Icon::new(UiIcon::Bell)
                .size(IconSize::Small)
                .color(Color::Placeholder)
                .into_any_element()
        });
        assert_eq!(size.width, px(12.));
    }
}
