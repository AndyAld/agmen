//! Verb badges (DONE, ALERT, ...) and count pills.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{BADGE_TINT, RADIUS_SM, SPACE_1, SPACE_5, SPACE_7};

/// The color of a message verb's badge, from `VERB_COLOR` in
/// `docs/mockups/scripts/console.py`. "to" badges, such as "to listen", are accented.
#[must_use]
pub(crate) fn verb_color(verb: &str) -> Color {
    match verb {
        "DONE" | "DECISION" => Color::Success,
        "ALERT" | "REVIEW" | "NEEDS YOU" => Color::Error,
        "DELEGATED" | "PROPOSAL" | "SETTINGS" => Color::Accent,
        "GRANT" | "STATE" => Color::Muted,
        "ASSIGN" => Color::Role("coordinator".into()),
        "CLAIM" => Color::Role("engineer".into()),
        _ if verb.starts_with("to ") => Color::Accent,
        _ => Color::Default,
    }
}

#[derive(Debug, IntoElement)]
pub(crate) struct Badge {
    label: SharedString,
    color: Color,
    solid: bool,
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
            code: false,
            leading: None,
        }
    }

    /// A message verb's badge, in the code font.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "chat messages use it in Task 9 of the desktop plan"
        )
    )]
    #[must_use]
    pub(crate) fn verb(verb: impl Into<SharedString>) -> Self {
        let verb = verb.into();
        let color = verb_color(&verb);
        Self {
            code: true,
            ..Self::new(verb, color)
        }
    }

    /// A solid red pill with a count, as the chat tab's "1".
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the chat tab uses it in Task 13 of the desktop plan"
        )
    )]
    #[must_use]
    pub(crate) fn count(count: u32) -> Self {
        Self::new(count.to_string(), Color::Error).solid()
    }

    /// Fills the badge with its color and sets the text on it in the
    /// background color.
    #[must_use]
    pub(crate) const fn solid(mut self) -> Self {
        self.solid = true;
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
            (fill.opacity(BADGE_TINT), self.color.clone())
        };
        let label = Label::new(self.label)
            .size(LabelSize::Small)
            .color(text.clone())
            .weight(FontWeight::SEMIBOLD);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_5)
            .py(SPACE_1)
            .px(if self.solid { SPACE_7 } else { SPACE_5 })
            .rounded(RADIUS_SM)
            .bg(bg)
            .when_some(self.leading, |this, glyph| {
                this.child(
                    Label::new(glyph)
                        .size(LabelSize::Small)
                        .color(text)
                        .weight(FontWeight::BOLD),
                )
            })
            .child(if self.code { label.code() } else { label })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[test]
    fn badge_color_per_verb() {
        assert_eq!(verb_color("DONE"), Color::Success);
        assert_eq!(verb_color("ALERT"), Color::Error);
        assert_eq!(verb_color("REVIEW"), Color::Error);
        assert_eq!(verb_color("DELEGATED"), Color::Accent);
        assert_eq!(verb_color("ANSWER"), Color::Default);
        assert_eq!(verb_color("GRANT"), Color::Muted);
        assert_eq!(verb_color("FYI"), Color::Default);
        assert_eq!(verb_color("to listen"), Color::Accent);
        assert_eq!(verb_color("to sean/plan"), Color::Accent);
        assert_eq!(verb_color("ASSIGN"), Color::Role("coordinator".into()));
        assert_eq!(verb_color("CLAIM"), Color::Role("engineer".into()));
        assert_eq!(Badge::verb("DONE").color, Color::Success);
        assert!(Badge::count(1).solid);
    }

    #[gpui::test]
    fn badge_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || Badge::verb("DONE").into_any_element());
        assert!(size.width > px(10.));
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
