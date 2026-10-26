//! Envelopes (protocol section 3) and the hub's envelope checks.

use std::collections::BTreeMap;
use std::fmt;

use jiff::SignedDuration;
use schemars::JsonSchema;
use serde::de::{self, IgnoredAny};
use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;

use crate::error::ProtoError;
use crate::ids::{
    Body, IdentityId, ItemKey, LaneName, MAX_BODY_BYTES, Mention, MessageId, Seq, Stream, Timestamp,
};
use crate::reply::NackReason;
use crate::verbs::{Payload, Verb};

/// The largest raw envelope the hub parses, in bytes (protocol section 8).
pub const MAX_ENVELOPE_BYTES: usize = 64 * 1024;

/// The most DM or ASK recipients, and the most identity mentions, in one message.
const MAX_ADDRESSEES: usize = 8;

/// How far past `valid_until` the hub still accepts a message, for clock skew.
const VALID_UNTIL_SKEW: SignedDuration = SignedDuration::from_secs(120);

/// What kind of identity sent a message (protocol section 1).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum IdentityKind {
    /// A person.
    Human,
    /// A coding agent working for a person.
    Agent,
    /// An agent or person with the coordinator role.
    Coordinator,
    /// A person's listener: reads the chat and delegates work.
    Listener,
    /// The hub itself.
    Hub,
}

impl IdentityKind {
    /// The wire name, such as `listener`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
            Self::Coordinator => "coordinator",
            Self::Listener => "listener",
            Self::Hub => "hub",
        }
    }
}

impl fmt::Display for IdentityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Something the body names that the hub knows, stamped as a link.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
pub struct Ref {
    /// What the text names.
    pub kind: RefKind,
    /// Byte offset in `body` where the text starts.
    pub start: u32,
    /// Byte offset in `body` just past the text.
    pub end: u32,
    /// The id, name, key or number it names.
    pub target: String,
}

/// What a `Ref` names.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    /// An identity's handle.
    Identity,
    /// A lane.
    Lane,
    /// An item.
    Item,
    /// A role.
    Role,
    /// A pull request number.
    Pr,
}

/// What a client publishes to `up/{identity_id}`.
///
/// Parsing rejects fields protocol v1 does not define, so a client cannot
/// smuggle in `from` or another hub-stamped field. In Rust,
/// `ClientEnvelope::from_json` also says which NACK reason a failure earns;
/// plain serde deserialization applies the same rules.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema, TS)]
#[schemars(deny_unknown_fields)]
pub struct ClientEnvelope {
    /// Protocol version; 1.
    pub v: u8,
    /// A ULID the client generates; the dedupe key with `from`.
    pub id: MessageId,
    /// The id of an earlier message this one answers or threads under.
    pub reply_to: Option<MessageId>,
    /// Recipients: 1 to 8 on DM, 0 to 8 on ASK (an ASK with recipients goes
    /// to their inboxes, like a DM), and empty for every other verb. Parses
    /// from a list, `null`, or a missing field.
    #[serde(default)]
    #[schemars(with = "Option<Vec<IdentityId>>")]
    pub to: Vec<IdentityId>,
    /// Mentions; only FYI, ASK, ANSWER and ALERT take them.
    #[serde(default)]
    pub mentions: Vec<Mention>,
    /// Items the message is about.
    #[serde(default)]
    pub items: Vec<ItemKey>,
    /// The lane the message is about.
    pub lane: Option<LaneName>,
    /// Markdown text.
    pub body: Body,
    /// When the message stops being valid; required on CLAIM.
    pub valid_until: Option<Timestamp>,
    /// The client's clock when it sent the message; display only.
    pub sent_at: Timestamp,
    /// The verb and its data, as top-level `verb` and `data` fields.
    #[serde(flatten)]
    pub payload: Payload,
}

