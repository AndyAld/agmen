//! The chat pane (frame "Desktop · chat", stream node 137 and composer node
//! 374): the project chat as a virtualized list of message rows above the
//! composer. Rows wrap, so the list measures each one. Change cards sit
//! under the listener message they belong to, and ledger presence lines
//! (HELLO, BYE, ROLLCALL, HEARTBEAT) hide until "Show ledger".

use std::collections::BTreeSet;
use std::rc::Rc;

use agmen_proto::{MessageId, Seq, StampedEnvelope, Stream};
use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState, ParentElement as _,
    Render, SharedString, StyleRefinement, Styled as _, Task, WeakEntity, Window, div, list, px,
};

use super::chat_context::ChatContext;
use super::components::{Color, Label, LabelSize};
use super::composer::Composer;
use super::message::{MessageRow, is_presence};
use super::ref_chip::RefEvent;
use super::tokens::{LIST_OVERDRAW, SPACE_2, SPACE_6, SPACE_10, SPACE_12, SPACE_14};
use crate::alerts::{AlertState, Me};
use crate::backend::{Action, BackendError, BackendHandle, Draft, Snapshot, Update};
use crate::local_time::UtcOffset;

/// The indices of the messages the chat draws, oldest first.
#[must_use]
fn visible_rows(messages: &[StampedEnvelope], show_ledger: bool) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, env)| show_ledger || !is_presence(env))
        .map(|(i, _)| i)
        .collect()
}

/// The message list. It is a view of its own, drawn cached, so a keystroke
/// in the composer (which redraws the chat around it) reuses the rows from
/// the last frame; it redraws only when the chat notifies it with new rows
/// or the list notifies it on a scroll.
pub(crate) struct ChatStream {
    context: Rc<ChatContext>,
    /// Indices into the snapshot's messages of the drawn rows, oldest first.
    rows: Vec<usize>,
    list: ListState,
    /// Where chips and change card buttons report.
    chat: WeakEntity<ChatView>,
}

impl std::fmt::Debug for ChatStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatStream")
            .field("rows", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl ChatStream {
    /// Shows `rows` of `context`'s snapshot. When the old rows lead the new
    /// ones, the list keeps its measurements and scroll position.
    fn set_rows(&mut self, context: Rc<ChatContext>, rows: Vec<usize>, cx: &mut Context<Self>) {
        self.context = context;
        let old = std::mem::replace(&mut self.rows, rows);
        if self.rows.starts_with(&old) {
            self.list
                .splice(old.len()..old.len(), self.rows.len() - old.len());
            // A card's state may have changed its row's height.
            self.list.remeasure();
        } else {
            self.list.reset(self.rows.len());
        }
        cx.notify();
    }

    /// Shows `rows`, keeping the message at the top of the view where it
    /// was. A view that followed the newest message keeps following it.
    fn set_rows_keeping_top(&mut self, rows: Vec<usize>, cx: &mut Context<Self>) {
        let top = self.list.logical_scroll_top();
        let top_message = self.rows.get(top.item_ix).copied();
        self.rows = rows;
        self.list.reset(self.rows.len());
        if let Some(message) = top_message {
            // The first row at or after the old top: the same message, or the
            // one after a presence line that just hid.
            let item_ix = self.rows.partition_point(|&row| row < message);
            let same = self.rows.get(item_ix) == Some(&message);
            self.list.scroll_to(ListOffset {
                item_ix,
                offset_in_item: if same { top.offset_in_item } else { px(0.) },
            });
        }
        cx.notify();
    }

    fn render_row(&self, ix: usize) -> AnyElement {
        let Some(&index) = self.rows.get(ix) else {
            return div().into_any_element();
        };
        let (on_ref, on_action) = (self.chat.clone(), self.chat.clone());
        let row = MessageRow::new(index, self.context.clone())
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
        // The stream's side padding (node 137) sits on each row: a list lays
        // its items out at its left edge whatever its padding.
        div()
            .px(SPACE_12)
            .when(ix > 0, |this| this.pt(SPACE_2))
            .child(row)
            .into_any_element()
    }
}

impl Render for ChatStream {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        list(
            self.list.clone(),
            cx.processor(|this, ix, _, _| this.render_row(ix)),
        )
        .size_full()
        .pt(SPACE_10)
        .pb(SPACE_6)
    }
}

