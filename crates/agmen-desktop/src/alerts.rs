//! Which ALERTs need the person, and what each new message should do to the
//! window (spec 8, Task 13 of the desktop plan).
//!
//! An ALERT reaches me when I own its sender or the hub stamped me into
//! `mentioned`, which it does for the sender's owners and for coordinators
//! (protocol 4, ALERT). A normal one gives a plain notification; an urgent
//! one needs me: highlight, chime and notification. During quiet hours an
//! urgent alert from someone else's agent only highlights and a normal one
//! makes no effect, while my own agents' and coordinators' still sound.
//!
//! A `needs: anyone` alert clears at the first ANSWER replying to it. A
//! `needs: owner` alert from my own agent stays until I answer it; one from
//! someone else's agent clears for me at the first answer, as it does for a
//! coordinator at their reply. The status bar's alert count uses the same
//! clearing rule.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use agmen_proto::{IdentityId, IdentityKind, MessageId, Needs, Payload, StampedEnvelope, Urgency};

use crate::backend::Snapshot;

/// How many alert ids the state remembers to ignore redeliveries. A copy
/// arriving after this many newer alerts is treated as new.
pub(crate) const MAX_REMEMBERED_ALERTS: usize = 1024;

/// At most this many alerts need me at once; past it the oldest one that is
/// not an owner alert from my own agent drops off the badge (the oldest of
/// all when every one is), so a flood of urgent alerts cannot grow the state
/// forever.
pub(crate) const MAX_NEEDING_ME: usize = 256;

/// The person running this app.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Me {
    pub(crate) id: IdentityId,
    /// Such as `andy`; my agents' handles start with `andy/`.
    pub(crate) handle: String,
}

impl From<&Snapshot> for Me {
    fn from(snapshot: &Snapshot) -> Self {
        Self {
            id: snapshot.me_id.clone(),
            handle: snapshot.me.clone(),
        }
    }
}

impl Me {
    fn owns(&self, env: &StampedEnvelope) -> bool {
        env.from_handle
            .strip_prefix(self.handle.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// Whether quiet hours are in effect now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum QuietHours {
    #[default]
    Off,
    On,
}

// shortcut: quiet hours and muted agents are plain values the caller fills in;
// read them from AgmenSettings once its settings file has them (spec 9.4).
/// The person's alert preferences.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AlertPrefs {
    /// During quiet hours, urgent alerts from others highlight without a chime
    /// or notification and normal ones make no effect; my own agents' and
    /// coordinators' still sound and notify.
    pub(crate) quiet_hours: QuietHours,
    /// Senders whose urgent alerts arrive as normal ones.
    pub(crate) muted: BTreeSet<IdentityId>,
}

/// What the window does for one new message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AlertEffect {
    None,
    /// A native notification only: a normal ALERT that reaches me, except
    /// during quiet hours when it is not from my own agent or a coordinator.
    Notify,
    Highlight,
    HighlightChimeNotify,
}

/// Whose ANSWER clears an alert for me.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ClearedBy {
    Anyone,
    /// A `needs: owner` alert from my own agent.
    OnlyMe,
}

impl ClearedBy {
    fn of(needs: Needs, alert: &StampedEnvelope, me: &Me) -> Self {
        if needs == Needs::Owner && me.owns(alert) {
            Self::OnlyMe
        } else {
            Self::Anyone
        }
    }

    fn cleared_by(self, answer: &StampedEnvelope, me: &Me) -> bool {
        self == Self::Anyone || answer.from == me.id
    }
}

/// The urgent alerts that need me, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AlertState {
    pub(crate) prefs: AlertPrefs,
    needing_me: Vec<MessageId>,
    /// Alerts in `needing_me` that only my answer clears.
    only_me: BTreeSet<MessageId>,
    /// Alert ids seen, newest last, at most [`MAX_REMEMBERED_ALERTS`].
    seen: VecDeque<MessageId>,
    /// Alerts I snoozed, oldest first, with whose answer clears them; at
    /// most [`MAX_NEEDING_ME`], since each came out of `needing_me`.
    snoozed: VecDeque<(MessageId, ClearedBy)>,
    /// When each alert in `needing_me` or `snoozed` arrived, counted up, so
    /// a woken alert goes back to its place; at most twice
    /// [`MAX_NEEDING_ME`].
    arrived: BTreeMap<MessageId, u64>,
    next_arrival: u64,
}

