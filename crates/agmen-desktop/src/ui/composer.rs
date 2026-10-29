//! The chat's composer (frame "Desktop · chat", node 374): the "to" chip,
//! the model chip, the input and the ⏎ hint. An agent's view uses it for a
//! DM, with neither chip (frame "Desktop · agent", node 650).

use std::cell::Cell;
use std::rc::Rc;

use agmen_proto::{IdentityId, MessageId};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, ScrollHandle,
    SharedString, Styled as _, Subscription, TextRun, Window, anchored, canvas, deferred, div, px,
};

use super::components::{
    Chip, ChipButton, Color, IconButton, IconSize, Label, LabelSize, Menu, MenuRow,
};
use super::ink_clip::InkClip;
use super::model_picker::{ModelMenu, PickerEvent};
use super::text_input::{TextInput, TextInputEvent};
use super::tokens::{
    AUTO_LINE_12, AUTO_LINE_13, BORDER_WIDTH, COMPOSER_INPUT_MIN_WIDTH, DROPDOWN_WIDTH, ICON_SM,
    MODEL_PICKER_OFFSET, RADIUS_INPUT, SPACE_8, SPACE_10, SPACE_12, SPACE_14, TEXT_DEFAULT,
};
use crate::appearance::{Appearance, Fonts};
use crate::backend::{BackendError, Draft, Snapshot};
use crate::icons::UiIcon;
use crate::keymap::{CompleteMention, Dismiss};

/// The input's placeholder (node 384).
const PLACEHOLDER: &str = "Ask or tell your listener · @ to mention someone else · ⏎ send";

/// Task 7: the placeholder and the "to" chip while the person has no
/// listener, so a message goes to the project chat with no mention.
const PLACEHOLDER_NO_LISTENER: &str = "Message the project chat · @ to mention someone · ⏎ send";
const EVERYONE: &str = "everyone";

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
    let handle = trim_handle_punctuation(token);
    (!handle.is_empty()).then_some(handle)
}

fn trim_handle_punctuation(token: &str) -> &str {
    token.trim_end_matches(|c: char| c.is_ascii_punctuation() && c != '/')
}

