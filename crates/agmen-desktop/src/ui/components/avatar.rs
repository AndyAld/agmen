//! A person's or agent's initial in a round badge, or a square one for an
//! agent in the chat.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, Pixels, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{AVATAR_SIZE, RADIUS_MD, TINT_AVATAR, tint};

#[derive(Debug, IntoElement)]
pub(crate) struct Avatar {
    letter: SharedString,
    color: Color,
    fill: Option<Color>,
    square: bool,
    size: Pixels,
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
            fill: None,
            square: false,
            size: AVATAR_SIZE,
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

impl Avatar {
    /// Fills the badge with a tint of `color` instead of the selected fill,
    /// as a chat message's avatar (node 139).
    #[must_use]
    pub(crate) fn fill(mut self, color: Color) -> Self {
        self.fill = Some(color);
        self
    }

    /// Rounded corners instead of a circle: agents in the chat.
    #[must_use]
    pub(crate) const fn square(mut self) -> Self {
        self.square = true;
        self
    }

    #[must_use]
    pub(crate) const fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for Avatar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let bg = self.fill.map_or(theme.element_selected, |fill| {
            tint(fill.hsla(theme), TINT_AVATAR)
        });
        div()
            .size(self.size)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .map(|this| {
                if self.square {
                    this.rounded(RADIUS_MD)
                } else {
                    this.rounded_full()
                }
            })
            .bg(bg)
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

    #[gpui::test]
    fn message_avatar_takes_its_size(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            Avatar::new("Engineer")
                .color(Color::Role("engineer".into()))
                .fill(Color::Role("engineer".into()))
                .square()
                .size(gpui::px(24.))
                .into_any_element()
        });
        assert_eq!(size.width, gpui::px(24.));
    }
}
