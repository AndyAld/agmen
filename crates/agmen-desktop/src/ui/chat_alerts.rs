//! Task 13 in the chat: what alerts do beyond `alerts.rs`. The chat feeds
//! every message through [`NeedsYou`], draws the banner above its rows and
//! the alert looks on them, rings the [`AlarmHandle`] for live alerts, and
//! keeps the toast and the snooze timers. A child of `chat`, so it reaches
//! the chat's fields.

use std::rc::Rc;

use agmen_proto::{MessageId, StampedEnvelope};
use gpui::{App, Context, Task, Window};

use super::{ChatView, Snapshot};
use crate::alerts::{AlertState, MAX_NEEDING_ME, Me, alert_count};
use crate::ui::alert_banner::AlertBanner;
use crate::ui::composer::Target;
use crate::ui::format::clock;
use crate::ui::needs_you::{
    AlarmHandle, Arrival, Banner, NeedsYou, SNOOZE_FOR, Signals, TOAST_FOR, Toast, alert_looks,
    find_alert,
};

/// The needs-you and alert counts the status bar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AlertCounts {
    /// Urgent alerts that need me: the red pill and the chat tab's badge.
    pub(crate) needs_you: usize,
    /// ALERTs still open for me (mockup fix 34).
    pub(crate) alerts: usize,
}

/// The chat's alert state beyond [`AlertState`].
#[derive(Debug, Default)]
pub(super) struct ChatAlerts {
    needs_you: NeedsYou,
    /// [`alert_count`] over the chat's messages, kept per update.
    open_alerts: usize,
    /// Dismisses the toast; replaced (and so cancelled) by the next toast.
    toast_timer: Option<Task<()>>,
    /// Wakes each snoozed alert; at most [`MAX_NEEDING_ME`].
    snooze_timers: Vec<(MessageId, Task<()>)>,
}

/// The alert state of `snapshot`'s history: what needs me and what was
/// answered, with no chime, notification or toast.
pub(super) fn replay(snapshot: &Snapshot) -> (AlertState, ChatAlerts) {
    let me = Me::from(snapshot);
    let mut alerts = AlertState::default();
    let mut chat = ChatAlerts::default();
    for env in &snapshot.messages {
        let quiet =
            chat.needs_you
                .on_message(&mut alerts, env, &me, &snapshot.messages, Arrival::History);
        debug_assert_eq!(quiet, Signals::default(), "history never signals");
    }
    chat.open_alerts = count(snapshot, &me);
    (alerts, chat)
}

fn count(snapshot: &Snapshot, me: &Me) -> usize {
    usize::try_from(alert_count(&snapshot.messages, me)).unwrap_or(usize::MAX)
}

impl ChatView {
    /// The status bar's needs-you and alert counts.
    pub(crate) fn alert_counts(&self) -> AlertCounts {
        AlertCounts {
            needs_you: self.alerts.badge(),
            alerts: self.alert_ui.open_alerts,
        }
    }

    /// The banner above the rows, if any.
    pub(crate) fn banner(&self) -> Option<Banner> {
        self.alert_ui
            .needs_you
            .banner(&self.alerts, &self.snapshot.messages, |at| {
                clock(at, &self.zone)
            })
    }

    /// The toast the workspace floats, if any.
    pub(crate) const fn toast(&self) -> Option<&Toast> {
        self.alert_ui.needs_you.toast()
    }

