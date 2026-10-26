//! The answer buttons under an ASK addressed to me (plan Task 15). A click
//! sends an ANSWER whose `reply_to` names the ASK, to the identity that
//! asked. Once anyone answers it, the card says so instead.
//!
//! The protocol's ASK carries no options (`Payload::Ask { of }` in
//! agmen-proto), so the card offers "Yes" and "No"; any other answer is
//! typed in the composer.

use std::rc::Rc;

use agmen_proto::{Payload, StampedEnvelope, Verb};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use super::components::{Button, ButtonStyle, ChangeHandler, Color, Label, LabelSize};
use super::tokens::SPACE_6;
use crate::backend::{Draft, Snapshot};

/// The answers the card offers, the first drawn as the primary button.
const ANSWERS: [&str; 2] = ["Yes", "No"];

/// Whether `env` is an ASK for the person: its `of` or its `to` names them.
#[must_use]
pub(crate) fn asks_me(env: &StampedEnvelope, snapshot: &Snapshot) -> bool {
    let me = &snapshot.me_id;
    match &env.client.payload {
        Payload::Ask { of } => of.as_ref() == Some(me) || env.client.to.contains(me),
        _ => false,
    }
}

/// Whether an ANSWER in the chat replies to `ask`.
fn answered(ask: &StampedEnvelope, snapshot: &Snapshot) -> bool {
    snapshot.messages.iter().any(|m| {
        m.client.reply_to.as_ref() == Some(&ask.client.id)
            && m.client.payload.verb() == Some(Verb::Answer)
    })
}

/// The ANSWER that `choice` sends to `ask`.
#[must_use]
pub(crate) fn answer(ask: &StampedEnvelope, choice: &str) -> Draft {
    Draft {
        to: vec![ask.from.clone()],
        body: choice.to_owned(),
        reply_to: Some(ask.client.id.clone()),
        model: None,
    }
}

/// Tests find a button as `"{message id}:answer:{i}"` and the answered
/// note as `"{message id}:answered"`.
#[derive(IntoElement)]
pub(crate) struct AskCard {
    id: SharedString,
    answers: Vec<Draft>,
    answered: bool,
    on_answer: Option<ChangeHandler<Draft>>,
}

impl std::fmt::Debug for AskCard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskCard")
            .field("id", &self.id)
            .field("answered", &self.answered)
            .finish_non_exhaustive()
    }
}

impl AskCard {
    /// The card for `env`, when it is an ASK addressed to me.
    #[must_use]
    pub(crate) fn for_message(env: &StampedEnvelope, snapshot: &Snapshot) -> Option<Self> {
        asks_me(env, snapshot).then(|| Self {
            id: env.client.id.as_str().to_owned().into(),
            answers: ANSWERS.iter().map(|choice| answer(env, choice)).collect(),
            answered: answered(env, snapshot),
            on_answer: None,
        })
    }

    /// Called with the ANSWER a button sends; the caller passes it to
    /// `Backend::send`.
    #[must_use]
    pub(crate) fn on_answer(mut self, handler: ChangeHandler<Draft>) -> Self {
        self.on_answer = Some(handler);
        self
    }
}

impl RenderOnce for AskCard {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let row = div().flex().flex_wrap().items_center().gap(SPACE_6);
        if self.answered {
            let selector = format!("{}:answered", self.id);
            return row.child(
                div().debug_selector(|| selector).child(
                    Label::new("answered")
                        .size(LabelSize::Small)
                        .color(Color::Placeholder),
                ),
            );
        }
        let id = self.id;
        let on_answer = self.on_answer;
        row.children(self.answers.into_iter().enumerate().map(|(i, draft)| {
            let style = if i == 0 {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            };
            let label = draft.body.clone();
            Button::new(SharedString::from(format!("{id}:answer:{i}")), label)
                .style(style)
                .when_some(on_answer.clone(), |button, handler| {
                    let handler = Rc::clone(&handler);
                    button.on_click(move |_, window, cx| handler(draft.clone(), window, cx))
                })
        }))
    }
}

#[cfg(test)]
#[path = "ask_card_tests.rs"]
mod tests;
