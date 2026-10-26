//! A folded tool call in an agent's session (`desktop-agent.webp`): "▸",
//! the tool's name in bold and its argument on a card.

use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{BORDER_WIDTH, RADIUS_CARD, SPACE_6, SPACE_8, SPACE_10};

/// Tests find the fold glyph as `tool-card:fold`.
#[derive(Debug, IntoElement)]
pub(crate) struct ToolCard {
    name: SharedString,
    arg: SharedString,
}

impl ToolCard {
    #[must_use]
    pub(crate) fn new(name: impl Into<SharedString>, arg: impl Into<SharedString>) -> Self {
        Self {
            name: name.into(),
            arg: arg.into(),
        }
    }
}

impl RenderOnce for ToolCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        div()
            .flex()
            .items_center()
            .gap(SPACE_8)
            .py(SPACE_6)
            .px(SPACE_10)
            .rounded(RADIUS_CARD)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .bg(theme.elevated_surface)
            .child(
                div()
                    .debug_selector(|| "tool-card:fold".into())
                    .child(Label::new("▸").size(LabelSize::Small).color(Color::Muted)),
            )
            .child(
                Label::new(self.name)
                    .code()
                    .weight(FontWeight::BOLD)
                    .color(Color::Accent),
            )
            .child(Label::new(self.arg).code().color(Color::Placeholder).wrap())
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext};

    use super::*;
    use crate::ui::test_support::{init, redraw};
    use crate::ui::tokens::ICON_SM;

    struct Host;

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .debug_selector(|| "card".into())
                .child(ToolCard::new("Bash", "cargo test"))
        }
    }

    #[gpui::test]
    fn tool_card_folds_with_a_text_glyph(cx: &mut TestAppContext) {
        // V81: console.py draws the fold as an 11 px "▸" in icon.muted, not
        // a 12 px chevron icon.
        init(cx);
        let (_, cx) = cx.add_window_view(|_, _| Host);
        redraw(cx);
        let fold = cx.debug_bounds("tool-card:fold").unwrap();
        assert!(fold.size.width < ICON_SM, "{fold:?}");
        assert!(cx.debug_bounds("card").unwrap().size.height > fold.size.height);
    }
}
