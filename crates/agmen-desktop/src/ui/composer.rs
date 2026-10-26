//! The chat's composer (frame "Desktop · chat", node 374): the "to" chip,
//! the model chip, the input and the ⏎ hint. An agent's view uses it for a
//! DM, with neither chip (frame "Desktop · agent", node 650).

use std::rc::Rc;

use agmen_proto::{IdentityId, MessageId};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, anchored, deferred, div,
};

use super::components::{Chip, Color, IconButton, IconSize, Label, LabelSize};
use super::model_picker::{ModelMenu, PickerEvent};
use super::text_input::{TextInput, TextInputEvent};
use super::tokens::{
    AUTO_LINE_13, BORDER_WIDTH, MODEL_PICKER_OFFSET, RADIUS_INPUT, SPACE_8, SPACE_10, SPACE_12,
    SPACE_14, TEXT_DEFAULT,
};
use crate::appearance::Appearance;
use crate::backend::{BackendError, Draft, Snapshot};
use crate::icons::UiIcon;
use crate::keymap::Dismiss;

/// The input's placeholder (node 384).
const PLACEHOLDER: &str = "Ask or tell your listener · @ to mention someone else · ⏎ send";

/// How many lines the input grows to before it scrolls.
const MAX_INPUT_LINES: usize = 6;

/// What the composer tells its owner, besides sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerEvent {
    /// The model chip opened the model picker.
    PickModel,
}

/// How the composer picks who a message goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressing {
    /// The "to" chip names the target, which a leading `@mention` or a
    /// reply changes (node 375).
    Chip,
    /// A DM: no chip, and every message goes to the recipient (node 650).
    Fixed,
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
    addressing: Addressing,
    /// The model chip and its picker (Task 15); `None` draws no chip.
    models: Option<ModelMenu>,
    /// The message the next send replies to, and its sender (Task 13).
    reply: Option<(MessageId, Target)>,
    on_send: SendHandler,
    _subscriptions: [Subscription; 2],
}

