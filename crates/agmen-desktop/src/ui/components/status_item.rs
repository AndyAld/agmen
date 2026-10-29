//! A status bar glyph and label, with an optional compact button action.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::{Button, ButtonSize, ButtonStyle, ClickHandler, Color, Label, LabelSize};
use crate::ui::tokens::SPACE_5;

#[derive(IntoElement)]
pub(crate) struct StatusItem {
    id: SharedString,
    glyph: SharedString,
    color: Color,
    label: SharedString,
    label_color: Option<Color>,
    truncate: bool,
    button: bool,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for StatusItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusItem")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl StatusItem {
    #[must_use]
    pub(crate) fn new(
        id: impl Into<SharedString>,
        glyph: impl Into<SharedString>,
        color: Color,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            glyph: glyph.into(),
            color,
            label: label.into(),
            label_color: None,
            truncate: false,
            button: false,
            on_click: None,
        }
    }

    #[must_use]
    pub(crate) fn label_color(mut self, color: Color) -> Self {
        self.label_color = Some(color);
        self
    }

    #[must_use]
    pub(crate) const fn truncate(mut self) -> Self {
        self.truncate = true;
        self
    }

    /// Keeps the compact button layout even while its action is unavailable.
    #[must_use]
    pub(crate) const fn button(mut self) -> Self {
        self.button = true;
        self
    }

    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.button = true;
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for StatusItem {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let glyph_selector = format!("{}:glyph", self.id);
        let glyph = div().debug_selector(|| glyph_selector).flex_none().child(
            Label::new(self.glyph)
                .size(LabelSize::Small)
                .color(self.color),
        );
        if self.button {
            return Button::new(self.id, self.label)
                .style(ButtonStyle::Plain)
                .size(ButtonSize::Compact)
                .leading(glyph)
                .when_some(self.label_color, Button::label_color)
                .when(self.truncate, Button::truncate)
                .when_some(self.on_click, Button::on_click)
                .into_any_element();
        }
        let label_selector = format!("{}:label", self.id);
        div()
            .debug_selector(|| self.id.to_string())
            .flex()
            .when(self.truncate, |this| this.min_w_0())
            .when(!self.truncate, |this| this.flex_none())
            .items_center()
            .gap(SPACE_5)
            .child(glyph)
            .child(
                div()
                    .debug_selector(|| label_selector)
                    .when(self.truncate, |this| this.flex_1().min_w_0())
                    .child(
                        Label::new(self.label)
                            .size(LabelSize::Small)
                            .color(self.label_color.unwrap_or(Color::Placeholder))
                            .when(self.truncate, Label::truncate),
                    ),
            )
            .into_any_element()
    }
}
