//! The chat's composer (frame "Desktop · chat", node 374): the "to" chip,
//! the model chip, the input and the ⏎ hint. An agent's view uses it for a
//! DM, with neither chip (frame "Desktop · agent", node 650).

use std::cell::Cell;
use std::rc::Rc;

use agmen_proto::{IdentityId, MessageId};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TextRun, Window, anchored, canvas,
    deferred, div, px,
};

use super::components::{Chip, Color, IconButton, IconSize, Label, LabelSize};
use super::ink_clip::InkClip;
use super::model_picker::{ModelMenu, PickerEvent};
use super::text_input::{TextInput, TextInputEvent};
use super::tokens::{
    AUTO_LINE_12, AUTO_LINE_13, BORDER_WIDTH, COMPOSER_INPUT_MIN_WIDTH, ICON_SM,
    MODEL_PICKER_OFFSET, RADIUS_INPUT, SPACE_8, SPACE_10, SPACE_12, SPACE_14, TEXT_DEFAULT,
};
use crate::appearance::{Appearance, Fonts};
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
            box_width: Rc::default(),
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
            div()
                .id("composer-model")
                .debug_selector(|| "composer:model".into())
                .relative()
                // C278: the chip shrinks to its ◆ and ▾ and no further;
                // below that the composer hides it. The picker is
                // deferred, so the clip does not cut it.
                .min_w(floor)
                .cursor_pointer()
                // C81: while open, mouse down outside the picker closes it;
                // a click here too would reopen it on mouse up.
                .when(!open, |this| {
                    this.on_click(cx.listener(|this, _, _, cx| {
                        this.open_model_picker(cx);
                    }))
                })
                .child(
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
                        )
                        .trailing(
                            div()
                                .debug_selector(|| "composer:model-caret".into())
                                .flex_none()
                                .child(caret()),
                        ),
                )
                .when_some(picker, |this, picker| {
                    this.child(div().absolute().top_0().left_0().child(picker))
                }),
        ))
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
        let theme = &cx.global::<Appearance>().theme;
        let to = chip_addressed.then(|| {
            // C287: clipped at its sides only, as the model chip is.
            InkClip::sides(
                div()
                    .debug_selector(|| "composer:to".into())
                    // C260 and C278: the handle gives way, then the chip
                    // stops at its lead and its close mark or ▾, so the way
                    // out of a reply stays whole.
                    .min_w(floors.to)
                    .when(replying, |this| {
                        // C260: while replying, the handle gives way first,
                        // then the model chip.
                        this.flex_shrink(REPLY_CHIP_SHRINK)
                    })
                    .child(
                        // C260: a narrow box cuts the handle, not the close
                        // mark or the caret after it.
                        Chip::new(self.target(cx).handle.clone())
                            .compact()
                            .code()
                            // Node 378: 12 px mono on its 14 px line.
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
                    ),
            )
        });
        div()
            .w_full()
            .flex_none()
            // The font Floors::measure sets the parts in.
            .font_family(fonts.ui_family.clone())
            // Task 13 and Task 15: Escape closes the open model picker and
            // goes no further (C87, C113), else ends a reply; src/keymap.rs
            // binds it.
            .key_context(crate::keymap::COMPOSER_CONTEXT)
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
                if this.models.as_ref().is_some_and(ModelMenu::is_open) {
                    this.close_model_picker(window, cx);
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

/// The ▾ after a chip's label.
const CARET: &str = "▾";

/// The send hint at the end of the row (node 385).
const SEND_HINT: &str = "⏎";

/// The narrowest each part of the composer's row gets, measured in the UI
/// font the composer sets on itself.
#[derive(Debug, Clone, Copy)]
struct Floors {
    /// The "to" chip: its padding, gaps, lead label and close mark or ▾,
    /// with the handle cut to nothing (C260).
    to: Pixels,
    /// The model chip: its padding, gaps, ◆ and ▾, with the model's name
    /// cut to nothing (C278).
    model: Pixels,
    /// The ⏎ hint, which keeps its width.
    hint: Pixels,
}

impl Floors {
    fn measure(lead: &str, replying: bool, fonts: &Fonts, window: &Window) -> Self {
        let width = |text: &str, size: LabelSize| ui_text_width(text, size, fonts, window);
        let caret = width(CARET, LabelSize::XSmall);
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
    /// neither ◆ nor ▾. A box not laid out yet shows it.
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

/// The ▾ after a chip's label.
fn caret() -> Label {
    Label::new(CARET)
        .size(LabelSize::XSmall)
        .color(Color::Placeholder)
}

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;
