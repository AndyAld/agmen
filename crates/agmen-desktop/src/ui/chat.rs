//! The chat pane (frame "Desktop · chat", stream node 137 and composer node
//! 374): the project chat as a virtualized list of message rows above the
//! composer. Rows wrap, so the list measures each one. Change cards sit
//! under the listener message they belong to, and ledger presence lines
//! (HELLO, BYE, ROLLCALL, HEARTBEAT) hide until "Show ledger".

use std::rc::Rc;

use agmen_proto::{StampedEnvelope, Verb};
use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ListAlignment, ListState, ParentElement as _, Render,
    SharedString, Styled as _, Task, Window, div, list,
};

use super::components::{Color, Label, LabelSize};
use super::composer::Composer;
use super::message::MessageRow;
use super::ref_chip::{ChatContext, RefEvent};
use super::tokens::{LIST_OVERDRAW, SPACE_2, SPACE_6, SPACE_10, SPACE_12, SPACE_14};
use crate::alerts::{AlertState, Me};
use crate::backend::{Action, BackendError, BackendHandle, Draft, Snapshot, Update};
use crate::local_time::UtcOffset;

/// Presence bookkeeping, which the chat hides until "Show ledger". Lease
/// and state lines (GRANT, DENY, RELEASE, STATE) stay, as the mockup draws
/// the GRANT and the hub's STATE.
#[must_use]
pub(crate) fn is_presence(env: &StampedEnvelope) -> bool {
    matches!(
        env.client.payload.verb(),
        Some(Verb::Hello | Verb::Bye | Verb::Rollcall | Verb::Heartbeat)
    )
}

/// The indices of the messages the chat draws, oldest first.
#[must_use]
pub(crate) fn visible_rows(messages: &[StampedEnvelope], show_ledger: bool) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, env)| show_ledger || !is_presence(env))
        .map(|(i, _)| i)
        .collect()
}

/// The chat. It hears the backend's updates, so what the person sends
/// and the cards they apply show up here. Reference chips are passed on
/// as [`RefEvent`]s for the center to open. Tests find a refusal from the
/// backend as `"chat:error"`.
pub(crate) struct ChatView {
    snapshot: Rc<Snapshot>,
    context: Rc<ChatContext>,
    /// Indices into `snapshot.messages` of the drawn rows, oldest first.
    rows: Vec<usize>,
    list: ListState,
    show_ledger: bool,
    alerts: AlertState,
    composer: Entity<Composer>,
    /// Why the last send or action failed, until the next one succeeds.
    error: Option<SharedString>,
    utc_offset_minutes: i32,
    _updates: Option<Task<()>>,
}

impl std::fmt::Debug for ChatView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatView")
            .field("rows", &self.rows.len())
            .field("show_ledger", &self.show_ledger)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<RefEvent> for ChatView {}

impl ChatView {
    /// The chat over `snapshot`, sending and acting through the
    /// [`BackendHandle`] global when there is one.
    pub(crate) fn new(snapshot: Snapshot, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let utc_offset_minutes = cx.try_global::<UtcOffset>().map_or(0, |o| o.minutes());
        let chat = cx.entity().downgrade();
        let composer = cx.new(|cx| {
            Composer::new(
                &snapshot,
                move |draft, _, cx| {
                    chat.update(cx, |chat, cx| chat.send(draft, cx))
                        // The chat closes with its window before its
                        // composer; a draft sent in between is refused.
                        .unwrap_or(Err(BackendError::Unsupported(
                            "sending after the chat closed",
                        )))
                },
                window,
                cx,
            )
        });
        let updates = cx
            .try_global::<BackendHandle>()
            .map(|backend| backend.0.subscribe());
        // The loop ends when the backend drops its sender or the chat closes.
        let updates = updates.map(|mut updates| {
            cx.spawn(async move |chat, cx| {
                while let Some(update) = updates.next().await {
                    if chat.update(cx, |chat, cx| chat.apply(update, cx)).is_err() {
                        break;
                    }
                }
            })
        });
        let snapshot = Rc::new(snapshot);
        let rows = visible_rows(&snapshot.messages, false);
        let mut alerts = AlertState::default();
        replay(&mut alerts, &snapshot);
        Self {
            context: Rc::new(
                ChatContext::new(snapshot.clone()).with_utc_offset(utc_offset_minutes),
            ),
            snapshot,
            list: ListState::new(rows.len(), ListAlignment::Bottom, LIST_OVERDRAW),
            rows,
            show_ledger: false,
            alerts,
            composer,
            error: None,
            utc_offset_minutes,
            _updates: updates,
        }
    }