impl AlertState {
    /// Updates the state for one message and says how the window reacts.
    /// Only a first delivery of an ALERT makes an effect; an ANSWER may
    /// clear one.
    pub(crate) fn on_message(&mut self, env: &StampedEnvelope, me: &Me) -> AlertEffect {
        match &env.client.payload {
            Payload::Alert { urgency, needs, .. } => {
                if self.seen.contains(&env.client.id) {
                    return AlertEffect::None;
                }
                if self.seen.len() >= MAX_REMEMBERED_ALERTS {
                    self.seen.pop_front();
                }
                self.seen.push_back(env.client.id.clone());
                self.on_alert(env, *urgency, *needs, me)
            }
            Payload::Answer {} => {
                if let Some(alert) = &env.client.reply_to {
                    let snoozed = self.snoozed.iter().find(|(id, _)| id == alert);
                    let by = if self.only_me.contains(alert) {
                        ClearedBy::OnlyMe
                    } else {
                        snoozed.map_or(ClearedBy::Anyone, |(_, by)| *by)
                    };
                    if by.cleared_by(env, me) {
                        self.clear(alert);
                    }
                }
                AlertEffect::None
            }
            _ => AlertEffect::None,
        }
    }

    fn on_alert(
        &mut self,
        env: &StampedEnvelope,
        urgency: Urgency,
        needs: Needs,
        me: &Me,
    ) -> AlertEffect {
        let mine = me.owns(env);
        let reaches_me = env.from != me.id && (mine || env.mentioned.contains(&me.id));
        if !reaches_me {
            return AlertEffect::None;
        }
        // During quiet hours only my own agents and coordinators make a sound
        // or a notification.
        let hushed = self.prefs.quiet_hours == QuietHours::On
            && !mine
            && env.from_kind != IdentityKind::Coordinator;
        let urgent = urgency == Urgency::Urgent && !self.prefs.muted.contains(&env.from);
        if !urgent {
            return if hushed {
                AlertEffect::None
            } else {
                AlertEffect::Notify
            };
        }
        let id = &env.client.id;
        // A copy that outlived `seen` is still the same alert.
        if self.needing_me.contains(id) || self.is_snoozed(id) {
            return AlertEffect::None;
        }
        self.arrived.insert(id.clone(), self.next_arrival);
        self.next_arrival = self.next_arrival.saturating_add(1);
        self.push_needing(id.clone(), ClearedBy::of(needs, env, me));
        if hushed {
            AlertEffect::Highlight
        } else {
            AlertEffect::HighlightChimeNotify
        }
    }

    /// Adds `id` to the alerts that need me at its place by arrival,
    /// dropping the oldest past [`MAX_NEEDING_ME`].
    fn push_needing(&mut self, id: MessageId, by: ClearedBy) {
        if self.needing_me.len() >= MAX_NEEDING_ME {
            // Others' alerts go first, so a flood from many senders cannot
            // push out an owner alert from my own agent.
            let at = self
                .needing_me
                .iter()
                .position(|id| !self.only_me.contains(id))
                .unwrap_or(0);
            let evicted = self.needing_me.remove(at);
            self.only_me.remove(&evicted);
            self.arrived.remove(&evicted);
        }
        if by == ClearedBy::OnlyMe {
            self.only_me.insert(id.clone());
        }
        let arrived = |id: &MessageId| self.arrived.get(id).copied().unwrap_or(u64::MAX);
        let mine = arrived(&id);
        let at = self
            .needing_me
            .partition_point(|other| arrived(other) <= mine);
        self.needing_me.insert(at, id);
    }

    /// The urgent alerts that need me, oldest first.
    #[must_use]
    pub(crate) fn needing_me(&self) -> &[MessageId] {
        &self.needing_me
    }

    /// The red count on the chat tab and the status bar's needs-you count.
    #[must_use]
    pub(crate) fn badge(&self) -> usize {
        self.needing_me.len()
    }

