//! Delivery replies on `ack/{identity_id}` (protocol section 5).

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ProtoError;
use crate::ids::{MessageId, Seq, Stream};

/// The hub's answer to one publish on `up/`. Serializes as
/// `{"verb": "ACK", "id": ..., "stream": ..., "seq": ...}` or
/// `{"verb": "NACK", "id": ..., "retryable": ..., "retry_after_ms": ..., "reason": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "verb", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DeliveryReply {
    /// The message is recorded at `seq` in `stream`.
    Ack {
        /// The envelope's id.
        id: MessageId,
        /// Where the hub recorded it. A DM is recorded in the sender's inbox.
        stream: Stream,
        /// Its number in that stream.
        seq: Seq,
    },
    /// The message was not recorded.
    Nack {
        /// The envelope's id.
        id: MessageId,
        /// Whether the client should send it again with the same id.
        retryable: bool,
        /// How long to wait before retrying, when the hub says.
        retry_after_ms: Option<u64>,
        /// Why.
        reason: NackReason,
    },
}

/// Why the hub refused a message (protocol section 5.4).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum NackReason {
    /// Slow down; honor `retry_after_ms`.
    RateLimited,
    /// A transient server condition.
    HubBusy,
    /// The envelope or its `data` does not match v1.
    Schema,
    /// The body is over 32,768 bytes.
    BodyTooLarge,
    /// The message arrived after `valid_until`.
    Expired,
    /// The sender's kind or role may not send this verb.
    ForbiddenVerb,
    /// A mention the sender may not use, more than 8 identities, or mentions
    /// on a verb that does not take them.
    ForbiddenMention,
    /// Text matched a credential pattern; fix it and resend with a new id.
    SecretDetected,
    /// The same id was reused with different content.
    Conflict,
    /// The sender was revoked.
    IdentityRevoked,
    /// A LEND from an identity that neither owns the lane nor coordinates.
    NotLaneOwner,
    /// A PUSH_REQUEST while push approval is `off`.
    NotRequired,
    /// The group used its message quota for the UTC day.
    QuotaExceeded,
    /// The group is read-only until its owner pays.
    ReadOnly,
    /// A reason this client does not recognize; final and never sent by a hub.
    #[serde(other)]
    Unknown,
}

impl NackReason {
    /// Whether a client should retry after this reason. Only `rate_limited`
    /// and `hub_busy` are retryable; the rest are final.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::RateLimited | Self::HubBusy)
    }

    /// The wire name, such as `body_too_large`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::HubBusy => "hub_busy",
            Self::Schema => "schema",
            Self::BodyTooLarge => "body_too_large",
            Self::Expired => "expired",
            Self::ForbiddenVerb => "forbidden_verb",
            Self::ForbiddenMention => "forbidden_mention",
            Self::SecretDetected => "secret_detected",
            Self::Conflict => "conflict",
            Self::IdentityRevoked => "identity_revoked",
            Self::NotLaneOwner => "not_lane_owner",
            Self::NotRequired => "not_required",
            Self::QuotaExceeded => "quota_exceeded",
            Self::ReadOnly => "read_only",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for NackReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for NackReason {}

impl From<&ProtoError> for NackReason {
    /// The reason the hub answers when an envelope fails to parse: an
    /// oversize body is `body_too_large`, anything else is `schema`.
    fn from(err: &ProtoError) -> Self {
        match err {
            ProtoError::BodyTooLarge { .. } => Self::BodyTooLarge,
            ProtoError::Malformed { .. }
            | ProtoError::EnvelopeTooLarge { .. }
            | ProtoError::UnknownField(_)
            | ProtoError::UnknownVerb(_)
            | ProtoError::Schema(_)
            | ProtoError::NotUpTopic => Self::Schema,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn new_reasons_are_final() {
        for (reason, name) in [
            (NackReason::QuotaExceeded, "quota_exceeded"),
            (NackReason::ReadOnly, "read_only"),
        ] {
            assert!(!reason.is_retryable());
            assert_eq!(reason.as_str(), name);
        }
    }

    #[test]
    fn unknown_nack_reason_parses() {
        let reason: NackReason = serde_json::from_value(json!("some_future_reason")).unwrap();
        assert_eq!(reason, NackReason::Unknown);
        assert!(!reason.is_retryable());
        assert_eq!(reason.as_str(), "unknown");
    }

    #[test]
    fn reply_wire_shape() {
        let id: MessageId = "01JBX6Q4T7W3C2M8Y9K5N0P1RS".parse().unwrap();
        let ack = DeliveryReply::Ack {
            id: id.clone(),
            stream: Stream::Feed,
            seq: Seq(7),
        };
        assert_eq!(
            serde_json::to_value(&ack).unwrap(),
            json!({"verb": "ACK", "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS", "stream": "feed", "seq": 7})
        );
        let nack = DeliveryReply::Nack {
            id,
            retryable: false,
            retry_after_ms: None,
            reason: NackReason::BodyTooLarge,
        };
        let value = serde_json::to_value(&nack).unwrap();
        assert_eq!(value["reason"], json!("body_too_large"));
        assert_eq!(
            serde_json::from_value::<DeliveryReply>(value).unwrap(),
            nack
        );
    }

    #[test]
    fn reason_names_match_serde() {
        for reason in [
            NackReason::RateLimited,
            NackReason::HubBusy,
            NackReason::Schema,
            NackReason::BodyTooLarge,
            NackReason::Expired,
            NackReason::ForbiddenVerb,
            NackReason::ForbiddenMention,
            NackReason::SecretDetected,
            NackReason::Conflict,
            NackReason::IdentityRevoked,
            NackReason::NotLaneOwner,
            NackReason::NotRequired,
            NackReason::QuotaExceeded,
            NackReason::ReadOnly,
            NackReason::Unknown,
        ] {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                json!(reason.as_str())
            );
        }
    }

    #[test]
    fn only_transient_reasons_retry() {
        assert!(NackReason::RateLimited.is_retryable());
        assert!(NackReason::HubBusy.is_retryable());
        assert!(!NackReason::Schema.is_retryable());
        assert!(!NackReason::IdentityRevoked.is_retryable());
    }
}
