//! Types for the desktop console: a snapshot of a project's chat, then events.

use agmen_proto::{IdentityId, IdentityKind, StampedEnvelope};
use serde::{Deserialize, Serialize};

/// Who the console acts as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    /// The identity id.
    pub id: IdentityId,
    /// The handle, for display.
    pub handle: String,
}

/// Presence, as in the hub's `state/roster` (protocol section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Connected and working.
    Online,
    /// Connected, but idle.
    Away,
    /// Not connected, or never was.
    Offline,
}

/// One row of `state/roster`. Optional fields are left out when unset, as the
/// hub leaves them out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterEntry {
    /// The identity id.
    pub identity_id: IdentityId,
    /// The handle, for display.
    pub handle: String,
    /// What kind of identity it is.
    pub kind: IdentityKind,
    /// Whether it is connected.
    pub status: Status,
    /// The coding agent its latest HELLO named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The model its latest HELLO named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The listener that started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_by: Option<IdentityId>,
}

/// The console's starting state for one project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsoleSnapshot {
    /// The project name.
    pub project: String,
    /// The acting identity.
    pub me: Me,
    /// Who is in the project.
    pub roster: Vec<RosterEntry>,
    /// Recent messages, oldest first.
    pub messages: Vec<StampedEnvelope>,
    /// Whether older messages exist.
    pub older: bool,
}

/// A change pushed after the snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "the brief fixes Message(StampedEnvelope); events are moved once, not stored in bulk"
)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleEvent {
    /// A new message.
    Message(StampedEnvelope),
    /// The roster changed; this is the whole new roster.
    Roster(Vec<RosterEntry>),
    /// The daemon's hub connection went up or down.
    Hub {
        /// Whether the hub is reachable.
        connected: bool,
    },
}
