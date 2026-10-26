//! A key binding hint, such as ⌘⇧P.

use gpui::{App, IntoElement, RenderOnce, SharedString, Window};

use super::{Color, Label};

#[derive(Debug, IntoElement)]
pub(crate) struct KeyHint {
    keys: SharedString,
    color: Color,
}

impl KeyHint {
    #[must_use]
    pub(crate) fn new(keys: impl Into<SharedString>) -> Self {
        Self {
            keys: keys.into(),
            color: Color::Accent,
        }
    }

    /// The hint's color; accent by default.
    #[must_use]
    pub(crate) fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

impl RenderOnce for KeyHint {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        Label::new(self.keys).code().color(self.color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn key_hint_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || KeyHint::new("⌘⇧P").into_any_element());
        assert!(size.width > px(0.));
    }
}
