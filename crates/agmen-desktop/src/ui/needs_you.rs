//! What the window does for an alert beyond the alert state (plan Task 13):
//! the banner the chat shows, the in-app toast, the chime and the native
//! notification, and the text each one says.
//!
//! History loaded at launch only updates the state: it never chimes,
//! notifies, toasts or shows the Answered banner. A message that arrives
//! while the chat is open signals as `alerts.rs` decides: an urgent alert
//! that needs me chimes, notifies and toasts; one hushed by quiet hours only
//! highlights; a normal alert only notifies. "chime played" shows only once
//! the alarm says the chime sounds. A live answer to the last alert that
//! needed me shows the Answered banner for [`ANSWERED_FOR`] or until it is
//! clicked.
//!
//! The banner, toast and notification text is the alert's body with its
//! leading mentions dropped, as plain text; the mockup's copy is an edited
//! summary no rule derives (reference sheet mismatch 12).

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::Duration;

use agmen_proto::{IdentityKind, MessageId, Needs, Payload, StampedEnvelope, Urgency};
use gpui::{App, Global};
use jiff::Timestamp;

use crate::alerts::{AlertEffect, AlertState, MAX_NEEDING_ME, Me};
use crate::chime::{Chime, Played};

/// How long Snooze 15 min keeps an alert quiet.
pub(crate) const SNOOZE_FOR: Duration = Duration::from_secs(15 * 60);
/// How long the toast stays unless it is clicked or its alert clears.
pub(crate) const TOAST_FOR: Duration = Duration::from_secs(10);
/// How long the Answered banner stays unless it is clicked or another
/// alert takes the banner.
pub(crate) const ANSWERED_FOR: Duration = Duration::from_secs(30);
/// The app's own snooze button, which an alert's actions may repeat.
pub(crate) const SNOOZE_LABEL: &str = "Snooze 15 min";
/// The most sender actions an alert row draws.
pub(crate) const MAX_ALERT_ACTIONS: usize = 6;
/// The longest notification or toast body, in characters.
const MAX_NOTICE_CHARS: usize = 240;

/// Whether a message is history loaded at launch or arrives now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arrival {
    History,
    Live,
}

/// How an alert's row draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AlertLook {
    /// Red, with NEEDS YOU and its buttons.
    NeedsMe,
    /// Snoozed: the alert's look, dimmed, without the badge or buttons.
    Snoozed,
}

/// A native notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notice {
    /// Replaces an earlier notification for the same alert.
    pub(crate) tag: String,
    pub(crate) title: String,
    pub(crate) body: String,
}

/// What one message asks of the window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Signals {
    /// The alert whose chime to play.
    pub(crate) chime: Option<MessageId>,
    pub(crate) notice: Option<Notice>,
    /// A new toast replaced the last one.
    pub(crate) toast: bool,
    /// A new Answered banner replaced the last one.
    pub(crate) answered: bool,
}

/// The in-app toast (node 6004).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Toast {
    pub(crate) alert: MessageId,
    /// "adam/coord needs you".
    pub(crate) title: String,
    pub(crate) body: String,
    /// Whether its chime played, for the "♪ urgent chime" line.
    pub(crate) chimed: bool,
}

/// The banner under the chat's breadcrumb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Banner {
    /// The oldest alert that needs me (node 5704).
    NeedsYou {
        alert: MessageId,
        message: String,
        /// Whether its chime played, for the "chime played" chip.
        chimed: bool,
    },
    /// The last alert that needed me was just answered (node 6257).
    Answered { message: String },
}

/// Who answered the last alert that needed me.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Answered {
    /// "You", "adam (coordinator)" or a handle.
    by: String,
    /// The alert's sender.
    sender: String,
    at: Timestamp,
    /// A `needs: anyone` alert, which the answer cleared for everyone.
    everyone: bool,
}

/// The chime and the native notification, as a global so tests and
/// screenshots stay silent: without it nothing sounds or posts.
pub(crate) trait Alarm {
    /// Plays the chime; returns whether it sounds (started now or still
    /// sounding from an alert just before).
    fn chime(&self) -> bool;
    fn notify(&self, notice: &Notice, cx: &App);
}