pub(crate) struct Composer {
    input: Entity<TextInput>,
    /// Who a message without an `@mention` goes to: the listener, or the
    /// agent a DM composer is for. Task 7: `None` without a listener, so
    /// the message goes to the chat as a whole and mentions no one.
    recipient: Option<Target>,
    /// Task 7: whether the recipient is the snapshot's listener, which a
    /// new snapshot can bring, change or take away.
    follows_listener: bool,
    /// Every person and agent in the project roster.
    agents: Vec<Target>,
    /// An explicit audience, including Everyone, persists across sends.
    picked_recipient: Option<Option<Target>>,
    /// A picker click overrides an existing mention in this draft only.
    picked_for_draft: bool,
    audience_open: bool,
    mention_selected: usize,
    mention_scroll: ScrollHandle,
    mentions_dismissed: bool,
    /// The next input change is a completion's own edit, which keeps the
    /// list closed.
    completing: bool,
    addressing: Addressing,
    /// The model chip and its picker (Task 15); `None` draws no chip.
    models: Option<ModelMenu>,
    /// The message the next send replies to, and its sender (Task 13).
    reply: Option<(MessageId, Target)>,
    /// The box's width in the last frame, which decides whether the model
    /// chip fits (C278); `None` until the box is first laid out.
    box_width: Rc<Cell<Option<Pixels>>>,
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
    fn mention_query<'a>(&self, cx: &'a App) -> Option<(std::ops::Range<usize>, &'a str)> {
        if self.mentions_dismissed || self.audience_open {
            return None;
        }
        let input = self.input.read(cx);
        let cursor = input.completion_cursor()?;
        let before = &input.text()[..cursor];
        let start = before.rfind(char::is_whitespace).map_or(0, |at| {
            at + before[at..].chars().next().map_or(0, char::len_utf8)
        });
        let prefix = before[start..].strip_prefix('@')?;
        let end = input.text()[cursor..]
            .find(char::is_whitespace)
            .map_or(input.text().len(), |at| cursor + at);
        let end = start + 1 + trim_handle_punctuation(&input.text()[start + 1..end]).len();
        Some((start..end, prefix))
    }

    fn mention_candidates(&self, cx: &App) -> Vec<&Target> {
        let Some((_, prefix)) = self.mention_query(cx) else {
            return Vec::new();
        };
        self.agents
            .iter()
            .filter(|t| t.handle.starts_with(prefix))
            .collect()
    }

    fn move_mention(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.mention_candidates(cx).len();
        if count == 0 {
            cx.propagate();
            return;
        }
        self.mention_selected = if down {
            (self.mention_selected + 1) % count
        } else {
            (self.mention_selected + count - 1) % count
        };
        self.mention_scroll.scroll_to_item(self.mention_selected);
        cx.stop_propagation();
        cx.notify();
    }

    fn complete_mention(
        &mut self,
        target: Option<&Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((mut range, _)) = self.mention_query(cx) else {
            return false;
        };
        let candidates = self.mention_candidates(cx);
        let Some(target) = target.or_else(|| candidates.get(self.mention_selected).copied()) else {
            return false;
        };
        let suffix = &self.input.read(cx).text()[range.end..];
        let space = suffix.chars().next().map_or_else(
            || " ".to_owned(),
            |c| {
                if c.is_whitespace() {
                    range.end += c.len_utf8();
                    c.to_string()
                } else {
                    String::new()
                }
            },
        );
        let text = format!("@{}{space}", target.handle);
        self.completing = self.input.update(cx, |input, cx| {
            input.replace_bytes(range, &text, window, cx)
        });
        self.mentions_dismissed = true;
        self.picked_for_draft = false;
        self.input.focus_handle(cx).focus(window, cx);
        cx.notify();
        true
    }

    fn target_menu(&self, mentions: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let id = if mentions {
            "composer:mentions"
        } else {
            "composer:audience"
        };
        let targets: Vec<Option<Target>> = if mentions {
            self.mention_candidates(cx)
                .into_iter()
                .cloned()
                .map(Some)
                .collect()
        } else {
            std::iter::once(None)
                .chain(self.agents.iter().cloned().map(Some))
                .collect()
        };
        let rows = targets
            .into_iter()
            .enumerate()
            .map(|(index, target)| {
                let key = target.as_ref().map_or("everyone", |t| t.id.as_str());
                let label = target
                    .as_ref()
                    .map_or("Everyone", |t| t.handle.as_str())
                    .to_owned();
                let checked = if mentions {
                    index == self.mention_selected
                } else {
                    self.target(cx) == target.as_ref()
                };
                MenuRow::new(format!("{id}:{key}"))
                    .picker()
                    .checked(checked)
                    .child(Label::new(label).code())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if mentions {
                            this.complete_mention(target.as_ref(), window, cx);
                        } else {
                            this.picked_recipient = Some(target.clone());
                            this.picked_for_draft = true;
                            this.reply = None;
                            this.audience_open = false;
                            this.input.focus_handle(cx).focus(window, cx);
                            cx.notify();
                        }
                    }))
            })
            .collect::<Vec<_>>();
        let entity = cx.entity().downgrade();
        Menu::new(id)
            .width(DROPDOWN_WIDTH)
            .when(mentions, |menu| menu.track_scroll(&self.mention_scroll))
            .place(Anchor::BottomLeft, MODEL_PICKER_OFFSET)
            .children(rows)
            .on_close(move |window, cx| {
                entity
                    .update(cx, |this, cx| {
                        if mentions {
                            this.mentions_dismissed = true;
                        } else {
                            this.audience_open = false;
                        }
                        this.input.focus_handle(cx).focus(window, cx);
                        cx.notify();
                    })
                    .unwrap_or_default();
            })
    }

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
                Some(planner),
                placeholder.into(),
                None,
                Rc::new(on_send),
                window,
                cx,
            );
        }
        let listener = listener_target(snapshot);
        let (placeholder, models) = match listener {
            Some(_) => (PLACEHOLDER, Some(ModelMenu::new(snapshot))),
            None => (PLACEHOLDER_NO_LISTENER, None),
        };
        Self {
            follows_listener: true,
            ..Self::build(
                agents,
                listener,
                placeholder.into(),
                models,
                Rc::new(on_send),
                window,
                cx,
            )
        }
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
                Some(agent),
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
        recipient: Option<Target>,
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
        let edits = cx.observe(&input, |this, _, cx| {
            if std::mem::take(&mut this.completing) {
                cx.notify();
                return;
            }
            this.mention_selected = 0;
            this.mention_scroll.scroll_to_item(0);
            this.mentions_dismissed = false;
            cx.notify();
        });
        Self {
            input,
            recipient,
            follows_listener: false,
            agents,
            picked_recipient: None,
            picked_for_draft: false,
            audience_open: false,
            mention_selected: 0,
            mention_scroll: ScrollHandle::new(),
            mentions_dismissed: false,
            completing: false,
            addressing: Addressing::Chip,
            models,
            reply: None,
            box_width: Rc::default(),
            on_send,
            _subscriptions: [submits, edits],
        }
    }

    /// Who the text typed so far goes to: the agent an `@mention` at its
    /// start names, or the recipient; `None` for the chat as a whole.
    pub(crate) fn target(&self, cx: &App) -> Option<&Target> {
        self.target_for(self.input.read(cx).text())
    }

    /// What the "to" chip names: the target's handle, or "everyone".
    fn target_label(&self, cx: &App) -> String {
        self.target(cx)
            .map_or_else(|| EVERYONE.to_owned(), |t| t.handle.clone())
    }

    /// Unknown handles stay plain text, and the message goes to the
    /// recipient, or to the sender of the message it replies to.
    fn target_for(&self, text: &str) -> Option<&Target> {
        if self.picked_for_draft {
            return self.picked_recipient.as_ref().and_then(Option::as_ref);
        }
        self.mentioned(text)
            .or(self.reply.as_ref().map(|(_, to)| to))
            .or(match &self.picked_recipient {
                Some(picked) => picked.as_ref(),
                None => self.recipient.as_ref(),
            })
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
        let to_recipient = target.map(|t| &t.id) == self.recipient.as_ref().map(|r| &r.id);
        let draft = Draft {
            to: target.map(|t| vec![t.id.clone()]).unwrap_or_default(),
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
            self.picked_for_draft = false;
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
        self.picked_for_draft = false;
        self.input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    // Task 6: a post the live backend refused after the input cleared.
    /// Puts a refused message's `body` back, with the `reply` it was
    /// (message and sender), when the input is empty. Text typed since
    /// stays as it is, with its own reply state: the person's newer words
    /// win, and the error above the composer still says what was lost.
    pub(crate) fn restore(
        &mut self,
        body: &str,
        reply: Option<(MessageId, Target)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input.read(cx).text().is_empty() {
            return;
        }
        self.input
            .update(cx, |input, cx| input.set_text(body, window, cx));
        if reply.is_some() {
            self.reply = reply;
        }
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

    /// Takes a new snapshot's installed agents, models and listener. Task 7:
    /// a composer addressed to the listener follows it, since a live chat
    /// can open before the hub sends the roster; the roster's agents become
    /// what an `@mention` can address.
    pub(crate) fn set_snapshot(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        // A refresh keeps the highlighted identity, so Tab completes the
        // row the person chose.
        let selected = self
            .mention_candidates(cx)
            .get(self.mention_selected)
            .map(|t| t.id.clone());
        self.agents = roster_targets(snapshot);
        self.mention_selected = selected
            .and_then(|id| self.mention_candidates(cx).iter().position(|t| t.id == id))
            .unwrap_or(0);
        self.mention_scroll.scroll_to_item(self.mention_selected);
        if let Some(Some(picked)) = &self.picked_recipient {
            self.picked_recipient = Some(self.agents.iter().find(|t| t.id == picked.id).cloned());
        }
        if self.follows_listener {
            let listener = listener_target(snapshot);
            if listener.is_some() != self.recipient.is_some() {
                let placeholder = if listener.is_some() {
                    PLACEHOLDER
                } else {
                    PLACEHOLDER_NO_LISTENER
                };
                self.input
                    .update(cx, |input, cx| input.set_placeholder(placeholder, cx));
            }
            self.models = match (self.models.take(), &listener) {
                (Some(mut models), Some(_)) => {
                    models.refresh(snapshot);
                    Some(models)
                }
                (None, Some(_)) => Some(ModelMenu::new(snapshot)),
                (_, None) => None,
            };
            self.recipient = listener;
        } else if let Some(models) = &mut self.models {
            models.refresh(snapshot);
        }
        cx.notify();
    }

    /// Opens the model picker.
    pub(crate) fn open_model_picker(&mut self, cx: &mut Context<Self>) {
        if let Some(models) = &mut self.models
            && !models.is_open()
        {
            models.set_open(true);
            cx.emit(ComposerEvent::PickModel);
            cx.notify();
        }
    }

    /// Closes the model picker. Every close comes here: an effort level
    /// takes focus on mouse down (Tab reaches it too) and its focus goes
    /// with the picker, so focus inside the picker goes back to the input,
    /// or Escape would no longer reach the composer and typing would go
    /// nowhere (C283).
    fn close_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(models) = &mut self.models
            && models.is_open()
        {
            // The last frame still drew the picker, so its context is on
            // the focused element's path while focus is inside it.
            let focus_inside = window
                .context_stack()
                .iter()
                .any(|context| context.contains(crate::keymap::MODEL_PICKER_CONTEXT));
            models.set_open(false);
            if focus_inside {
                self.input.focus_handle(cx).focus(window, cx);
            }
            cx.notify();
        }
    }

    fn on_pick(&mut self, event: &PickerEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(models) = &mut self.models {
            models.apply(event);
            cx.notify();
            if event.closes() {
                self.close_model_picker(window, cx);
            }
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
    fn model_chip(
        &self,
        floor: Pixels,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
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
                            .key_context(crate::keymap::MODEL_PICKER_CONTEXT)
                            .on_mouse_down_out(move |_, window, cx| {
                                on_close
                                    .update(cx, |this, cx| this.close_model_picker(window, cx))
                                    .unwrap_or_default();
                            })
                            .child(picker),
                    ),
            )
        });
        // C287: the wrapper clips at its sides only, so the chip's label
        // shows the glyphs its own clip lets through.
        Some(InkClip::sides(
            ChipButton::new(
                "composer-model",
                Chip::new(models.current().label())
                    .compact()
                    .code()
                    // Node 382: 12 px mono on its 14 px line.
                    .line_height(AUTO_LINE_12)
                    .truncate()
                    .color(Color::Default)
                    .outline(Color::Accent)
                    .leading(
                        Label::new(MODEL_MARK)
                            .size(LabelSize::XSmall)
                            .color(Color::Accent),
                    ),
            )
            .debug_selector("composer:model")
            .min_width(floor)
            .caret(true)
            .open(open)
            .on_toggle(cx.processor(|this, open, window, cx| {
                if open {
                    this.open_model_picker(cx);
                } else {
                    this.close_model_picker(window, cx);
                }
                // The picker keeps typing and navigation in the composer.
                this.input.focus_handle(cx).focus(window, cx);
            }))
            .menu(picker.map(|picker| {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .child(picker)
                    .into_any_element()
            })),
        ))
    }
}

