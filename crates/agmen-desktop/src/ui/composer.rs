//! The chat's composer (frame "Desktop · chat", node 374): the "to" chip,
//! the model chip, the input and the ⏎ hint. An agent's view uses it for a
//! DM, without the model chip.

use std::rc::Rc;

use agmen_proto::IdentityId;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
};

use super::components::{Chip, Color, Label, LabelSize};
use super::text_input::{TextInput, TextInputEvent};
use super::tokens::{
    BORDER_WIDTH, RADIUS_INPUT, SPACE_8, SPACE_10, SPACE_12, SPACE_14, TEXT_DEFAULT,
};
use crate::appearance::Appearance;
use crate::backend::{BackendError, Draft, Snapshot};

/// The input's placeholder (node 384). The ⏎ hint after the input already
/// says how to send, so the placeholder does not repeat it.
const PLACEHOLDER: &str = "Ask or tell your listener · @ to mention someone else";

/// How many lines the input grows to before it scrolls.
const MAX_INPUT_LINES: usize = 6;

/// What the composer tells its owner, besides sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerEvent {
    /// The model chip was clicked; Task 15 opens the picker.
    PickModel,
}

/// Who a message goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) id: IdentityId,
    pub(crate) handle: String,
}

/// What the composer does with a message: the caller passes it to
/// `Backend::send`. A refused draft comes back into the input.
pub(crate) type SendHandler = Rc<dyn Fn(Draft, &mut Window, &mut App) -> Result<(), BackendError>>;

/// The handle an `@mention` at the start of `text` names, without trailing
/// punctuation: `@andy/fx1, go` names `andy/fx1`.
fn mention_handle(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix('@')?;
    let token = rest.split_whitespace().next()?;
    // A mention at the start of the text begins right after the @.
    if !rest.starts_with(token) {
        return None;
    }
    let handle = token.trim_end_matches(|c: char| c.is_ascii_punctuation() && c != '/');
    (!handle.is_empty()).then_some(handle)
}

pub(crate) struct Composer {
    input: Entity<TextInput>,
    /// Who a message without an `@mention` goes to: the listener, or the
    /// agent a DM composer is for.
    recipient: Target,
    /// Every agent in the roster, which an `@mention` can address.
    agents: Vec<Target>,
    /// The model chip's label; `None` draws no chip.
    model: Option<SharedString>,
    on_send: SendHandler,
    _subscriptions: [Subscription; 2],
}