/// The app's [`Alarm`].
pub(crate) struct AlarmHandle(pub(crate) Rc<dyn Alarm>);

impl Global for AlarmHandle {}

impl AlarmHandle {
    /// The chime and the platform's notifications.
    #[must_use]
    pub(crate) fn system() -> Self {
        Self(Rc::new(SystemAlarm::default()))
    }
}

/// [`Chime`] and `notify.rs`.
#[derive(Debug, Default)]
struct SystemAlarm {
    chime: Chime,
}

impl Alarm for SystemAlarm {
    fn chime(&self) -> bool {
        matches!(self.chime.play(), Played::Started | Played::StillPlaying)
    }

    fn notify(&self, notice: &Notice, cx: &App) {
        crate::notify::notify(&notice.tag, &notice.title, &notice.body, cx);
    }
}

/// The banner, toast and chime state beside [`AlertState`].
#[derive(Debug, Default)]
pub(crate) struct NeedsYou {
    /// Alerts whose chime played, while they need me or sleep; at most
    /// [`MAX_NEEDING_ME`].
    chimed: Vec<MessageId>,
    answered: Option<Answered>,
    toast: Option<Toast>,
}

impl NeedsYou {
    /// Feeds `env` to `alerts` and says what the window does. `messages`
    /// holds the chat's earlier messages, where an answered alert is found.
    pub(crate) fn on_message(
        &mut self,
        alerts: &mut AlertState,
        env: &StampedEnvelope,
        me: &Me,
        messages: &[StampedEnvelope],
        arrival: Arrival,
    ) -> Signals {
        // The alert an ANSWER replies to, while it needs me or sleeps.
        let open_before = match (&env.client.payload, &env.client.reply_to) {
            (Payload::Answer {}, Some(alert)) if is_open(alerts, alert) => Some(alert.clone()),
            _ => None,
        };
        let effect = alerts.on_message(env, me);
        let mut answered = false;
        if let Some(alert) = open_before
            && !is_open(alerts, &alert)
        {
            // Only a live answer to the last alert that needed me: history
            // may be days old, and another alert still needing me keeps
            // the banner.
            self.answered = if arrival == Arrival::Live && alerts.needing_me().is_empty() {
                find_alert(messages, &alert).map(|alert| Answered::new(alert, env, me))
            } else {
                None
            };
            answered = self.answered.is_some();
        }
        if matches!(
            effect,
            AlertEffect::Highlight | AlertEffect::HighlightChimeNotify
        ) {
            self.answered = None;
        }
        self.forget_closed(alerts);
        if arrival == Arrival::History {
            return Signals::default();
        }
        let signals = match effect {
            AlertEffect::None | AlertEffect::Highlight => Signals::default(),
            AlertEffect::Notify => Signals {
                notice: Some(Notice {
                    tag: tag(env),
                    title: format!("Alert from {}", env.from_handle),
                    body: notice_body(env),
                }),
                ..Signals::default()
            },
            AlertEffect::HighlightChimeNotify => {
                // "chime played" waits for the alarm to say it did.
                let toast = toast_for(env);
                let notice = Notice {
                    tag: tag(env),
                    title: toast.title.clone(),
                    body: toast.body.clone(),
                };
                self.toast = Some(toast);
                Signals {
                    chime: Some(env.client.id.clone()),
                    notice: Some(notice),
                    toast: true,
                    answered: false,
                }
            }
        };
        Signals {
            answered,
            ..signals
        }
    }

    fn mark_chimed(&mut self, alert: &MessageId) {
        if self.chimed.contains(alert) {
            return;
        }
        if self.chimed.len() >= MAX_NEEDING_ME {
            self.chimed.remove(0);
        }
        self.chimed.push(alert.clone());
    }

