//! A clickable icon.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::{Color, Icon, IconSize};
use crate::appearance::Appearance;
use crate::icons::UiIcon;
use crate::ui::tokens::{DOCK_BUTTON, RADIUS_SM};

/// What a click does.
pub(crate) type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// How the button is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum IconButtonStyle {
    /// The bare icon, with a hover fill.
    #[default]
    Ghost,
    /// On a surface fill, as the right dock's close button.
    Filled,
}

#[derive(IntoElement)]
pub(crate) struct IconButton {
    id: ElementId,
    icon: UiIcon,
    color: Color,
    size: IconSize,
    style: IconButtonStyle,
    selected: bool,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for IconButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IconButton")
            .field("id", &self.id)
            .field("icon", &self.icon)
            .finish_non_exhaustive()
    }
}

impl IconButton {
    #[must_use]
    pub(crate) fn new(id: impl Into<ElementId>, icon: UiIcon) -> Self {
        Self {
            id: id.into(),
            icon,
            color: Color::Placeholder,
            size: IconSize::Medium,
            style: IconButtonStyle::Ghost,
            selected: false,
            on_click: None,
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

    #[must_use]
    pub(crate) const fn style(mut self, style: IconButtonStyle) -> Self {
        self.style = style;
        self
    }

    /// Draws the button's on state, as a status bar panel button while its
    /// panel shows: a selected fill behind a bright icon.
    #[must_use]
    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
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

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let hover = theme.element_hover;
        let selected_fill = theme.element_selected;
        let color = if self.selected {
            Color::Default
        } else {
            self.color
        };
        let selector = self.id.to_string();
        div()
            .id(self.id)
            .debug_selector(|| selector)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(RADIUS_SM)
            .when(self.style == IconButtonStyle::Filled, |this| {
                this.bg(theme.elevated_surface)
                    .w(DOCK_BUTTON.width)
                    .h(DOCK_BUTTON.height)
            })
            .when(self.selected, |this| this.bg(selected_fill))
            .hover(move |style| style.bg(hover))
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            .child(Icon::new(self.icon).color(color).size(self.size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn icon_button_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            IconButton::new("close", UiIcon::ChevronRight)
                .style(IconButtonStyle::Filled)
                .on_click(|_, _, _| {})
                .into_any_element()
        });
        // Mockup check V63: the dock's collapse button is 17 by 20.
        assert_eq!(size, gpui::size(px(17.), px(20.)));
    }
}
