//! A collapsible side dock, and the strips at the top of each dock.

use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::components::{Color, IconButton, IconButtonStyle, IconSize, Label, Tab};
use super::tokens::{BORDER_WIDTH, SPACE_8, SPACE_10, SPACE_14, TAB_BAR_HEIGHT};
use super::workspace::{ToggleLeftDock, ToggleRightDock};
use crate::icons::UiIcon;

/// Which side of the window a dock sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockSide {
    Left,
    Right,
}

/// A dock's open state and width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Dock {
    open: bool,
    width: Pixels,
}

impl Dock {
    pub(crate) const fn new(width: Pixels) -> Self {
        Self { open: true, width }
    }

    pub(crate) const fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) const fn width(&self) -> Pixels {
        self.width
    }

    pub(crate) const fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub(crate) const fn open(&mut self) {
        self.open = true;
    }
}

/// The left dock's tab strip and its hide button, with its rule inside.
#[derive(Debug, IntoElement)]
pub(crate) struct LeftDockTabs;

impl RenderOnce for LeftDockTabs {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        // The mockup's gaps overflow the 272 px dock; 8 px gaps fit all four
        // tabs and the hide button.
        div()
            .debug_selector(|| "dock-tabs".into())
            .h(TAB_BAR_HEIGHT)
            .border_b(BORDER_WIDTH)
            .border_color(super::rule_color(cx))
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_8)
            .pl(SPACE_10)
            .pr(SPACE_8)
            .child(Tab::new("tab-agents", UiIcon::Agents, "Agents").selected(true))
            .child(Tab::new("tab-project", UiIcon::Project, "Project"))
            .child(Tab::new("tab-git", UiIcon::Git, "Git"))
            .child(Tab::new("tab-roles", UiIcon::Roles, "Roles"))
            .child(div().flex_1())
            .child(
                IconButton::new("hide-left-dock", UiIcon::ChevronLeft)
                    .size(IconSize::Glyph)
                    .color(Color::Muted)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleLeftDock), cx);
                    }),
            )
    }
}

/// A right dock panel's title, subtitle and close button, with its rule
/// inside.
#[derive(Debug, IntoElement)]
pub(crate) struct RightDockHeader {
    title: SharedString,
    subtitle: SharedString,
}

impl RightDockHeader {
    #[must_use]
    pub(crate) fn new(title: impl Into<SharedString>, subtitle: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
        }
    }
}

impl RenderOnce for RightDockHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .debug_selector(|| "right-dock-head".into())
            .h(TAB_BAR_HEIGHT)
            .border_b(BORDER_WIDTH)
            .border_color(super::rule_color(cx))
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_8)
            .px(SPACE_14)
            .child(Label::new(self.title).weight(FontWeight::SEMIBOLD))
            .child(Label::new(self.subtitle).code().color(Color::Placeholder))
            .child(div().flex_1())
            .child(
                IconButton::new("close-right-dock", UiIcon::ChevronRight)
                    .size(IconSize::Glyph)
                    .style(IconButtonStyle::Filled)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleRightDock), cx);
                    }),
            )
    }
}