impl std::fmt::Debug for Composer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Composer")
            .field("recipient", &self.recipient)
            .field("addressing", &self.addressing)
            .field("models", &self.models)
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
        Self::build(
            agents,
            listener,
            PLACEHOLDER.into(),
            Some(ModelMenu::new(snapshot)),
            Rc::new(on_send),
            window,
            cx,
        )
    }

    /// A DM composer for `agent`'s view (node 650): it draws no "to" chip
    /// and every message goes to `agent`, so an `@mention` stays text in
    /// the DM. It draws no model chip, since the agent runs its own model.
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
        Self {
            addressing: Addressing::Fixed,
            ..Self::build(
                roster_targets(snapshot),
                agent,
                placeholder.into(),
                None,
                Rc::new(on_send),
                window,
                cx,
            )
        }
    }

    fn build(
        agents: Vec<Target>,
        recipient: Target,
        placeholder: SharedString,
        models: Option<ModelMenu>,
        on_send: SendHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextInput::new(cx)
                .line_height(AUTO_LINE_13)
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
            addressing: Addressing::Chip,
            models,
            reply: None,
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
    /// recipient, or to the sender of the message it replies to.
    fn target_for(&self, text: &str) -> &Target {
        self.mentioned(text)
            .or(self.reply.as_ref().map(|(_, to)| to))
            .unwrap_or(&self.recipient)
    }

    /// The roster agent an `@mention` at the start of `text` names; none
    /// in a DM, which has one fixed recipient.
    fn mentioned(&self, text: &str) -> Option<&Target> {
        if self.addressing == Addressing::Fixed {
            return None;
        }
        let handle = mention_handle(text)?;
        self.agents.iter().find(|agent| agent.handle == handle)
    }

    /// The message `text` replies to: the pending reply, unless a leading
    /// `@mention` names someone other than its sender (C232). A reply sent
    /// to another agent would be an ANSWER that closes the alert while its
    /// sender never sees it.
    fn reply_for(&self, text: &str) -> Option<&MessageId> {
        let (message, sender) = self.reply.as_ref()?;
        self.mentioned(text)
            .is_none_or(|mentioned| mentioned.id == sender.id)
            .then_some(message)
    }

    fn send(&mut self, body: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Blank text is not a message; the input keeps it, as if Enter had
        // not been pressed.
        if body.trim().is_empty() {
            self.input
                .update(cx, |input, cx| input.set_text(body, window, cx));
            return;
        }
        let target = self.target_for(body);
        // Task 15: the model choice is for the recipient; an @mention of
        // another agent runs on that agent's own model.
        let to_recipient = target.id == self.recipient.id;
        let draft = Draft {
            to: vec![target.id.clone()],
            body: body.to_owned(),
            reply_to: self.reply_for(body).cloned(),
            model: self
                .models
                .as_ref()
                .filter(|_| to_recipient)
                .and_then(ModelMenu::outgoing),
        };
        // The input cleared itself on Enter; a refused draft is not lost.
        // The caller shows why it was refused.
        if (self.on_send)(draft, window, cx).is_err() {
            self.input
                .update(cx, |input, cx| input.set_text(body, window, cx));
        } else {
            self.reply = None;
            if let Some(models) = self.models.as_mut().filter(|_| to_recipient) {
                // Task 15: a "this message" choice ends with the message.
                models.sent();
            }
            cx.notify();
        }
    }

    // Task 13: an alert's Reply.
    /// Starts a reply to `message`, addressed to `to`, and focuses the
    /// input. The next message sent replies to it; an `@mention` of
    /// another agent sends a plain message to that agent instead.
    pub(crate) fn reply(
        &mut self,
        to: Target,
        message: MessageId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reply = Some((message, to));
        self.input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Ends the reply: the next message goes to the recipient again. Escape
    /// and the "reply to" chip's close mark call it.
    pub(crate) fn cancel_reply(&mut self, cx: &mut Context<Self>) {
        if self.reply.take().is_some() {
            cx.notify();
        }
    }

    /// Ends the reply unless `open` says its message still waits for an
    /// answer, so a reply to an alert someone else answered is not sent.
    pub(crate) fn drop_reply_unless(
        &mut self,
        open: impl Fn(&MessageId) -> bool,
        cx: &mut Context<Self>,
    ) {
        if self
            .reply
            .as_ref()
            .is_some_and(|(message, _)| !open(message))
        {
            self.cancel_reply(cx);
        }
    }

    // Task 15: the model chip's choice and its picker.
    /// The model the next message runs on, as the chip shows it; `None` on
    /// a DM composer, which has no model chip.
    #[cfg(test)]
    pub(crate) fn model_choice(&self) -> Option<&crate::backend::ModelChoice> {
        self.models.as_ref().map(ModelMenu::current)
    }

    /// Takes a new snapshot's installed agents, models and listener.
    pub(crate) fn set_snapshot(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        if let Some(models) = &mut self.models {
            models.refresh(snapshot);
            cx.notify();
        }
    }

    /// Opens or closes the model picker.
    pub(crate) fn set_model_picker_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if let Some(models) = &mut self.models
            && models.is_open() != open
        {
            models.set_open(open);
            if open {
                cx.emit(ComposerEvent::PickModel);
            }
            cx.notify();
        }
    }

    fn on_pick(&mut self, event: &PickerEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(models) = &mut self.models {
            models.apply(event);
            if event.closes() {
                models.set_open(false);
                // A click on an effort level focuses it; typing goes back
                // to the input.
                self.input.focus_handle(cx).focus(window, cx);
            }
            cx.notify();
        }
    }

    /// The text typed so far.
    #[cfg(test)]
    pub(crate) fn text<'a>(&self, cx: &'a App) -> &'a str {
        self.input.read(cx).text()
    }

    /// The model chip (node 380) and, while open, the picker above it. The
    /// picker's bottom left corner sits [`MODEL_PICKER_OFFSET`] from the
    /// chip's top left.
    fn model_chip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let models = self.models.as_ref()?;
        let open = models.is_open();
        let entity = cx.entity().downgrade();
        let picker = open.then(|| {
            let on_event = entity.clone();
            let on_close = entity.clone();
            let picker = models.picker(move |event, window, cx| {
                // The picker lives inside the composer's frame.
                on_event
                    .update(cx, |this, cx| this.on_pick(&event, window, cx))
                    .unwrap_or_default();
            });
            deferred(
                anchored()
                    .anchor(Anchor::BottomLeft)
                    .offset(MODEL_PICKER_OFFSET)
                    .snap_to_window()
                    .child(
                        div()
                            .on_mouse_down_out(move |_, _, cx| {
                                on_close
                                    .update(cx, |this, cx| this.set_model_picker_open(false, cx))
                                    .unwrap_or_default();
                            })
                            .child(picker),
                    ),
            )
        });
        Some(
            div()
                .id("composer-model")
                .debug_selector(|| "composer:model".into())
                .relative()
                // A narrow box clips the chip rather than letting it run
                // past its edge; the picker is deferred, so not clipped.
                .min_w_0()
                .overflow_hidden()
                .cursor_pointer()
                // C81: while open, mouse down outside the picker closes it;
                // a click here too would reopen it on mouse up.
                .when(!open, |this| {
                    this.on_click(cx.listener(|this, _, _, cx| {
                        this.set_model_picker_open(true, cx);
                    }))
                })
                .child(
                    Chip::new(models.current().label())
                        .compact()
                        .code()
                        .color(Color::Default)
                        .outline(Color::Accent)
                        .leading(Label::new("◆").size(LabelSize::XSmall).color(Color::Accent))
                        .trailing(caret()),
                )
                .when_some(picker, |this, picker| {
                    this.child(div().absolute().top_0().left_0().child(picker))
                }),
        )
    }
}

impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model_chip(cx);
        let theme = &cx.global::<Appearance>().theme;
        // Task 13: a pending reply says so and offers a way out. An
        // @mention of another agent drops it, and the chip says "to".
        let replying = self.reply_for(self.input.read(cx).text()).is_some();
        let to = (self.addressing == Addressing::Chip).then(|| {
            div()
                .debug_selector(|| "composer:to".into())
                .min_w_0()
                .overflow_hidden()
                .child(
                    Chip::new(self.target(cx).handle.clone())
                        .compact()
                        .code()
                        .color(Color::Accent)
                        .leading(
                            Label::new(if replying { "reply to" } else { "to" })
                                .size(LabelSize::Small)
                                .color(Color::Placeholder),
                        )
                        .map(|chip| {
                            if replying {
                                chip.trailing(
                                    IconButton::new("composer:cancel-reply", UiIcon::Close)
                                        .size(IconSize::Small)
                                        .color(Color::Placeholder)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.cancel_reply(cx)),
                                        ),
                                )
                            } else {
                                chip.trailing(caret())
                            }
                        }),
                )
        });
        div()
            .w_full()
            .flex_none()
            // Task 13 and Task 15: Escape closes the open model picker and
            // goes no further (C87, C113), else ends a reply; src/keymap.rs
            // binds it.
            .key_context(crate::keymap::COMPOSER_CONTEXT)
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                if this.models.as_ref().is_some_and(ModelMenu::is_open) {
                    this.set_model_picker_open(false, cx);
                } else if this.reply.is_some() {
                    this.cancel_reply(cx);
                } else {
                    cx.propagate();
                }
            }))
            .pt(SPACE_8)
            .px(SPACE_14)
            .pb(SPACE_12)
            .child(
                div()
                    .debug_selector(|| "composer:box".into())
                    .w_full()
                    // Node 375 clips: a box too narrow for its chips cuts
                    // them at its edge.
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap(SPACE_8)
                    .py(SPACE_8)
                    .px(SPACE_10)
                    .rounded(RADIUS_INPUT)
                    .border(BORDER_WIDTH)
                    .border_color(theme.border)
                    .bg(theme.elevated_surface)
                    .children(to)
                    .children(model)
                    .child(
                        div()
                            .debug_selector(|| "composer:input".into())
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .text_size(TEXT_DEFAULT)
                            .child(self.input.clone()),
                    )
                    // The UI font at body size, as node 385 draws it.
                    .child(
                        div()
                            .debug_selector(|| "composer:send-hint".into())
                            .flex_none()
                            .child(Label::new("⏎").color(Color::Muted)),
                    ),
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
