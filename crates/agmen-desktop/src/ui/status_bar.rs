//! The status bar: panel buttons, counts, the listener, spend, the Listener
//! toggle and the right-side buttons.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::chat::AlertCounts;
use super::components::{Badge, Button, ButtonStyle, Color, IconButton, KeyHint, Label, LabelSize};
use super::format::{counted, counted_capped, listener_label, spend_today};
use super::keys::OpenSettings;
use super::left_dock::PanelButtons;
use super::tokens::{BORDER_WIDTH, SPACE_5, SPACE_10, SPACE_14, STATUS_BAR_HEIGHT};
use super::workspace::{OpenAlerts, ToggleRightDock};
use crate::backend::{Snapshot, StatusSummary};
use crate::icons::UiIcon;

#[derive(Debug, IntoElement)]
pub(crate) struct StatusBar {
    status: StatusSummary,
    /// The listener's state, or what the active tab is about.
    context: SharedString,
    right_dock_open: bool,
    panels: PanelButtons,
    /// Task 13: the chat's counts, not stored numbers (mockup fix 34).
    alerts: AlertCounts,
}

impl StatusBar {
    #[must_use]
    pub(crate) fn new(snapshot: &Snapshot, right_dock_open: bool) -> Self {
        Self {
            status: snapshot.status.clone(),
            context: listener_label(&snapshot.listener.agent, &snapshot.listener.state).into(),
            right_dock_open,
            panels: PanelButtons::default(),
            alerts: AlertCounts::default(),
        }
    }

    /// The red needs-you pill and the alert count, from the chat's alert
    /// state (Task 13).
    #[must_use]
    pub(crate) const fn alert_counts(mut self, counts: AlertCounts) -> Self {
        self.alerts = counts;
        self
    }

    /// The files, git and search buttons and what they show as active.
    #[must_use]
    pub(crate) const fn panels(mut self, panels: PanelButtons) -> Self {
        self.panels = panels;
        self
    }

    /// Shows `context` in the listener's place, as "settings" while
    /// Settings is the active tab (frame "Desktop · settings", node 8240).
    #[must_use]
    pub(crate) fn context(mut self, context: impl Into<SharedString>) -> Self {
        self.context = context.into();
        self
    }
}

/// A glyph and its count, as "● 9 online".
fn item(glyph: &'static str, color: Color, label: String) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(SPACE_5)
        .child(Label::new(glyph).size(LabelSize::Small).color(color))
        .child(
            Label::new(label)
                .size(LabelSize::Small)
                .color(Color::Placeholder),
        )
}

/// The right-side buttons; the gear opens Settings.
fn buttons(icons: &[(&'static str, UiIcon)]) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(SPACE_10)
        .children(icons.iter().map(|(id, icon)| IconButton::new(*id, *icon)))
        .child(
            IconButton::new("status-settings", UiIcon::Settings).on_click(|_, window, cx| {
                window.dispatch_action(Box::new(OpenSettings), cx);
            }),
        )
}

impl RenderOnce for StatusBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let s = &self.status;
        div()
            .debug_selector(|| "status-bar".into())
            .h(STATUS_BAR_HEIGHT)
            .border_t(BORDER_WIDTH)
            .border_color(super::rule_color(cx))
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_14)
            .px(SPACE_10)
            .child(self.panels)
            .when(self.alerts.needs_you > 0, |this| {
                this.child(
                    div().debug_selector(|| "needs-you-pill".into()).child(
                        Badge::new(
                            counted_capped(self.alerts.needs_you, "needs you", "need you"),
                            Color::Error,
                        )
                        .solid()
                        .leading("!"),
                    ),
                )
            })
            .child(item(
                "●",
                Color::Success,
                counted(s.online, "online", "online"),
            ))
            // Task 13: the count opens the chat at the alert (node 456).
            .child(
                div()
                    .id("status-alerts")
                    .debug_selector(|| "status-alerts".into())
                    .when(self.alerts.alerts > 0, |this| {
                        this.cursor_pointer().on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(OpenAlerts), cx);
                        })
                    })
                    .child(item(
                        "⚠",
                        Color::Warning,
                        counted_capped(self.alerts.alerts, "alert", "alerts"),
                    )),
            )
            .child(item(
                "⎇",
                Color::Placeholder,
                counted(s.worktrees, "worktree", "worktrees"),
            ))
            .child(div().flex_1())
            .child(
                div().debug_selector(|| "status-context".into()).child(
                    Label::new(self.context)
                        .size(LabelSize::Small)
                        .color(Color::Placeholder),
                ),
            )
            .child(
                Label::new(spend_today(s.spend_today_usd))
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
            )
            .when(!self.right_dock_open, |this| {
                this.child(
                    Button::new("listener-toggle", "Listener")
                        .style(ButtonStyle::Filled)
                        .leading(Label::new("◨").size(LabelSize::Small).color(Color::Accent))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ToggleRightDock), cx);
                        }),
                )
            })
            .child(buttons(&[
                ("status-terminal", UiIcon::Terminal),
                ("status-chat", UiIcon::Chat),
                ("status-bell", UiIcon::Bell),
            ]))
            .child(KeyHint::new("⌘⇧P"))
    }
}