    /// The banner the chat shows, if any. `clock` shows a time as the chat
    /// does.
    #[must_use]
    pub(crate) fn banner(
        &self,
        alerts: &AlertState,
        messages: &[StampedEnvelope],
        clock: impl Fn(Timestamp) -> String,
    ) -> Option<Banner> {
        if let Some(first) = alerts.needing_me().first() {
            let alert = find_alert(messages, first)?;
            return Some(Banner::NeedsYou {
                alert: first.clone(),
                message: format!(
                    "{}: {}",
                    alert.from_handle,
                    plain_text(alert.client.body.as_str())
                ),
                chimed: self.chimed.contains(first),
            });
        }
        self.answered.as_ref().map(|answered| Banner::Answered {
            message: answered.text(clock),
        })
    }

    /// Records that `alert`'s chime played, for the banner's chip and the
    /// toast's chime line.
    pub(crate) fn chime_played(&mut self, alert: &MessageId) {
        self.mark_chimed(alert);
        if let Some(toast) = self.toast.as_mut().filter(|toast| &toast.alert == alert) {
            toast.chimed = true;
        }
    }

    /// Hides the Answered banner.
    pub(crate) fn dismiss_answered(&mut self) {
        self.answered = None;
    }

    /// Shows the Answered banner for the last urgent alert in `messages`
    /// that an ANSWER closed, as if that answer had just arrived. For the
    /// `needs-you-answered` screenshot, whose frame shows it; nothing shows
    /// while an alert needs me.
    pub(crate) fn show_answered(
        &mut self,
        alerts: &AlertState,
        messages: &[StampedEnvelope],
        me: &Me,
    ) {
        if !alerts.needing_me().is_empty() {
            return;
        }
        let last = messages.iter().rev().find_map(|answer| {
            let (Payload::Answer {}, Some(id)) = (&answer.client.payload, &answer.client.reply_to)
            else {
                return None;
            };
            let alert = find_alert(messages, id).filter(|alert| {
                matches!(
                    alert.client.payload,
                    Payload::Alert {
                        urgency: Urgency::Urgent,
                        ..
                    }
                )
            })?;
            (!is_open(alerts, id)).then(|| Answered::new(alert, answer, me))
        });
        if last.is_some() {
            self.answered = last;
        }
    }

    /// The toast showing, if any.
    #[must_use]
    pub(crate) const fn toast(&self) -> Option<&Toast> {
        self.toast.as_ref()
    }

    pub(crate) fn dismiss_toast(&mut self) {
        self.toast = None;
    }

    /// Drops what refers to alerts that no longer need me: their chime and
    /// their toast. Call after the state changes outside a message, such as
    /// a snooze.
    pub(crate) fn forget_closed(&mut self, alerts: &AlertState) {
        self.chimed.retain(|id| is_open(alerts, id));
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| !alerts.needing_me().contains(&toast.alert))
        {
            self.toast = None;
        }
    }

    /// Shows the oldest alert that needs me as if it had just arrived: its
    /// toast and "chime played", with no sound or notification. For the
    /// `needs-you` screenshot, whose frame shows both.
    pub(crate) fn show_arrival(&mut self, alerts: &AlertState, messages: &[StampedEnvelope]) {
        let Some(alert) = alerts
            .needing_me()
            .first()
            .and_then(|id| find_alert(messages, id))
        else {
            return;
        };
        self.toast = Some(toast_for(alert));
        self.chime_played(&alert.client.id);
    }
}

impl Answered {
    fn new(alert: &StampedEnvelope, answer: &StampedEnvelope, me: &Me) -> Self {
        let by = if answer.from == me.id {
            "You".to_owned()
        } else if answer.from_kind == IdentityKind::Coordinator {
            format!("{} (coordinator)", answer.from_handle)
        } else {
            answer.from_handle.clone()
        };
        Self {
            by,
            sender: alert.from_handle.clone(),
            at: answer.hub_at,
            everyone: matches!(
                alert.client.payload,
                Payload::Alert {
                    needs: Needs::Anyone,
                    ..
                }
            ),
        }
    }

