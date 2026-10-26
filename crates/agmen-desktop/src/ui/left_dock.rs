//! The left dock's tabs, and the status bar buttons that open them.

use gpui::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div,
};

use super::components::{IconButton, Tab};
use super::tokens::SPACE_10;
use crate::icons::UiIcon;
use crate::keymap::{OpenSearch, ToggleGitPanel, ToggleProjectPanel};

/// Shows a left dock tab, opening the dock: a click on the tab strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct ShowLeftDockTab(pub(crate) LeftDockTab);

/// Which panel the left dock shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum LeftDockTab {
    #[default]
    Agents,
    Project,
    Git,
    Roles,
}

impl LeftDockTab {
    /// The tabs in the strip's order.
    pub(crate) const ALL: [Self; 4] = [Self::Agents, Self::Project, Self::Git, Self::Roles];

    const fn icon(self) -> UiIcon {
        match self {
            Self::Agents => UiIcon::Agents,
            Self::Project => UiIcon::Project,
            Self::Git => UiIcon::Git,
            Self::Roles => UiIcon::Roles,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::Project => "Project",
            Self::Git => "Git",
            Self::Roles => "Roles",
        }
    }

    /// The tab's element id and test selector, such as `tab-project`.
    fn selector(self) -> SharedString {
        format!("tab-{}", self.label().to_lowercase()).into()
    }

    /// The tab in the strip; a click shows it.
    pub(crate) fn tab(self, selected: Self) -> Tab {
        Tab::new(self.selector(), self.icon(), self.label())
            .selected(self == selected)
            .on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(ShowLeftDockTab(self)), cx);
            })
    }
}

/// The status bar's files, git and search buttons, each drawn active while
/// its target shows. Tests find them as `status-project`, `status-git` and
/// `status-search`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, IntoElement)]
pub(crate) struct PanelButtons {
    /// The left dock's tab, or `None` while the dock is hidden.
    shown: Option<LeftDockTab>,
    /// Whether the active pane shows the Search tab.
    search: bool,
}

impl PanelButtons {
    #[must_use]
    pub(crate) const fn new(shown: Option<LeftDockTab>, search: bool) -> Self {
        Self { shown, search }
    }
}

impl RenderOnce for PanelButtons {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let project = matches!(self.shown, Some(LeftDockTab::Project));
        let git = matches!(self.shown, Some(LeftDockTab::Git));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_10)
            .child(
                IconButton::new("status-project", UiIcon::Project)
                    .selected(project)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleProjectPanel), cx);
                    }),
            )
            .child(
                IconButton::new("status-git", UiIcon::Git)
                    .selected(git)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleGitPanel), cx);
                    }),
            )
            .child(
                IconButton::new("status-search", UiIcon::Search)
                    .selected(self.search)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(OpenSearch), cx);
                    }),
            )
    }
}