impl ClientEnvelope {
    /// Parses a raw publish from `up/`.
    ///
    /// ```
    /// # fn main() -> Result<(), agmen_proto::ProtoError> {
    /// use agmen_proto::{ClientEnvelope, NackReason};
    /// let raw = br#"{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"FYI","data":{},
    ///     "body":"hi","sent_at":"2026-10-25T16:04:11Z","from":"idn7k2q9"}"#;
    /// let err = ClientEnvelope::from_json(raw).unwrap_err();
    /// assert_eq!(NackReason::from(&err), NackReason::Schema);
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// [`ProtoError::EnvelopeTooLarge`] past [`MAX_ENVELOPE_BYTES`],
    /// [`ProtoError::BodyTooLarge`] for a body past
    /// [`MAX_BODY_BYTES`], [`ProtoError::UnknownField`] for a field v1 does
    /// not define, and [`ProtoError::Schema`] for anything else that does not
    /// match v1. `NackReason::from(&err)` gives the hub's answer.
    pub fn from_json(raw: &[u8]) -> Result<Self, ProtoError> {
        if raw.len() > MAX_ENVELOPE_BYTES {
            return Err(ProtoError::EnvelopeTooLarge {
                len: raw.len(),
                limit: MAX_ENVELOPE_BYTES,
            });
        }
        serde_json::from_slice::<WireClient>(raw)?.into_strict()
    }
}

impl<'de> Deserialize<'de> for ClientEnvelope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        WireClient::deserialize(deserializer)?
            .into_strict()
            .map_err(de::Error::custom)
    }
}

/// A message as the hub delivers it on `feed` or `inbox/`: the client's
/// envelope plus the fields the hub stamps.
///
/// Unlike `ClientEnvelope`, parsing ignores fields it does not know, so a
/// reader keeps working when a later minor version adds one (protocol
/// section 9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct StampedEnvelope {
    /// What the sender published.
    #[serde(flatten, deserialize_with = "lenient_client")]
    pub client: ClientEnvelope,
    /// The stream this copy belongs to.
    pub stream: Stream,
    /// Its number in that stream.
    pub seq: Seq,
    /// The sender, taken from the `up/` topic it published on.
    pub from: IdentityId,
    /// The sender's handle, for display.
    pub from_handle: String,
    /// The sender's kind.
    pub from_kind: IdentityKind,
    /// The identities this message interrupts: expanded mentions, plus reply
    /// authors where the protocol says so, never the sender.
    #[serde(default)]
    pub mentioned: Vec<IdentityId>,
    /// Links for what the body names.
    #[serde(default)]
    pub refs: Vec<Ref>,
    /// The hub's clock when it recorded the message.
    pub hub_at: Timestamp,
}

fn lenient_client<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ClientEnvelope, D::Error> {
    WireClient::deserialize(deserializer)?
        .into_lenient()
        .map_err(de::Error::custom)
}

/// The client fields as they arrive, before the body limit, the payload and
/// the unknown-field rule are checked. `serde(deny_unknown_fields)` does not
/// work with `flatten`, so unknown fields are collected and checked by hand.
/// `verb` is a plain string so the reader path can keep a verb it does not
/// know as `Payload::Unknown`.
#[derive(Deserialize)]
struct WireClient {
    v: u8,
    id: MessageId,
    #[serde(default)]
    reply_to: Option<MessageId>,
    #[serde(default, deserialize_with = "null_as_empty")]
    to: Vec<IdentityId>,
    #[serde(default)]
    mentions: Vec<Mention>,
    #[serde(default)]
    items: Vec<ItemKey>,
    #[serde(default)]
    lane: Option<LaneName>,
    body: String,
    #[serde(default)]
    valid_until: Option<Timestamp>,
    sent_at: Timestamp,
    verb: String,
    data: serde_json::Value,
    #[serde(flatten)]
    unknown: BTreeMap<String, IgnoredAny>,
}

fn null_as_empty<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<IdentityId>, D::Error> {
    Ok(Option::deserialize(deserializer)?.unwrap_or_default())
}

impl WireClient {
    /// The hub's ingest rules: no unknown fields and no unknown verbs.
    fn into_strict(self) -> Result<ClientEnvelope, ProtoError> {
        if let Some(name) = self.unknown.keys().next() {
            return Err(ProtoError::UnknownField(name.clone()));
        }
        let verb = self.verb.parse::<Verb>()?;
        self.into_envelope(Some(verb))
    }

    /// The reader's rules: unknown fields are ignored and an unknown verb
    /// becomes `Payload::Unknown`.
    fn into_lenient(self) -> Result<ClientEnvelope, ProtoError> {
        // An unknown verb is not an error here: it becomes Payload::Unknown.
        let verb = self.verb.parse::<Verb>().ok();
        self.into_envelope(verb)
    }

