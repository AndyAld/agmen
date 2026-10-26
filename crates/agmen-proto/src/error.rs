//! The crate's one error type.

use crate::ids::MAX_BODY_BYTES;

/// Why a value or an envelope could not be parsed.
///
/// `NackReason::from(&err)` gives the reason the hub answers with.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    /// A string does not follow the format its type requires.
    #[error("{what} must match {rule}")]
    Malformed {
        /// The type, such as "identity id".
        what: &'static str,
        /// The pattern it must match.
        rule: &'static str,
    },
    /// A body over [`MAX_BODY_BYTES`](crate::MAX_BODY_BYTES) after UTF-8 encoding.
    #[error("body is {len} bytes, over the {MAX_BODY_BYTES} byte limit")]
    BodyTooLarge {
        /// The body's length in bytes.
        len: usize,
    },
    /// A raw envelope over [`MAX_ENVELOPE_BYTES`](crate::MAX_ENVELOPE_BYTES).
    #[error("envelope is {len} bytes, over the {limit} byte limit")]
    EnvelopeTooLarge {
        /// The envelope's length in bytes.
        len: usize,
        /// The limit it broke.
        limit: usize,
    },
    /// A client envelope carries a field that protocol v1 does not define.
    #[error("unknown field `{0}`")]
    UnknownField(String),
    /// A verb name that protocol v1 does not define. A client envelope with
    /// one is refused; a stamped envelope keeps it as `Payload::Unknown`.
    #[error("unknown verb `{0}`")]
    UnknownVerb(String),
    /// The JSON does not match protocol v1.
    #[error("envelope does not match protocol v1")]
    Schema(#[from] serde_json::Error),
    /// A topic is not an `agmen/v1/{tenant}/{project}/up/{identity}` topic.
    #[error("not an agmen/v1 up topic")]
    NotUpTopic,
}
