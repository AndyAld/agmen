//! A collapsible side dock, and the strips at the top of each dock.

use gpui::{
    App, FontWeight, IntoElement, ParentElement as _, Pixels, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use super::components::{Color, Divider, IconButton, IconButtonStyle, IconSize, Label, Tab};
use super::tokens::{SPACE_8, SPACE_10, SPACE_14, TAB_BAR_HEIGHT};
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

/// The left dock's tab strip and its hide button, with the rule under it.
#[derive(Debug, IntoElement)]
pub(crate) struct LeftDockTabs;

impl RenderOnce for LeftDockTabs {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // The mockup's gaps overflow the 272 px dock; 8 px gaps fit all four
        // tabs and the hide button.
        let strip = div()
            .h(TAB_BAR_HEIGHT)
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
                    .size(IconSize::Small)
                    .color(Color::Muted)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleLeftDock), cx);
                    }),
            );
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(strip)
            .child(Divider::horizontal())
    }
}

/// A right dock panel's title, subtitle and close button, with the rule
/// under it.
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
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let head = div()
            .h(TAB_BAR_HEIGHT)
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
                    .size(IconSize::Small)
                    .style(IconButtonStyle::Filled)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleRightDock), cx);
                    }),
            );
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(head)
            .child(Divider::horizontal())
    }
}
