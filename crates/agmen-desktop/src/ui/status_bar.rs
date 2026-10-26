//! The status bar: panel buttons, counts, the listener, spend, the Listener
//! toggle and the right-side buttons.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div,
};

use super::components::{Badge, Button, ButtonStyle, Color, IconButton, KeyHint, Label, LabelSize};
use super::format::{counted, listener_label, spend_today};
use super::tokens::{SPACE_5, SPACE_10, SPACE_14, STATUS_BAR_HEIGHT};
use super::workspace::ToggleRightDock;
use crate::backend::{Snapshot, StatusSummary};
use crate::icons::UiIcon;

#[derive(Debug, IntoElement)]
pub(crate) struct StatusBar {
    status: StatusSummary,
    listener: SharedString,
    right_dock_open: bool,
}

impl StatusBar {
    #[must_use]
    pub(crate) fn new(snapshot: &Snapshot, right_dock_open: bool) -> Self {
        Self {
            status: snapshot.status.clone(),
            listener: listener_label(&snapshot.listener.agent, &snapshot.listener.state).into(),
            right_dock_open,
        }
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

fn buttons(icons: &[(&'static str, UiIcon)]) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(SPACE_10)
        .children(icons.iter().map(|(id, icon)| IconButton::new(*id, *icon)))
}

impl RenderOnce for StatusBar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let s = &self.status;
        div()
            .h(STATUS_BAR_HEIGHT)
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_14)
            .px(SPACE_10)
            .child(buttons(&[
                ("status-project", UiIcon::Project),
                ("status-git", UiIcon::Git),
                ("status-search", UiIcon::Search),
            ]))
            .when(s.needs_you > 0, |this| {
                this.child(
                    Badge::new(counted(s.needs_you, "needs you", "need you"), Color::Error)
                        .solid()
                        .leading("!"),
                )
            })
            .child(item(
                "●",
                Color::Success,
                counted(s.online, "online", "online"),
            ))
            .child(item(
                "⚠",
                Color::Warning,
                counted(s.alerts, "alert", "alerts"),
            ))
            .child(item(
                "⎇",
                Color::Placeholder,
                counted(s.worktrees, "worktree", "worktrees"),
            ))
            .child(div().flex_1())
            .child(
                Label::new(self.listener)
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
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
                ("status-settings", UiIcon::Settings),
            ]))
            .child(KeyHint::new("⌘⇧P"))
    }
}