    pub(crate) fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        self.alert_ui.needs_you.dismiss_toast();
        self.alert_ui.toast_timer = None;
        cx.notify();
    }

    /// Scrolls the rows so `alert` shows ("Jump to it").
    pub(crate) fn jump_to_alert(&mut self, alert: &MessageId, cx: &mut Context<Self>) {
        let messages = &self.snapshot.messages;
        let Some(index) = find_alert(messages, alert)
            .and_then(|env| messages.iter().position(|m| std::ptr::eq(m, env)))
        else {
            return;
        };
        self.stream.update(cx, |stream, cx| {
            if let Some(row) = stream.rows.iter().position(|&row| row == index) {
                stream.list.scroll_to_reveal_item(row);
                cx.notify();
            }
        });
    }

    /// Draws the oldest alert that needs me as just arrived: its toast and
    /// "chime played", with no sound or notification. The `needs-you`
    /// screen's frame shows both, and history never makes them.
    pub(crate) fn show_arrival(&mut self, cx: &mut Context<Self>) {
        self.alert_ui
            .needs_you
            .show_arrival(&self.alerts, &self.snapshot.messages);
        self.start_toast_timer(cx);
        cx.notify();
    }

    /// Feeds one message that arrives now.
    pub(super) fn hear(&mut self, env: &StampedEnvelope, earlier: &[StampedEnvelope]) -> Signals {
        let me = Me::from(&*self.snapshot);
        self.alert_ui
            .needs_you
            .on_message(&mut self.alerts, env, &me, earlier, Arrival::Live)
    }

    /// Chimes, notifies and starts the toast's timer as `signals` ask.
    pub(super) fn signal(&mut self, signals: &Signals, cx: &mut Context<Self>) {
        let alarm = cx.try_global::<AlarmHandle>().map(|alarm| alarm.0.clone());
        if let Some(alarm) = alarm {
            if signals.chime {
                alarm.chime();
            }
            if let Some(notice) = &signals.notice {
                alarm.notify(notice, cx);
            }
        }
        if signals.toast {
            self.start_toast_timer(cx);
        }
    }

    fn start_toast_timer(&mut self, cx: &mut Context<Self>) {
        if self.toast().is_none() {
            return;
        }
        self.alert_ui.toast_timer = Some(cx.spawn(async move |chat, cx| {
            cx.background_executor().timer(TOAST_FOR).await;
            // A chat that closed has no toast left to dismiss.
            chat.update(cx, |chat, cx| chat.dismiss_toast(cx))
                .unwrap_or_default();
        }));
    }

    /// Snooze 15 min: the alert dims until its timer wakes it.
    pub(super) fn snooze(&mut self, alert: MessageId, cx: &mut Context<Self>) {
        if !self.alerts.snooze(&alert) {
            return;
        }
        let timers = &mut self.alert_ui.snooze_timers;
        if timers.len() >= MAX_NEEDING_ME {
            // Dropping the oldest timer cancels it; that alert stays
            // snoozed until it is answered.
            let (_, oldest) = timers.remove(0);
            drop(oldest);
        }
        let id = alert.clone();
        let timer = cx.spawn(async move |chat, cx| {
            cx.background_executor().timer(SNOOZE_FOR).await;
            // A chat that closed has nothing left to wake.
            chat.update(cx, |chat, cx| chat.wake(&id, cx))
                .unwrap_or_default();
        });
        timers.push((alert, timer));
        self.refresh_alerts(cx);
    }

    fn wake(&mut self, alert: &MessageId, cx: &mut Context<Self>) {
        self.alert_ui.snooze_timers.retain(|(id, _)| id != alert);
        if self.alerts.wake(alert) {
            self.refresh_alerts(cx);
        }
    }

    /// Starts a reply to the message at `index` in the composer, addressed
    /// to its sender (an alert's Reply).
    pub(super) fn reply(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(env) = self.snapshot.messages.get(index) else {
            return;
        };
        let to = Target {
            id: env.from.clone(),
            handle: env.from_handle.clone(),
        };
        let message = env.client.id.clone();
        self.composer
            .update(cx, |composer, cx| composer.reply(to, message, window, cx));
    }

    /// Recounts the open alerts and redraws the alert rows, after the
    /// messages or the alert state changed.
    pub(super) fn refresh_alerts(&mut self, cx: &mut Context<Self>) {
        let me = Me::from(&*self.snapshot);
        self.alert_ui.needs_you.forget_closed(&self.alerts);
        self.alert_ui.open_alerts = count(&self.snapshot, &me);
        let looks = Rc::new(alert_looks(&self.alerts, &self.snapshot.messages));
        self.stream.update(cx, |stream, cx| {
            if stream.looks != looks {
                stream.looks = looks;
                stream.list.remeasure();
                cx.notify();
            }
        });
        cx.notify();
    }

    /// The banner as an element, with Jump to it scrolling this chat.
    pub(super) fn banner_element(&self, cx: &Context<Self>) -> Option<AlertBanner> {
        let banner = self.banner()?;
        let chat = cx.entity().downgrade();
        Some(
            AlertBanner::new(banner).on_jump(move |alert, _: &mut Window, cx: &mut App| {
                // A banner outlives its chat only while the window closes.
                chat.update(cx, |chat, cx| chat.jump_to_alert(&alert, cx))
                    .unwrap_or_default();
            }),
        )
    }
}