/// The chat. It hears the backend's updates, so what the person sends
/// and the cards they apply show up here. Reference chips are passed on
/// as [`RefEvent`]s for the center to open. Tests find a refusal from the
/// backend as `"chat:error"`.
pub(crate) struct ChatView {
    snapshot: Rc<Snapshot>,
    stream: Entity<ChatStream>,
    show_ledger: bool,
    alerts: AlertState,
    composer: Entity<Composer>,
    /// Why the last send or action failed, until the next one succeeds.
    error: Option<SharedString>,
    offset: UtcOffset,
    _updates: Option<Task<()>>,
}

impl std::fmt::Debug for ChatView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatView")
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
        let offset = cx.try_global::<UtcOffset>().copied().unwrap_or_default();
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
        let context = Rc::new(ChatContext::new(snapshot.clone()).with_utc_offset(offset));
        let chat = cx.entity().downgrade();
        let stream = cx.new(|_| ChatStream {
            context,
            list: ListState::new(rows.len(), ListAlignment::Bottom, LIST_OVERDRAW),
            rows,
            chat,
        });
        Self {
            snapshot,
            stream,
            show_ledger: false,
            alerts,
            composer,
            error: None,
            offset,
            _updates: updates,
        }
    }

    /// The chat tab's red count: urgent alerts that need me (Task 13).
    pub(crate) fn badge(&self) -> usize {
        self.alerts.badge()
    }

    /// Whether the presence lines show; the breadcrumb's "Show ledger".
    pub(crate) const fn show_ledger(&self) -> bool {
        self.show_ledger
    }

    /// Shows or hides the presence lines, keeping the message at the top of
    /// the view where it was. A view that followed the newest message keeps
    /// following it.
    pub(crate) fn set_show_ledger(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.show_ledger == on {
            return;
        }
        self.show_ledger = on;
        let rows = visible_rows(&self.snapshot.messages, on);
        self.stream
            .update(cx, |stream, cx| stream.set_rows_keeping_top(rows, cx));
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
            self.stream.update(cx, |stream, cx| {
                stream.list.scroll_to_end();
                cx.notify();
            });
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
                // Only messages the chat has not seen update the alerts, so
                // an alert the person snoozed or cleared stays that way.
                let seen: BTreeSet<(&Stream, Seq, &MessageId)> =
                    self.snapshot.messages.iter().map(position).collect();
                for env in &snapshot.messages {
                    if !seen.contains(&position(env)) {
                        // Task 13 plays the chime and posts the notification
                        // from this effect, as for a single message.
                        let _effect = self.alerts.on_message(env, &me);
                    }
                }
                *snapshot
            }
        };
        self.snapshot = Rc::new(snapshot);
        let context = Rc::new(ChatContext::new(self.snapshot.clone()).with_utc_offset(self.offset));
        let rows = visible_rows(&self.snapshot.messages, self.show_ledger);
        self.stream
            .update(cx, |stream, cx| stream.set_rows(context, rows, cx));
        cx.notify();
    }
}

/// Where the hub put `env`, with its client id: a client id alone is
/// unique per sender only.
fn position(env: &StampedEnvelope) -> (&Stream, Seq, &MessageId) {
    (&env.stream, env.seq, &env.client.id)
}

/// Feeds every message in `snapshot` to `alerts`, oldest first.
fn replay(alerts: &mut AlertState, snapshot: &Snapshot) {
    let me = Me::from(snapshot);
    for env in &snapshot.messages {
        // History replays without a chime or notification: only alerts that
        // arrive while the chat is open make an effect.
        let _effect = alerts.on_message(env, &me);
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "chat".into())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                self.stream
                    .clone()
                    .cached(StyleRefinement::default().flex_1().min_h_0()),
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
