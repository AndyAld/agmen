//! What an ALERT's row adds while it needs me (frame "Desktop · needs you",
//! node 5951): the red look, the NEEDS YOU marker after its verb (node 5960)
//! and the buttons under its text (node 5995). A snoozed alert keeps the
//! look, dimmed, without the marker or the buttons (mockup fix 43).
//! `message.rs` draws the row and calls these.

use agmen_proto::{Payload, StampedEnvelope};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _, div,
};

use super::components::{Badge, Button, ButtonStyle, ChangeHandler, Color};
use super::needs_you::{AlertButton, AlertLook, SNOOZE_LABEL, alert_buttons};
use super::tokens::{SNOOZED_OPACITY, SPACE_6};
use crate::backend::Action;

/// How strongly a row with `look` shows.
#[must_use]
pub(super) fn row_opacity(look: Option<AlertLook>) -> f32 {
    if look == Some(AlertLook::Snoozed) {
        SNOOZED_OPACITY
    } else {
        1.
    }
}

/// NEEDS YOU (node 5960). Tests find it as `"{id}:needs-you"`.
pub(super) fn needs_you_marker(id: &str) -> impl IntoElement + use<> {
    let selector = format!("{id}:needs-you");
    div()
        .debug_selector(|| selector)
        .flex_none()
        .child(Badge::new("NEEDS YOU", Color::Error).marker())
}

/// Reply, the sender's actions and Snooze 15 min (node 5995). The message
/// at `index` is `env`. Tests find the row as `"{id}:alert-actions"` and its
/// buttons as `"{id}:alert:reply"`, `"{id}:alert:action:{n}"` and
/// `"{id}:alert:snooze"`.
pub(super) fn alert_actions(
    env: &StampedEnvelope,
    index: usize,
    on_action: Option<&ChangeHandler<Action>>,
    on_reply: Option<&ChangeHandler<usize>>,
) -> impl IntoElement + use<> {
    let id = env.client.id.as_str();
    let offered: &[String] = match &env.client.payload {
        Payload::Alert { actions, .. } => actions,
        _ => &[],
    };
    let answer = |action: Action| {
        on_action.cloned().map(|handler| {
            move |_: &gpui::ClickEvent, window: &mut gpui::Window, cx: &mut gpui::App| {
                handler(action.clone(), window, cx);
            }
        })
    };
    let mut own = 0;
    let buttons: Vec<Button> = alert_buttons(offered)
        .into_iter()
        .map(|button| match button {
            AlertButton::Reply => {
                let reply = on_reply.cloned();
                Button::new(SharedString::from(format!("{id}:alert:reply")), "Reply")
                    .style(ButtonStyle::Danger)
                    .when_some(reply, |this, handler| {
                        this.on_click(move |_, window, cx| handler(index, window, cx))
                    })
            }
            AlertButton::Answer(choice) => {
                let name = SharedString::from(format!("{id}:alert:action:{own}"));
                own += 1;
                let action = Action::AnswerAlert {
                    alert: env.client.id.clone(),
                    choice: choice.clone(),
                };
                Button::new(name, choice)
                    .style(ButtonStyle::Bordered)
                    .when_some(answer(action), Button::on_click)
            }
            AlertButton::Snooze => Button::new(
                SharedString::from(format!("{id}:alert:snooze")),
                SNOOZE_LABEL,
            )
            .style(ButtonStyle::Bordered)
            .when_some(
                answer(Action::SnoozeAlert(env.client.id.clone())),
                Button::on_click,
            ),
        })
        .collect();
    let selector = format!("{id}:alert-actions");
    div()
        .debug_selector(|| selector)
        .flex()
        .flex_wrap()
        .items_center()
        .gap(SPACE_6)
        .children(buttons)
}

#[cfg(test)]
#[path = "alert_row_tests.rs"]
mod tests;
