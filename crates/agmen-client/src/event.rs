//! What a session reports, and which messages are for this session
//! (ruling R31). `agmen watch --json` prints each [`ClientEvent`] as one
//! line of JSON, and the Claude Code plugin reads exactly that.

use agmen_proto::{IdentityId, MessageId, Payload, StampedEnvelope, Stream};
use serde::{Deserialize, Serialize};

use crate::outbox::OutboxState;

/// Who a session runs as: the identity, its handle, and the person who owns
/// it. `agmen watch --json` prints it first, as `{"kind":"self",...}`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Me {
    /// The identity.
    pub identity_id: IdentityId,
    /// Its handle, such as `andy/sol`.
    pub handle: String,
    /// The person who owns it.
    pub owner_id: IdentityId,
}

impl Me {
    /// Whether `message` is for this session (ruling R31): it mentions
    /// this identity, is a DM or an ASK to it, assigns it an item, is a
    /// GRANT or DENY in its inbox, answers one of its own messages (as
    /// `is_own` says), or is an ALERT from its owner. Never when this
    /// identity sent it.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use agmen_client::Me;
    /// use agmen_proto::StampedEnvelope;
    ///
    /// let me = Me {
    ///     identity_id: "idn7k2q9".parse()?,
    ///     handle: "andy/sol".into(),
    ///     owner_id: "andyandy".parse()?,
    /// };
    /// let dm: StampedEnvelope = serde_json::from_str(r#"{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS",
    ///     "verb":"DM","data":{},"to":["idn7k2q9"],"body":"hi","sent_at":"2026-10-25T16:04:11Z",
    ///     "stream":"inbox:idn7k2q9","seq":3,"from":"adamcoord","from_handle":"adam/coord",
    ///     "from_kind":"coordinator","hub_at":"2026-10-25T16:04:11Z"}"#)?;
    /// assert!(me.is_for_me(&dm, |_| false));
    /// # Ok(()) }
    /// ```
    #[must_use]
    pub fn is_for_me(
        &self,
        message: &StampedEnvelope,
        is_own: impl Fn(&MessageId) -> bool,
    ) -> bool {
        let me = &self.identity_id;
        if message.from == *me {
            return false;
        }
        let client = &message.client;
        let in_my_inbox = message.stream == Stream::Inbox(me.clone());
        message.mentioned.contains(me)
            || match &client.payload {
                Payload::Dm {} | Payload::Ask { .. } => client.to.contains(me),
                Payload::Assign { to, .. } => to == me,
                Payload::Grant { .. } | Payload::Deny { .. } => in_my_inbox,
                Payload::Answer {} => client.reply_to.as_ref().is_some_and(is_own),
                Payload::Alert { .. } => message.from == self.owner_id,
                _ => false,
            }
    }
}

/// One thing a session reports. Serializes as one JSON object tagged by
/// `kind` (ruling R31); consumers ignore kinds they do not know.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ClientEvent {
    /// `{"kind":"self","identity_id","handle","owner_id"}`: who the session
    /// runs as. `agmen watch --json` prints it first; a session never
    /// yields it.
    #[serde(rename = "self")]
    Me(Me),
    /// `{"kind":"message","message":<StampedEnvelope>}`: a message from the
    /// feed or this identity's inbox, in seq order per stream, each once.
    Message {
        /// The message.
        message: Box<StampedEnvelope>,
    },
    /// `{"kind":"delivery","id","state"}`: where one of this identity's
    /// outbox messages stands.
    Delivery {
        /// The message's id.
        id: MessageId,
        /// Its state.
        state: OutboxState,
    },
    /// `{"kind":"connection","connected"}`: the broker link went up or
    /// down.
    Connection {
        /// Whether the session is connected now.
        connected: bool,
    },
}

impl ClientEvent {
    /// A message event.
    pub(crate) fn message(message: StampedEnvelope) -> Self {
        Self::Message {
            message: Box::new(message),
        }
    }
}

#[cfg(test)]
#[path = "event_tests.rs"]
mod tests;
