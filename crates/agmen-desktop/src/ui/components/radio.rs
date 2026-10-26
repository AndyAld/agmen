//! A radio option: "◉ this message" or "○ until I change it" in the model
//! picker (nodes 3405 and 3408). On, the glyph is accent and the label
//! text; off, both are the placeholder color.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::{ClickHandler, Color, Label, LabelSize};
use crate::ui::tokens::SPACE_6;

#[derive(IntoElement)]
pub(crate) struct Radio {
    id: SharedString,
    label: SharedString,
    on: bool,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for Radio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Radio")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("on", &self.on)
            .finish_non_exhaustive()
    }
}

impl Radio {
    /// `id` names the option; tests find it with `debug_bounds`.
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            on: false,
            on_click: None,
        }
    }

    #[must_use]
    pub(crate) const fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    /// Called when the option is clicked while it is off.
    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Radio {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let selector = self.id.to_string();
        let (glyph, glyph_color, label_color) = if self.on {
            ("◉", Color::Accent, Color::Default)
        } else {
            ("○", Color::Placeholder, Color::Placeholder)
        };
        div()
            .id(self.id)
            .debug_selector(|| selector)
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_6)
            .when(!self.on, |this| {
                this.when_some(self.on_click, |this, handler| {
                    this.cursor_pointer().on_click(handler)
                })
            })
            .child(Label::new(glyph).size(LabelSize::Small).color(glyph_color))
            .child(
                Label::new(self.label)
                    .size(LabelSize::Small)
                    .color(label_color),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::control_harness::{Reported, open};
    use crate::ui::test_support::click;
    use gpui::TestAppContext;

    #[gpui::test]
    fn an_off_option_reports_its_click(cx: &mut TestAppContext) {
        let reported = Reported::default();
        let sink = reported.clone();
        let cx = open(cx, move || {
            let (on, off) = (sink.clone(), sink.clone());
            div()
                .child(
                    Radio::new("this", "this message")
                        .on(true)
                        .on_click(move |_, _, _| on.set(Some(0))),
                )
                .child(
                    Radio::new("sticky", "until I change it").on_click(move |_, _, _| {
                        off.set(Some(1));
                    }),
                )
                .into_any_element()
        });
        click(cx, "this");
        assert_eq!(reported.get(), None, "the on option does nothing");
        click(cx, "sticky");
        assert_eq!(reported.get(), Some(1));
    }
}
