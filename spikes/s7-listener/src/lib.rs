//! Offline infrastructure for spike S7's listener measurements.

#![warn(missing_docs)]

pub mod harness;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The worker tier requested by a listener.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Normal work, the listener's default.
    Standard,
    /// Work marked hard or a failed standard attempt.
    Deep,
}

/// The refreshed listener delegation schema, including a required role.
/// `agent` is optional so the launcher's configured default can apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateArgs {
    /// The assignment that authorized this delegation.
    pub reply_to: String,
    /// The worker's task.
    pub brief: String,
    /// The enforced worker role, for example engineer or reviewer.
    pub role: String,
    /// The installed agent to use, or the launcher's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The model tier.
    pub tier: Tier,
    /// The worker's dollar cap. The fake records it without spending it.
    pub budget_usd: f64,
    /// The assigned item, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The assigned lane, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// The worktree's base ref, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The worker's branch, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}