    /// Builds the envelope; `verb` is `self.verb` parsed, or `None` when this
    /// version does not know it.
    fn into_envelope(self, verb: Option<Verb>) -> Result<ClientEnvelope, ProtoError> {
        let body = Body::try_from(self.body)?;
        let payload = match verb {
            Some(verb) => {
                let mut tagged = serde_json::Map::new();
                tagged.insert("verb".to_owned(), verb.as_str().into());
                tagged.insert("data".to_owned(), self.data);
                serde_json::from_value(serde_json::Value::Object(tagged))?
            }
            None => Payload::Unknown {
                verb: self.verb,
                data: self.data,
            },
        };
        Ok(ClientEnvelope {
            v: self.v,
            id: self.id,
            reply_to: self.reply_to,
            to: self.to,
            mentions: self.mentions,
            items: self.items,
            lane: self.lane,
            body,
            valid_until: self.valid_until,
            sent_at: self.sent_at,
            payload,
        })
    }
}

/// Checks the rules a parsed envelope must follow before the hub records it,
/// judging `valid_until` by the hub's clock.
///
/// It checks the verb (`Payload::Unknown` is `schema`), the version, `to`
/// (1 to 8 recipients on DM, 0 to 8 on ASK, none elsewhere),
/// `reply_to` on the verbs that answer something, `valid_until` on CLAIM
/// with a 120 s skew allowance, and `mentions` (only on FYI, ASK, ANSWER and
/// ALERT, at most 8 identities). The body limit holds by construction of
/// [`Body`]. Which kinds may use group, lane and role mentions, and who may
/// send which verb, depend on the sender and are the hub's checks.
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use agmen_proto::{ClientEnvelope, NackReason, validate};
/// let raw = br#"{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"DM","data":{},
///     "to":[],"body":"hi","sent_at":"2026-10-25T16:04:11Z"}"#;
/// let env = ClientEnvelope::from_json(raw)?;
/// assert_eq!(validate(&env, "2026-10-25T16:04:12Z".parse()?), Err(NackReason::Schema));
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// [`NackReason::Schema`] for a rule the envelope breaks,
/// [`NackReason::ForbiddenMention`] for mentions on the wrong verb or more
/// than 8 identity mentions, and [`NackReason::Expired`] when `hub_now` is
/// more than 120 s past `valid_until`. For CLAIM the hub answers `Expired`
/// with DENY `expired` instead of a NACK.
pub fn validate(env: &ClientEnvelope, hub_now: Timestamp) -> Result<(), NackReason> {
    debug_assert!(
        env.body.as_str().len() <= MAX_BODY_BYTES,
        "Body enforces the limit"
    );
    // Only a StampedEnvelope parses to Payload::Unknown; the hub never takes one.
    let Some(verb) = env.payload.verb() else {
        return Err(NackReason::Schema);
    };
    if env.v != 1 {
        return Err(NackReason::Schema);
    }
    let recipients_ok = match verb {
        Verb::Dm => (1..=MAX_ADDRESSEES).contains(&env.to.len()),
        // An ASK with `to` is routed like a DM; without it, it goes to the feed.
        Verb::Ask => env.to.len() <= MAX_ADDRESSEES,
        _ => env.to.is_empty(),
    };
    if !recipients_ok {
        return Err(NackReason::Schema);
    }
    let needs_reply_to = matches!(
        verb,
        Verb::Answer
            | Verb::Ack
            | Verb::Delegated
            | Verb::Review
            | Verb::Decision
            | Verb::PushOk
            | Verb::PushNo
    );
    if needs_reply_to && env.reply_to.is_none() {
        return Err(NackReason::Schema);
    }
    if verb == Verb::Claim && env.valid_until.is_none() {
        return Err(NackReason::Schema);
    }
    let takes_mentions = matches!(verb, Verb::Fyi | Verb::Ask | Verb::Answer | Verb::Alert);
    if !env.mentions.is_empty() && !takes_mentions {
        return Err(NackReason::ForbiddenMention);
    }
    let identities = env
        .mentions
        .iter()
        .filter(|mention| matches!(mention, Mention::Identity(_)))
        .count();
    if identities > MAX_ADDRESSEES {
        return Err(NackReason::ForbiddenMention);
    }
    // A valid_until so late that adding the skew overflows never expires.
    if let Some(until) = env.valid_until
        && until
            .checked_add(VALID_UNTIL_SKEW)
            .is_ok_and(|deadline| hub_now > deadline)
    {
        return Err(NackReason::Expired);
    }
    Ok(())
}

#[cfg(test)]
#[path = "envelope_tests.rs"]
mod tests;
