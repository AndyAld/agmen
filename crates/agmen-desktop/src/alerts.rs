//! Which ALERTs need the person, and what each new message should do to the
//! window (spec 8, Task 13 of the desktop plan).
//!
//! An urgent ALERT needs me when it mentions me, or comes from one of my own
//! agents or from a coordinator. A `needs: anyone` alert clears at the first
//! ANSWER replying to it; a `needs: owner` alert from my own agent stays
//! until I answer it. The status bar's alert count comes from the same rule.

use std::collections::{BTreeMap, BTreeSet};

use agmen_proto::{IdentityId, IdentityKind, MessageId, Needs, Payload, StampedEnvelope, Urgency};

use crate::backend::Snapshot;

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
    /// or notification; my own agents' and coordinators' still sound.
    pub(crate) quiet_hours: QuietHours,
    /// Senders whose urgent alerts arrive as normal ones.
    pub(crate) muted: BTreeSet<IdentityId>,
}

/// What the window does for one new message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AlertEffect {
    None,
    Highlight,
    HighlightChimeNotify,
}

/// The urgent alerts that need me, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AlertState {
    pub(crate) needing_me: Vec<MessageId>,
    pub(crate) prefs: AlertPrefs,
    /// `needs: owner` alerts from my own agents: only my answer clears them.
    only_mine: BTreeSet<MessageId>,
}

impl AlertState {
    /// Updates the state for one message and says how the window reacts.
    /// Only ALERTs make an effect; an ANSWER may clear one.
    pub(crate) fn on_message(&mut self, env: &StampedEnvelope, me: &Me) -> AlertEffect {
        match &env.client.payload {
            Payload::Alert { urgency, needs, .. } => self.on_alert(env, *urgency, *needs, me),
            Payload::Answer {} => {
                if let Some(alert) = &env.client.reply_to
                    && clears(self.only_mine.contains(alert), env, me)
                {
                    self.needing_me.retain(|id| id != alert);
                    self.only_mine.remove(alert);
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
        let mentions_me = env.mentioned.contains(&me.id);
        let mine = me.owns(env);
        let coordinator = env.from_kind == IdentityKind::Coordinator;
        let urgent = urgency == Urgency::Urgent && !self.prefs.muted.contains(&env.from);
        let needs_me = urgent && env.from != me.id && (mentions_me || mine || coordinator);
        if !needs_me {
            return if mentions_me {
                AlertEffect::Highlight
            } else {
                AlertEffect::None
            };
        }
        let id = &env.client.id;
        if !self.needing_me.contains(id) {
            self.needing_me.push(id.clone());
        }
        if only_mine(needs, env, me) {
            self.only_mine.insert(id.clone());
        }
        if self.prefs.quiet_hours == QuietHours::On && !mine && !coordinator {
            AlertEffect::Highlight
        } else {
            AlertEffect::HighlightChimeNotify
        }
    }

    /// The red count on the chat tab and the status bar's needs-you count.
    #[must_use]
    pub(crate) fn badge(&self) -> usize {
        self.needing_me.len()
    }
}

fn only_mine(needs: Needs, alert: &StampedEnvelope, me: &Me) -> bool {
    needs == Needs::Owner && me.owns(alert)
}

/// Whether `answer` clears its alert for me.
fn clears(only_mine: bool, answer: &StampedEnvelope, me: &Me) -> bool {
    !only_mine || answer.from == me.id
}

/// The status bar's alert count: ALERTs in `messages` (oldest first) that
/// are still open for me, by the same clearing rule as [`AlertState`].
#[must_use]
pub(crate) fn alert_count(messages: &[StampedEnvelope], me: &Me) -> u32 {
    // Each open alert, and whether only my answer clears it.
    let mut open: BTreeMap<&MessageId, bool> = BTreeMap::new();
    for env in messages {
        match &env.client.payload {
            Payload::Alert { needs, .. } => {
                open.insert(&env.client.id, only_mine(*needs, env, me));
            }
            Payload::Answer {} => {
                if let Some(alert) = &env.client.reply_to
                    && let Some(&mine) = open.get(alert)
                    && clears(mine, env, me)
                {
                    open.remove(alert);
                }
            }
            _ => {}
        }
    }
    u32::try_from(open.len()).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "alerts_tests.rs"]
mod tests;