impl Composer {
    fn to_chip(
        &self,
        floor: Pixels,
        replying: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let lead = if replying { "reply to" } else { "to" };
        // C287: clipped at its sides only, as the model chip is.
        InkClip::sides(
            ChipButton::new(
                "composer-to",
                Chip::new(self.target_label(cx))
                    .compact()
                    .code()
                    .line_height(AUTO_LINE_12)
                    .truncate()
                    .color(Color::Accent)
                    .leading(
                        Label::new(lead)
                            .size(LabelSize::Small)
                            .color(Color::Placeholder),
                    )
                    .map(|chip| {
                        if replying {
                            chip.trailing(
                                // Keep the glyph slot, the chip's gaps and its height;
                                // the target overflows the slot into the chip's padding.
                                div()
                                    .w(ICON_SM)
                                    .h(ICON_SM)
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        IconButton::new("composer:cancel-reply", UiIcon::Close)
                                            .size(IconSize::Small)
                                            .color(Color::Placeholder)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.cancel_reply(cx);
                                                this.input.focus_handle(cx).focus(window, cx);
                                            })),
                                    ),
                            )
                        } else {
                            chip
                        }
                    }),
            )
            .debug_selector("composer:to")
            // C260 and C278: preserve the lead and close mark or caret.
            .min_width(floor)
            .caret(!replying)
            .open(self.audience_open)
            .when(replying, |this| this.flex_shrink(REPLY_CHIP_SHRINK))
            .when(!replying, |this| {
                this.on_toggle(cx.processor(|this, open, window, cx| {
                    this.audience_open = open;
                    if open {
                        this.mentions_dismissed = true;
                        this.close_model_picker(window, cx);
                    }
                    this.input.focus_handle(cx).focus(window, cx);
                    cx.notify();
                }))
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fonts = cx.global::<Appearance>().fonts.clone();
        // Task 13: a pending reply says so and offers a way out. An
        // @mention of another agent drops it, and the chip says "to".
        let replying = self.reply_for(self.input.read(cx).text()).is_some();
        let lead = if replying { "reply to" } else { "to" };
        let floors = Floors::measure(lead, replying, &fonts, window);
        let chip_addressed = self.addressing == Addressing::Chip;
        let model = if floors.model_fits(self.box_width.get(), chip_addressed) {
            self.model_chip(floors.model, cx)
        } else {
            // C282: the picker hangs off the chip, so it closes with it;
            // an open picker nobody sees would take the next Escape.
            self.close_model_picker(window, cx);
            None
        };
        let audience_menu = self.audience_open.then(|| self.target_menu(false, cx));
        let mention_menu =
            (!self.mention_candidates(cx).is_empty()).then(|| self.target_menu(true, cx));
        let to = chip_addressed.then(|| self.to_chip(floors.to, replying, cx));
        let theme = &cx.global::<Appearance>().theme;
        div()
            .w_full()
            .flex_none()
            // The font Floors::measure sets the parts in.
            .font_family(fonts.ui_family.clone())
            // Task 13 and Task 15: Escape closes the open model picker and
            // goes no further (C87, C113), else ends a reply; src/keymap.rs
            // binds it.
            .key_context(crate::keymap::COMPOSER_CONTEXT)
            .on_action(cx.listener(|this, _: &CompleteMention, window, cx| {
                if !this.complete_mention(None, window, cx) {
                    cx.propagate();
                }
            }))
            .capture_action(cx.listener(|this, _: &super::text_input::Up, _, cx| {
                this.move_mention(false, cx);
            }))
            .capture_action(cx.listener(|this, _: &super::text_input::Down, _, cx| {
                this.move_mention(true, cx);
            }))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
                if this.audience_open {
                    this.audience_open = false;
                    cx.notify();
                } else if !this.mention_candidates(cx).is_empty() {
                    this.mentions_dismissed = true;
                    cx.notify();
                } else if this.models.as_ref().is_some_and(ModelMenu::is_open) {
                    this.close_model_picker(window, cx);
                } else if this.reply.is_some() {
                    this.cancel_reply(cx);
                    this.input.focus_handle(cx).focus(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .relative()
            // The audience and mention menus share the composer's top-left
            // anchor, rather than the chip's anchor, so their placement stays
            // fixed when the input grows.
            .when_some(audience_menu, |this, menu| {
                this.child(div().absolute().top_0().left_0().child(menu))
            })
            .when_some(mention_menu, |this, menu| {
                this.child(div().absolute().top_0().left_0().child(menu))
            })
            .pt(SPACE_8)
            .px(SPACE_14)
            .pb(SPACE_12)
            .child(
                div()
                    .debug_selector(|| "composer:box".into())
                    .w_full()
                    // Node 375 clips. The chips clip in their own
                    // wrappers; this cuts the input and the ⏎ in a pane
                    // too narrow for the "to" chip, the input's floor and
                    // the ⏎ (C278).
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
                            // C260: the chips give way first, so the
                            // input keeps room for its text and caret.
                            .min_w(COMPOSER_INPUT_MIN_WIDTH)
                            .flex()
                            .text_size(TEXT_DEFAULT)
                            .child(self.input.clone()),
                    )
                    // The UI font at body size, as node 385 draws it.
                    .child(
                        div()
                            .debug_selector(|| "composer:send-hint".into())
                            .flex_none()
                            .child(Label::new(SEND_HINT).color(Color::Muted)),
                    )
                    .child(self.box_probe(cx)),
            )
    }
}

/// The snapshot's listener as a recipient, when the person has one.
fn listener_target(snapshot: &Snapshot) -> Option<Target> {
    snapshot.listener.as_ref().map(|listener| Target {
        id: listener.id.clone(),
        handle: listener.handle.clone(),
    })
}

/// Every person and agent in the project's current roster.
fn roster_targets(snapshot: &Snapshot) -> Vec<Target> {
    snapshot
        .roster
        .iter()
        .flat_map(|group| {
            std::iter::once(Target {
                id: group.id.clone(),
                handle: group.person.clone(),
            })
            .chain(group.agents.iter().map(|agent| Target {
                id: agent.id.clone(),
                handle: agent.handle.clone(),
            }))
        })
        .collect()
}

impl Composer {
    /// An empty layer over the box that keeps its width for the next
    /// frame, and renders again when it changed (C278).
    fn box_probe(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let box_width = self.box_width.clone();
        let composer = cx.entity_id();
        canvas(
            move |bounds, _, cx| {
                let width = Some(bounds.size.width);
                if box_width.replace(width) != width {
                    // This frame laid the row out for the old width.
                    cx.defer(move |cx| cx.notify(composer));
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

/// How much harder the reply chip shrinks than the model chip: enough that
/// its handle is cut to nothing before the model chip gives up any width.
const REPLY_CHIP_SHRINK: f32 = 1000.;

/// The mark before the model chip's label.
const MODEL_MARK: &str = "◆";

/// The send hint at the end of the row (node 385).
const SEND_HINT: &str = "⏎";

/// The narrowest each part of the composer's row gets, measured in the UI
/// font the composer sets on itself.
#[derive(Debug, Clone, Copy)]
struct Floors {
    /// The "to" chip: its padding, gaps, lead label and close or chevron icon,
    /// with the handle cut to nothing (C260).
    to: Pixels,
    /// The model chip: its padding, gaps, ◆ and chevron icon, with the model's name
    /// cut to nothing (C278).
    model: Pixels,
    /// The ⏎ hint, which keeps its width.
    hint: Pixels,
}

impl Floors {
    fn measure(lead: &str, replying: bool, fonts: &Fonts, window: &Window) -> Self {
        let width = |text: &str, size: LabelSize| ui_text_width(text, size, fonts, window);
        let caret = ICON_SM;
        let to_end = if replying { ICON_SM } else { caret };
        Self {
            to: Chip::compact_floor(width(lead, LabelSize::Small), to_end),
            model: Chip::compact_floor(width(MODEL_MARK, LabelSize::XSmall), caret),
            hint: width(SEND_HINT, LabelSize::Default),
        }
    }

    /// Whether the model chip fits a box whose padding box is `box_width`
    /// wide (what `box_probe` measures) at its floor,
    /// beside the "to" chip (when `with_to`), the input and the ⏎ at
    /// theirs (C278). Below that it hides rather than show a sliver with
    /// neither ◆ nor the chevron icon. A box not laid out yet shows it.
    fn model_fits(self, box_width: Option<Pixels>, with_to: bool) -> bool {
        let Some(box_width) = box_width else {
            return true;
        };
        let to = if with_to { self.to + SPACE_8 } else { px(0.) };
        // The box probe measures inside the border (C281), so the box's
        // padding, then the parts and the gaps between them.
        let needed = SPACE_10 * 2.
            + to
            + self.model
            + SPACE_8
            + COMPOSER_INPUT_MIN_WIDTH
            + SPACE_8
            + self.hint;
        box_width >= needed
    }
}

/// How wide `text` sets in the UI font at `size`, ceiled as gpui's Text
/// measures its width.
fn ui_text_width(text: &str, size: LabelSize, fonts: &Fonts, window: &Window) -> Pixels {
    let run = TextRun {
        len: text.len(),
        font: gpui::font(fonts.ui_family.clone()),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let size = size.rems().to_pixels(window.rem_size());
    window
        .text_system()
        .shape_line(text.to_owned().into(), size, &[run], None)
        .width()
        .ceil()
}

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;
