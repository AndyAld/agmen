//! Wire types for the agmen protocol v1: envelope, verbs, topics.
//!
//! The normative spec is `docs/protocol.md`. This crate does no I/O and has
//! no async code; the hub, the client and the CLI share it.
//!
//! Times are [`Timestamp`], an alias for [`jiff::Timestamp`], serialized as
//! RFC 3339 in UTC.
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use agmen_proto::{Body, ClientEnvelope, IdentityId, Mention, Payload, validate};
//!
//! let to: IdentityId = "idn7k2q9".parse()?;
//! assert!("andy/sol".parse::<IdentityId>().is_err());
//! assert_eq!("all".parse::<Mention>()?, Mention::All);
//! assert!(Body::try_from("x".repeat(40_000)).is_err());
//!
//! let raw = br#"{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"DM","data":{},
//!     "to":["idn7k2q9"],"body":"hi","sent_at":"2026-10-25T16:04:11Z"}"#;
//! let env = ClientEnvelope::from_json(raw)?;
//! assert_eq!(env.to, [to]);
//! assert_eq!(env.payload, Payload::Dm {});
//! validate(&env, "2026-10-25T16:04:12Z".parse()?)?;
//! # Ok(()) }
//! ```
//!
//! The types derive [`schemars::JsonSchema`] and [`ts_rs::TS`]; the committed
//! JSON Schema and TypeScript in `schema/` come from them. Regenerate with
//! `cargo run -p agmen-proto --example export-schema`; the `schema_drift`
//! test fails when the committed files fall behind.
//!
//! Public enums are exhaustive on purpose: every crate that matches on them
//! lives in this workspace, and a new verb or reason should break those
//! matches until each one handles it.

#![warn(missing_docs)]

mod envelope;
mod error;
mod ids;
mod reply;
mod topics;
mod verbs;

pub use envelope::{
    ClientEnvelope, IdentityKind, MAX_ENVELOPE_BYTES, MAX_HELLO_FIELD_BYTES, Ref, RefKind,
    StampedEnvelope, validate,
};
pub use error::ProtoError;
pub use ids::{
    Body, GitSha, IdentityId, ItemKey, LaneName, MAX_BODY_BYTES, Mention, MessageId, ProjectId,
    RoleName, Seq, Stream, TenantId, Timestamp,
};
pub use reply::{DeliveryReply, NackReason};
pub use topics::{Topics, parse_up};
pub use verbs::{
    Change, Check, DenyReason, Finding, FindingSeverity, Needs, Outcome, Payload, Severity, Tokens,
    Urgency, Verb, Verdict,
};