    /// The chat tab's red count: urgent alerts that need me (Task 13).
    pub(crate) fn badge(&self) -> usize {
        self.alerts.badge()
    }

    #[cfg(test)]
    pub(crate) const fn show_ledger(&self) -> bool {
        self.show_ledger
    }

    /// Shows or hides the presence lines.
    pub(crate) fn set_show_ledger(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.show_ledger == on {
            return;
        }
        self.show_ledger = on;
        self.rows = visible_rows(&self.snapshot.messages, on);
        self.list.reset(self.rows.len());
        cx.notify();
    }

    /// Sends `draft` through the backend; a refusal shows above the
    /// composer until the next send or action succeeds.
    ///
    /// # Errors
    ///
    /// When the backend refuses the draft, or there is no backend.
    pub(crate) fn send(
        &mut self,
        draft: Draft,
        cx: &mut Context<Self>,
    ) -> Result<(), BackendError> {
        let result = match cx.try_global::<BackendHandle>() {
            Some(backend) => backend.0.send(draft),
            None => Err(BackendError::Unsupported("sending without a backend")),
        };
        self.error = result.as_ref().err().map(|err| err.to_string().into());
        if result.is_ok() {
            // The person wants to see what they sent, wherever they had scrolled.
            self.list.scroll_to_end();
        }
        cx.notify();
        result
    }

    /// Does `action` through the backend, such as a change card's Apply.
    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        let result = match cx.try_global::<BackendHandle>() {
            Some(backend) => backend.0.act(action),
            None => Err(BackendError::Unsupported("acting without a backend")),
        };
        self.error = result.err().map(|err| err.to_string().into());
        cx.notify();
    }

    fn apply(&mut self, update: Update, cx: &mut Context<Self>) {
        let me = Me::from(&*self.snapshot);
        let snapshot = match update {
            Update::Message(env) => {
                // Task 13 plays the chime and posts the notification from
                // this effect; the chat only keeps the count.
                let _effect = self.alerts.on_message(&env, &me);
                // shortcut: each message clones the snapshot and rebuilds the
                // context, O(messages); keep messages in a persistent list if
                // long chats lag.
                let mut snapshot = (*self.snapshot).clone();
                snapshot.messages.push(*env);
                snapshot
            }
            Update::Snapshot(snapshot) => {
                let mut alerts = AlertState::default();
                alerts.prefs = self.alerts.prefs.clone();
                replay(&mut alerts, &snapshot);
                self.alerts = alerts;
                *snapshot
            }
        };
        self.set_snapshot(snapshot);
        cx.notify();
    }

    fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.snapshot = Rc::new(snapshot);
        self.context = Rc::new(
            ChatContext::new(self.snapshot.clone()).with_utc_offset(self.utc_offset_minutes),
        );
        let old = std::mem::replace(
            &mut self.rows,
            visible_rows(&self.snapshot.messages, self.show_ledger),
        );
        if self.rows.starts_with(&old) {
            self.list
                .splice(old.len()..old.len(), self.rows.len() - old.len());
            // A card's state may have changed its row's height.
            self.list.remeasure();
        } else {
            self.list.reset(self.rows.len());
        }
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(env) = self
            .rows
            .get(ix)
            .and_then(|&i| self.snapshot.messages.get(i))
        else {
            return div().into_any_element();
        };
        let (on_ref, on_action) = (cx.entity().downgrade(), cx.entity().downgrade());
        let row = MessageRow::new(env.clone(), self.context.clone())
            .on_ref(move |event, _, cx| {
                // A chip outlives its chat only while the window closes.
                on_ref
                    .update(cx, |_, cx| cx.emit(event))
                    .unwrap_or_default();
            })
            .on_action(move |action, _, cx| {
                on_action
                    .update(cx, |chat, cx| chat.act(action, cx))
                    .unwrap_or_default();
            });
        div()
            .when(ix > 0, |this| this.pt(SPACE_2))
            .child(row)
            .into_any_element()
    }
}

/// Feeds every message in `snapshot` to `alerts`, oldest first.
fn replay(alerts: &mut AlertState, snapshot: &Snapshot) {
    let me = Me::from(snapshot);
    for env in &snapshot.messages {
        let _effect = alerts.on_message(env, &me);
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "chat".into())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix, _, cx| this.render_row(ix, cx)),
                )
                .flex_1()
                .min_h_0()
                .pt(SPACE_10)
                .pb(SPACE_6)
                .px(SPACE_12),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .debug_selector(|| "chat:error".into())
                        .px(SPACE_14)
                        .child(Label::new(error).size(LabelSize::Small).color(Color::Error)),
                )
            })
            .child(self.composer.clone())
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
