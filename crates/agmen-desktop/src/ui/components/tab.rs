//! A tab: a dock tab (icon and label, bright and bold when selected), or a
//! center pane tab with a fill, a rule and a close button slot.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div,
};

use super::{Badge, ClickHandler, Color, Icon, IconButton, IconSize, Label, LabelSize};
use crate::appearance::Appearance;
use crate::icons::UiIcon;
use crate::ui::tokens::{BORDER_WIDTH, RADIUS_SM, SPACE_5, SPACE_8, SPACE_14, TAB_CLOSE_SLOT};

/// Where the tab sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TabStyle {
    /// The left dock's tab strip: icon and label only.
    #[default]
    Dock,
    /// A center pane's tab bar (node 116).
    Pane,
}

#[derive(IntoElement)]
pub(crate) struct Tab {
    id: ElementId,
    icon: UiIcon,
    icon_color: Option<Color>,
    label: SharedString,
    selected: bool,
    style: TabStyle,
    code: bool,
    badge: Option<SharedString>,
    on_click: Option<ClickHandler>,
    on_close: Option<ClickHandler>,
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
            icon_color: None,
            label: label.into(),
            selected: false,
            style: TabStyle::Dock,
            code: false,
            badge: None,
            on_click: None,
            on_close: None,
        }
    }

    #[must_use]
    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Draws the tab as a center pane tab: body text size, a fill when
    /// selected, a rule on its right, and a close button slot that every
    /// tab reserves, so tabs keep their width (mockup fix 40). Tests find
    /// the tab by its id and the slot as `"{id}:close"` with `debug_bounds`.
    #[must_use]
    pub(crate) const fn pane(mut self) -> Self {
        self.style = TabStyle::Pane;
        self
    }

    /// The icon's color; by default it follows the label.
    #[must_use]
    pub(crate) fn icon_color(mut self, color: Color) -> Self {
        self.icon_color = Some(color);
        self
    }

    /// Sets the label in the code font, for handles and file names.
    #[must_use]
    pub(crate) const fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// A red count after a pane tab's close slot, as the chat tab's alerts
    /// that need the person (console.py `tab_bar`); nothing at zero. Tests
    /// find it as `"{id}:badge"`.
    #[must_use]
    pub(crate) fn badge(mut self, count: usize) -> Self {
        self.badge = (count > 0).then(|| count.to_string().into());
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

    /// Called when a pane tab's close button is clicked. The click does not
    /// also reach the tab's `on_click`.
    #[must_use]
    pub(crate) fn on_close(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_close = Some(Box::new(handler));
        self
    }

    fn render_dock(self, cx: &App) -> gpui::Stateful<gpui::Div> {
        let hover = cx.global::<Appearance>().theme.element_hover;
        let selector = self.id.to_string();
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
            .debug_selector(|| selector)
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_5)
            .rounded(RADIUS_SM)
            .hover(move |style| style.bg(hover))
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            .child(Icon::new(self.icon).color(self.icon_color.unwrap_or(color)))
            .child(if self.selected {
                label.weight(FontWeight::SEMIBOLD)
            } else {
                label
            })
    }

    fn render_pane(self, cx: &App) -> gpui::Stateful<gpui::Div> {
        let theme = &cx.global::<Appearance>().theme;
        let color = if self.selected {
            Color::Default
        } else {
            Color::Placeholder
        };
        let selector = self.id.to_string();
        let group: SharedString = format!("{selector}:group").into();
        let slot_selector = format!("{selector}:close");
        let badge_selector = format!("{selector}:badge");
        let label = Label::new(self.label).color(color.clone());
        let close = self.on_close.map(|handler| {
            IconButton::new(format!("{selector}:close-button"), UiIcon::Close)
                .size(IconSize::Glyph)
                .color(Color::Muted)
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                })
        });
        div()
            .id(self.id)
            .debug_selector(|| selector)
            .group(group.clone())
            .h_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_8)
            .px(SPACE_14)
            .border_r(BORDER_WIDTH)
            .border_color(theme.border_variant)
            .when(self.selected, |this| this.bg(theme.elevated_surface))
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            .child(Icon::new(self.icon).color(self.icon_color.unwrap_or(color)))
            .child(if self.code { label.code() } else { label })
            // Zed shows an inactive tab's close button on hover, in the
            // same slot, so the slot is always there.
            .child(
                div()
                    .debug_selector(|| slot_selector)
                    .w(TAB_CLOSE_SLOT)
                    .flex_none()
                    .flex()
                    .justify_center()
                    .when(!self.selected, |this| {
                        this.invisible().group_hover(group, |style| style.visible())
                    })
                    .children(close),
            )
            .when_some(self.badge, |this, count| {
                this.child(
                    div()
                        .debug_selector(|| badge_selector)
                        .child(Badge::new(count, Color::Error).solid()),
                )
            })
    }
}

impl RenderOnce for Tab {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        match self.style {
            TabStyle::Dock => self.render_dock(cx),
            TabStyle::Pane => self.render_pane(cx),
        }
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

    #[gpui::test]
    fn pane_tab_keeps_its_width_when_selected(cx: &mut TestAppContext) {
        let make = |selected: bool| {
            move || {
                Tab::new("chat", UiIcon::Chat, "chat")
                    .pane()
                    .icon_color(Color::Accent)
                    .selected(selected)
                    .on_close(|_, _, _| {})
                    .into_any_element()
            }
        };
        let active = rendered_size(cx, make(true));
        let mut cx2 = TestAppContext::single();
        let inactive = rendered_size(&mut cx2, make(false));
        assert_eq!(active.width, inactive.width);
        assert!(active.width > TAB_CLOSE_SLOT + SPACE_14 * 2.);
    }

    #[gpui::test]
    fn pane_tab_badge_shows_only_above_zero(cx: &mut TestAppContext) {
        let make = |count: usize| {
            move || {
                Tab::new("chat", UiIcon::Chat, "chat")
                    .pane()
                    .badge(count)
                    .into_any_element()
            }
        };
        let plain = rendered_size(cx, make(0));
        let mut cx2 = TestAppContext::single();
        let counted = rendered_size(&mut cx2, make(1));
        assert!(counted.width > plain.width);
    }
}