    /// Ends the need for `alert` without an ANSWER, such as after one of its
    /// actions, and ends its snooze. Returns whether it was needing me or
    /// snoozed.
    pub(crate) fn clear(&mut self, alert: &MessageId) -> bool {
        let before = self.needing_me.len() + self.snoozed.len();
        self.needing_me.retain(|id| id != alert);
        self.snoozed.retain(|(id, _)| id != alert);
        self.only_me.remove(alert);
        self.arrived.remove(alert);
        self.needing_me.len() + self.snoozed.len() != before
    }

    /// Snooze 15 min: the alert dims and stops needing me until [`wake`]
    /// (the chat's timer) or an answer that clears it. Returns whether it
    /// was needing me.
    ///
    /// [`wake`]: Self::wake
    pub(crate) fn snooze(&mut self, alert: &MessageId) -> bool {
        if !self.needing_me.contains(alert) {
            return false;
        }
        let by = if self.only_me.contains(alert) {
            ClearedBy::OnlyMe
        } else {
            ClearedBy::Anyone
        };
        // Not `clear`: the alert keeps its arrival for when it wakes.
        self.needing_me.retain(|id| id != alert);
        self.only_me.remove(alert);
        if self.snoozed.len() >= MAX_NEEDING_ME
            && let Some((dropped, _)) = self.snoozed.pop_front()
        {
            self.arrived.remove(&dropped);
        }
        self.snoozed.push_back((alert.clone(), by));
        true
    }

    /// Whether `alert` is snoozed.
    #[must_use]
    pub(crate) fn is_snoozed(&self, alert: &MessageId) -> bool {
        self.snoozed.iter().any(|(id, _)| id == alert)
    }

    /// Ends `alert`'s snooze: it needs me again, at its place by arrival
    /// and cleared by the same answers as before. Returns whether it was
    /// snoozed.
    pub(crate) fn wake(&mut self, alert: &MessageId) -> bool {
        let Some(at) = self.snoozed.iter().position(|(id, _)| id == alert) else {
            return false;
        };
        let Some((id, by)) = self.snoozed.remove(at) else {
            return false;
        };
        self.push_needing(id, by);
        true
    }

    #[cfg(test)]
    fn remembered_len(&self) -> usize {
        self.seen.len()
    }
}

/// The status bar's alert count: ALERTs in `messages` (oldest first) that
/// are still open for me, by the same clearing rule as [`AlertState`].
#[must_use]
pub(crate) fn alert_count(messages: &[StampedEnvelope], me: &Me) -> u32 {
    u32::try_from(open_alerts(messages, me).len()).unwrap_or(u32::MAX)
}

/// The first ALERT in `messages` (oldest first) that [`alert_count`]
/// counts: where the status bar's count opens the chat.
#[must_use]
pub(crate) fn oldest_open_alert<'a>(
    messages: &'a [StampedEnvelope],
    me: &Me,
) -> Option<&'a MessageId> {
    let open = open_alerts(messages, me);
    messages
        .iter()
        .filter(|env| matches!(env.client.payload, Payload::Alert { .. }))
        .map(|env| &env.client.id)
        .find(|id| open.contains_key(id))
}

/// The ALERTs in `messages` still open for me, by id.
fn open_alerts<'a>(messages: &'a [StampedEnvelope], me: &Me) -> BTreeMap<&'a MessageId, ClearedBy> {
    let mut open: BTreeMap<&MessageId, ClearedBy> = BTreeMap::new();
    // Alerts already answered, so a later copy does not reopen them; bounded
    // by the number of messages.
    let mut answered: BTreeSet<&MessageId> = BTreeSet::new();
    for env in messages {
        match &env.client.payload {
            Payload::Alert { needs, .. } => {
                if !answered.contains(&env.client.id) {
                    open.entry(&env.client.id)
                        .or_insert_with(|| ClearedBy::of(*needs, env, me));
                }
            }
            Payload::Answer {} => {
                if let Some(alert) = &env.client.reply_to
                    && let Some(by) = open.get(alert)
                    && by.cleared_by(env, me)
                {
                    open.remove(alert);
                    answered.insert(alert);
                }
            }
            _ => {}
        }
    }
    open
}

#[cfg(test)]
#[path = "alerts_tests.rs"]
mod tests;
