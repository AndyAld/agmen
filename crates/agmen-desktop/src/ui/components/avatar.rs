//! A person's or agent's initial in a round badge.

use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::AVATAR_SIZE;

#[derive(Debug, IntoElement)]
pub(crate) struct Avatar {
    letter: SharedString,
    color: Color,
}

impl Avatar {
    /// The first letter of `name`, uppercased.
    #[must_use]
    pub(crate) fn new(name: &str) -> Self {
        let letter: String = name
            .chars()
            .next()
            .into_iter()
            .flat_map(char::to_uppercase)
            .collect();
        Self {
            letter: letter.into(),
            color: Color::Default,
        }
    }

    /// The letter's color, such as the agent's role color.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "chat messages use it in Task 9 of the desktop plan"
        )
    )]
    #[must_use]
    pub(crate) fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

impl RenderOnce for Avatar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        div()
            .size(AVATAR_SIZE)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(theme.element_selected)
            .child(
                Label::new(self.letter)
                    .size(LabelSize::Small)
                    .color(self.color)
                    .weight(FontWeight::SEMIBOLD),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::TestAppContext;

    #[test]
    fn avatar_takes_the_first_letter() {
        assert_eq!(Avatar::new("andy").letter.as_ref(), "A");
        assert_eq!(Avatar::new("").letter.as_ref(), "");
    }

    #[gpui::test]
    fn avatar_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            Avatar::new("andy")
                .color(Color::Role("engineer".into()))
                .into_any_element()
        });
        assert_eq!(size.width, AVATAR_SIZE);
    }
}