impl std::fmt::Debug for Composer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Composer")
            .field("recipient", &self.recipient)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    /// A composer addressed to the snapshot's listener. Enter calls
    /// `on_send` with the draft; when it fails, the text comes back.
    pub(crate) fn new(
        snapshot: &Snapshot,
        on_send: impl Fn(Draft, &mut Window, &mut App) -> Result<(), BackendError> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let agents = roster_targets(snapshot);
        // Task 19: a project with a plan talks to its planner, with no model
        // chip: the planner runs its own (frame "Desktop · plan", node 8927).
        if let Some((planner, placeholder)) = super::plan_room::planner_target(snapshot) {
            return Self::build(
                agents,
                planner,
                placeholder.into(),
                None,
                Rc::new(on_send),
                window,
                cx,
            );
        }
        let listener = agents
            .iter()
            .find(|agent| agent.id == snapshot.listener_id)
            .cloned()
            .unwrap_or_else(|| Target {
                id: snapshot.listener_id.clone(),
                handle: snapshot.listener.handle.clone(),
            });
        // shortcut: the listener's harness with its configured model;
        // Task 15 adds models to the snapshot and the picker.
        let model = format!("{} · default", snapshot.listener.agent).into();
        Self::build(
            agents,
            listener,
            PLACEHOLDER.into(),
            Some(model),
            Rc::new(on_send),
            window,
            cx,
        )
    }

    /// A DM composer for `agent`'s view (`desktop-agent.webp`): its "to"
    /// chip names the agent and it draws no model chip, since the agent
    /// runs its own model. An `@mention` still addresses another agent.
    pub(crate) fn direct(
        snapshot: &Snapshot,
        agent: Target,
        on_send: impl Fn(Draft, &mut Window, &mut App) -> Result<(), BackendError> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = format!(
            "Message {} · sent as a DM, recorded by the hub",
            agent.handle
        );
        Self::build(
            roster_targets(snapshot),
            agent,
            placeholder.into(),
            None,
            Rc::new(on_send),
            window,
            cx,
        )
    }

    fn build(
        agents: Vec<Target>,
        recipient: Target,
        placeholder: SharedString,
        model: Option<SharedString>,
        on_send: SendHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextInput::new(cx)
                .placeholder(placeholder)
                .multiline(MAX_INPUT_LINES)
        });
        let submits = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            let TextInputEvent::Submit(body) = event;
            this.send(body, window, cx);
        });
        // The "to" chip follows an @mention as it is typed.
        let edits = cx.observe(&input, |_, _, cx| cx.notify());
        Self {
            input,
            recipient,
            agents,
            model,
            on_send,
            _subscriptions: [submits, edits],
        }
    }

    /// Who the text typed so far goes to: the agent an `@mention` at its
    /// start names, or the recipient.
    pub(crate) fn target(&self, cx: &App) -> &Target {
        self.target_for(self.input.read(cx).text())
    }

    /// Unknown handles stay plain text, and the message goes to the
    /// recipient.
    fn target_for(&self, text: &str) -> &Target {
        mention_handle(text)
            .and_then(|handle| self.agents.iter().find(|agent| agent.handle == handle))
            .unwrap_or(&self.recipient)
    }

    fn send(&self, body: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Blank text is not a message; the input keeps it, as if Enter had
        // not been pressed.
        if body.trim().is_empty() {
            self.input
                .update(cx, |input, cx| input.set_text(body, window, cx));
            return;
        }
        let draft = Draft {
            to: vec![self.target_for(body).id.clone()],
            body: body.to_owned(),
            // shortcut: no reply target yet; Task 13's Reply button sets one.
            reply_to: None,
        };
        // The input cleared itself on Enter; a refused draft is not lost.
        // The caller shows why it was refused.
        if (self.on_send)(draft, window, cx).is_err() {
            self.input
                .update(cx, |input, cx| input.set_text(body, window, cx));
        }
    }
}

impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let to = div()
            .debug_selector(|| "composer:to".into())
            .flex_none()
            .child(
                Chip::new(self.target(cx).handle.clone())
                    .compact()
                    .code()
                    .color(Color::Accent)
                    .leading(
                        Label::new("to")
                            .size(LabelSize::Small)
                            .color(Color::Placeholder),
                    )
                    .trailing(caret()),
            );
        let model = self.model.clone().map(|model| {
            div()
                .id("composer-model")
                .debug_selector(|| "composer:model".into())
                .flex_none()
                .cursor_pointer()
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ComposerEvent::PickModel)))
                .child(
                    Chip::new(model)
                        .compact()
                        .code()
                        .color(Color::Default)
                        .outline(Color::Accent)
                        .leading(Label::new("◆").size(LabelSize::XSmall).color(Color::Accent))
                        .trailing(caret()),
                )
        });
        div()
            .w_full()
            .flex_none()
            .pt(SPACE_8)
            .px(SPACE_14)
            .pb(SPACE_12)
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(SPACE_8)
                    .py(SPACE_8)
                    .px(SPACE_10)
                    .rounded(RADIUS_INPUT)
                    .border(BORDER_WIDTH)
                    .border_color(theme.border)
                    .bg(theme.elevated_surface)
                    .child(to)
                    .children(model)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .text_size(TEXT_DEFAULT)
                            .child(self.input.clone()),
                    )
                    // The UI font at body size, as node 385 draws it.
                    .child(Label::new("⏎").color(Color::Muted)),
            )
    }
}

/// Every agent in the snapshot's roster, as an `@mention` can address it.
fn roster_targets(snapshot: &Snapshot) -> Vec<Target> {
    snapshot
        .roster
        .iter()
        .flat_map(|group| &group.agents)
        .map(|agent| Target {
            id: agent.id.clone(),
            handle: agent.handle.clone(),
        })
        .collect()
}

/// The ▾ after a chip's label.
fn caret() -> Label {
    Label::new("▾")
        .size(LabelSize::XSmall)
        .color(Color::Placeholder)
}

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;