    /// "adam (coordinator) answered adam/coord's alert at 15:59. Cleared for
    /// everyone." (node 6261).
    fn text(&self, clock: impl Fn(Timestamp) -> String) -> String {
        let cleared = if self.everyone {
            " Cleared for everyone."
        } else {
            ""
        };
        format!(
            "{} answered {}'s alert at {}.{cleared}",
            self.by,
            self.sender,
            clock(self.at)
        )
    }
}

/// Whether `alert` needs me or is snoozed.
pub(crate) fn is_open(alerts: &AlertState, alert: &MessageId) -> bool {
    alerts.needing_me().contains(alert) || alerts.is_snoozed(alert)
}

/// The notification tag for `alert`, so a second post replaces the first.
fn tag(alert: &StampedEnvelope) -> String {
    format!("alert-{}", alert.client.id)
}

/// The toast for an alert that needs me (node 6004): "adam/coord needs you"
/// and its text.
fn toast_for(alert: &StampedEnvelope) -> Toast {
    Toast {
        alert: alert.client.id.clone(),
        title: format!("{} needs you", alert.from_handle),
        body: notice_body(alert),
        chimed: false,
    }
}

/// An alert's text for a notification or the toast, at most
/// [`MAX_NOTICE_CHARS`] characters.
fn notice_body(alert: &StampedEnvelope) -> String {
    let text = plain_text(alert.client.body.as_str());
    if text.chars().count() <= MAX_NOTICE_CHARS {
        return text;
    }
    let mut clipped: String = text.chars().take(MAX_NOTICE_CHARS - 1).collect();
    clipped.push('…');
    clipped
}

/// The message at each index of `messages` that is an alert needing me or
/// snoozed, with how it draws. Only the first ALERT carrying an id counts:
/// the hub keeps ids unique per sender only, so another sender's message
/// with that id draws as itself.
#[must_use]
pub(crate) fn alert_looks(
    alerts: &AlertState,
    messages: &[StampedEnvelope],
) -> BTreeMap<usize, AlertLook> {
    let mut looks = BTreeMap::new();
    let mut seen: BTreeSet<&MessageId> = BTreeSet::new();
    for (index, env) in messages.iter().enumerate() {
        if !matches!(env.client.payload, Payload::Alert { .. }) || !seen.insert(&env.client.id) {
            continue;
        }
        let id = &env.client.id;
        if alerts.needing_me().contains(id) {
            looks.insert(index, AlertLook::NeedsMe);
        } else if alerts.is_snoozed(id) {
            looks.insert(index, AlertLook::Snoozed);
        }
    }
    looks
}

/// The first ALERT in `messages` with id `id`.
#[must_use]
pub(crate) fn find_alert<'a>(
    messages: &'a [StampedEnvelope],
    id: &MessageId,
) -> Option<&'a StampedEnvelope> {
    messages
        .iter()
        .find(|m| &m.client.id == id && matches!(m.client.payload, Payload::Alert { .. }))
}

/// `body` as one line of plain text, without the mentions it starts with:
/// "@andy CI on main is red" reads "CI on main is red".
#[must_use]
pub(crate) fn plain_text(body: &str) -> String {
    body.split_whitespace()
        .skip_while(|word| word.starts_with('@'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The buttons under an alert that needs me: Reply, the sender's actions,
/// then the app's Snooze 15 min. A sender action that repeats Snooze is
/// left out, so it shows once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AlertButton {
    Reply,
    /// One of the sender's actions, sent back as the answer.
    Answer(String),
    Snooze,
}

/// [`AlertButton`]s for an alert offering `actions`, at most
/// [`MAX_ALERT_ACTIONS`] of them.
#[must_use]
pub(crate) fn alert_buttons(actions: &[String]) -> Vec<AlertButton> {
    let own = actions
        .iter()
        .filter(|action| !action.trim().eq_ignore_ascii_case(SNOOZE_LABEL))
        .take(MAX_ALERT_ACTIONS)
        .map(|action| AlertButton::Answer(action.clone()));
    std::iter::once(AlertButton::Reply)
        .chain(own)
        .chain(std::iter::once(AlertButton::Snooze))
        .collect()
}

#[cfg(test)]
#[path = "needs_you_tests.rs"]
mod tests;
