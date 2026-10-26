//! A dock tab: icon and label, bright and bold when selected.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div,
};

use super::{ClickHandler, Color, Icon, Label, LabelSize};
use crate::icons::UiIcon;
use crate::ui::tokens::SPACE_5;

#[derive(IntoElement)]
pub(crate) struct Tab {
    id: ElementId,
    icon: UiIcon,
    label: SharedString,
    selected: bool,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for Tab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tab")
            .field("label", &self.label)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Tab {
    #[must_use]
    pub(crate) fn new(
        id: impl Into<ElementId>,
        icon: UiIcon,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            label: label.into(),
            selected: false,
            on_click: None,
        }
    }

    #[must_use]
    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the Project, Git and Roles tabs open in Tasks 10 to 12"
        )
    )]
    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Tab {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let color = if self.selected {
            Color::Default
        } else {
            Color::Placeholder
        };
        let label = Label::new(self.label)
            .size(LabelSize::Small)
            .color(color.clone());
        div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_5)
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            .child(Icon::new(self.icon).color(color))
            .child(if self.selected {
                label.weight(FontWeight::SEMIBOLD)
            } else {
                label
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn tab_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            Tab::new("agents", UiIcon::Agents, "Agents")
                .selected(true)
                .on_click(|_, _, _| {})
                .into_any_element()
        });
        assert!(size.width > px(14.));
    }
}
