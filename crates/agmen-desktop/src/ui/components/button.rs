//! A text button, such as "Change listener…" or the status bar's Listener
//! toggle.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div,
};

use super::{ClickHandler, Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{BORDER_WIDTH, RADIUS_MD, RADIUS_SM, SPACE_1, SPACE_5, SPACE_7, SPACE_10};

/// How the button is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ButtonStyle {
    /// Surface fill with a border, as "Change listener…".
    #[default]
    Outlined,
    /// Selected fill, compact, as the status bar's Listener toggle.
    Filled,
}

#[derive(IntoElement)]
pub(crate) struct Button {
    id: ElementId,
    label: SharedString,
    style: ButtonStyle,
    leading: Option<AnyElement>,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for Button {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Button")
            .field("id", &self.id)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl Button {
    #[must_use]
    pub(crate) fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            style: ButtonStyle::Outlined,
            leading: None,
            on_click: None,
        }
    }

    #[must_use]
    pub(crate) const fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    /// Something drawn before the label, such as a glyph.
    #[must_use]
    pub(crate) fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let hover = theme.element_hover;
        let selector = self.id.to_string();
        let base = div()
            .id(self.id)
            .debug_selector(|| selector)
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_5);
        let styled = match self.style {
            ButtonStyle::Outlined => base
                .bg(theme.elevated_surface)
                .border(BORDER_WIDTH)
                .border_color(theme.border)
                .rounded(RADIUS_MD)
                .py(SPACE_5)
                .px(SPACE_10),
            ButtonStyle::Filled => base
                .bg(theme.element_selected)
                .rounded(RADIUS_SM)
                .py(SPACE_1)
                .px(SPACE_7),
        };
        styled
            .hover(move |style| style.bg(hover))
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            .children(self.leading)
            .child(
                Label::new(self.label)
                    .size(LabelSize::Small)
                    .color(Color::Default),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn buttons_render(cx: &mut TestAppContext) {
        let outlined = rendered_size(cx, || {
            Button::new("change", "Change listener…").into_any_element()
        });
        assert!(outlined.width > px(20.));
    }

    #[gpui::test]
    fn filled_button_renders(cx: &mut TestAppContext) {
        let filled = rendered_size(cx, || {
            Button::new("toggle", "Listener")
                .style(ButtonStyle::Filled)
                .leading(Label::new("◨").color(Color::Accent))
                .on_click(|_, _, _| {})
                .into_any_element()
        });
        assert!(filled.width > px(14.));
    }
}
