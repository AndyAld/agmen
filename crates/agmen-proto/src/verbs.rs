//! Verbs and their `data` (protocol section 4).
//!
//! Every verb in the spec has a type here. The MVP hub handles HELLO, BYE,
//! FYI, ASK, ANSWER, ACK, STATUS, ROLLCALL, ALERT, DM, CLAIM, GRANT, DENY,
//! HEARTBEAT, RELEASE, ASSIGN, FOUND, ITEM and DONE; the others exist so
//! clients and the hub agree on their shape before they are handled.
//!
//! `data` objects ignore fields they do not know, so a reader keeps working
//! when a later minor version adds an optional field (protocol section 9).

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{GitSha, IdentityId, ItemKey, LaneName, RoleName, Seq, Timestamp};

/// A verb and its `data`. Serializes as `{"verb": "CLAIM", "data": {...}}`;
/// verbs without data carry `{}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "verb", content = "data", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Payload {
    /// Joins the room; the hub updates the roster.
    Hello {
        /// Client name and version, such as `agmen-cli/0.1.0`.
        client: String,
        /// Coding agent and version, such as `claude-code/2.1.310`.
        agent: Option<String>,
        /// Model the agent runs.
        model: Option<String>,
        /// What the sender is looking at, shown in the roster.
        looking_at: Option<String>,
    },
    /// Leaves the room; the hub sets presence offline.
    Bye {},
    /// A chat message: the text is in `body`.
    Fyi {},
    /// A question; goes to the feed.
    Ask {
        /// Who should answer, when one identity should.
        of: Option<IdentityId>,
    },
    /// An answer; `reply_to` names the ASK.
    Answer {},
    /// A social acknowledgement (not the delivery ACK); `reply_to` is required.
    Ack {},
    /// A progress report.
    Status {
        /// One line on where the work stands.
        summary: String,
        /// Items the report covers.
        #[serde(default)]
        items: Vec<ItemKey>,
        /// Dollars spent so far.
        spend_usd: Option<f64>,
        /// Hours worked so far.
        hours: Option<f64>,
        /// Tokens used so far.
        tokens: Option<Tokens>,
    },
    /// Asks who is here; the hub answers from presence.
    Rollcall {},
    /// Asks for an item; `valid_until` is required.
    Claim {
        /// The item to claim.
        item: ItemKey,
    },
    /// The hub's yes to a CLAIM.
    Grant {
        /// The claimed item.
        item: ItemKey,
        /// When the lease ends unless a HEARTBEAT renews it.
        lease_until: Timestamp,
    },
    /// The hub's no to a CLAIM.
    Deny {
        /// The item that was asked for.
        item: ItemKey,
        /// Why the claim was refused.
        reason: DenyReason,
    },
    /// Renews the leases on held items; not posted to the feed.
    Heartbeat {
        /// The held items.
        items: Vec<ItemKey>,
    },
    /// Gives up a claim.
    Release {
        /// The released item.
        item: ItemKey,
        /// Why, such as `lease_expired` when the hub releases it.
        reason: Option<String>,
    },
    /// A coordinator hands an item to an identity.
    Assign {
        /// The item.
        item: ItemKey,
        /// The assignee.
        to: IdentityId,
    },
    /// A listener started a worker; `reply_to` is required.
    Delegated {
        /// The worker's identity.
        worker: IdentityId,
        /// The coding agent the worker runs.
        agent: String,
        /// The worker's role.
        role: RoleName,
        /// The model the worker runs.
        model: String,
        /// The tier the model came from, such as `standard`.
        tier: String,
        /// The worker's budget in dollars.
        budget_usd: f64,
        /// The item the worker is on.
        item: Option<ItemKey>,
    },
    /// A lane owner lends paths to an identity until a time.
    Lend {
        /// The lane.
        lane: LaneName,
        /// Globs within the lane.
        paths: Vec<String>,
        /// Who may edit them.
        to: IdentityId,
        /// When the lend ends.
        until: Timestamp,
    },
    /// Reports a new problem; the hub answers with ITEM.
    Found {
        /// A short title.
        title: String,
        /// The lane it belongs to.
        lane: Option<LaneName>,
        /// How bad it is.
        severity: Option<Severity>,
    },
    /// The hub minted an item key for a FOUND.
    Item {
        /// The new key.
        key: ItemKey,
    },
    /// Finished work on a claimed item.
    Done {
        /// The item.
        item: ItemKey,
        /// The branch holding the work.
        branch: String,
        /// The commit with the work.
        commit: GitSha,
        /// The commit the work started from.
        base_sha: GitSha,
        /// Check results.
        checks: Vec<Check>,
        /// Changed paths.
        changes: Vec<Change>,
    },
    /// A reviewer's verdict on a DONE; `reply_to` names the DONE.
    Review {
        /// Pass or changes.
        verdict: Verdict,
        /// What the reviewer found.
        findings: Vec<Finding>,
        /// The paths the reviewer traced.
        traced: Option<String>,
    },
    /// Options for a decision.
    Proposal {
        /// The options.
        options: Vec<String>,
        /// When the decision is due.
        deadline: Option<Timestamp>,
    },
    /// A planner's plan revision.
    Plan {
        /// The revision number.
        version: u32,
        /// The plan file's path.
        path: String,
        /// The commit holding that revision.
        sha: GitSha,
    },
    /// GO or VETO on a proposal, spec or plan; `reply_to` is required.
    Decision {
        /// GO or VETO.
        outcome: Outcome,
        /// The chosen option's index in the PROPOSAL.
        option: Option<u32>,
        /// The spec file and commit a person approves.
        subject: Option<String>,
    },
    /// A ruling from a coordinator or human.
    Ruling {
        /// What was ruled.
        subject: String,
    },
    /// An agent asks its owner to approve a push.
    PushRequest {
        /// The git remote.
        remote: String,
        /// The ref to push.
        #[serde(rename = "ref")]
        #[ts(rename = "ref")]
        git_ref: String,
        /// The commit to push.
        sha: GitSha,
    },
    /// The owner approves a PUSH_REQUEST; `reply_to` is required.
    PushOk {},
    /// The owner refuses a PUSH_REQUEST; `reply_to` is required.
    PushNo {},
    /// Asks people for attention, at high priority.
    Alert {
        /// What the sender needs.
        need: String,
        /// Normal or urgent.
        #[serde(default)]
        urgency: Urgency,
        /// Who can clear it.
        #[serde(default)]
        needs: Needs,
        /// Actions the sender offers, such as `Revert #10`.
        #[serde(default)]
        actions: Vec<String>,
    },
    /// A direct message to the identities in `to`.
    Dm {},
    /// The hub's notice that a retained `state/` head changed.
    State {
        /// The state key, such as `roster`.
        key: String,
        /// The key's version.
        v: u64,
        /// The feed seq the state reflects.
        as_of_seq: Seq,
    },
}

/// A verb name without its data, such as `CLAIM`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[expect(
    missing_docs,
    reason = "each variant is the verb of the same name in Payload"
)]
pub enum Verb {
    Hello,
    Bye,
    Fyi,
    Ask,
    Answer,
    Ack,
    Status,
    Rollcall,
    Claim,
    Grant,
    Deny,
    Heartbeat,
    Release,
    Assign,
    Delegated,
    Lend,
    Found,
    Item,
    Done,
    Review,
    Proposal,
    Plan,
    Decision,
    Ruling,
    PushRequest,
    PushOk,
    PushNo,
    Alert,
    Dm,
    State,
}

impl Verb {
    /// Every verb, in the order protocol section 4 lists them.
    pub const ALL: [Self; 30] = [
        Self::Hello,
        Self::Bye,
        Self::Fyi,
        Self::Ask,
        Self::Answer,
        Self::Ack,
        Self::Status,
        Self::Rollcall,
        Self::Claim,
        Self::Grant,
        Self::Deny,
        Self::Heartbeat,
        Self::Release,
        Self::Assign,
        Self::Delegated,
        Self::Lend,
        Self::Found,
        Self::Item,
        Self::Done,
        Self::Review,
        Self::Proposal,
        Self::Plan,
        Self::Decision,
        Self::Ruling,
        Self::PushRequest,
        Self::PushOk,
        Self::PushNo,
        Self::Alert,
        Self::Dm,
        Self::State,
    ];

    /// The wire name, such as `PUSH_REQUEST`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "HELLO",
            Self::Bye => "BYE",
            Self::Fyi => "FYI",
            Self::Ask => "ASK",
            Self::Answer => "ANSWER",
            Self::Ack => "ACK",
            Self::Status => "STATUS",
            Self::Rollcall => "ROLLCALL",
            Self::Claim => "CLAIM",
            Self::Grant => "GRANT",
            Self::Deny => "DENY",
            Self::Heartbeat => "HEARTBEAT",
            Self::Release => "RELEASE",
            Self::Assign => "ASSIGN",
            Self::Delegated => "DELEGATED",
            Self::Lend => "LEND",
            Self::Found => "FOUND",
            Self::Item => "ITEM",
            Self::Done => "DONE",
            Self::Review => "REVIEW",
            Self::Proposal => "PROPOSAL",
            Self::Plan => "PLAN",
            Self::Decision => "DECISION",
            Self::Ruling => "RULING",
            Self::PushRequest => "PUSH_REQUEST",
            Self::PushOk => "PUSH_OK",
            Self::PushNo => "PUSH_NO",
            Self::Alert => "ALERT",
            Self::Dm => "DM",
            Self::State => "STATE",
        }
    }
}

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Payload {
    /// The payload's verb.
    #[must_use]
    pub const fn verb(&self) -> Verb {
        match self {
            Self::Hello { .. } => Verb::Hello,
            Self::Bye {} => Verb::Bye,
            Self::Fyi {} => Verb::Fyi,
            Self::Ask { .. } => Verb::Ask,
            Self::Answer {} => Verb::Answer,
            Self::Ack {} => Verb::Ack,
            Self::Status { .. } => Verb::Status,
            Self::Rollcall {} => Verb::Rollcall,
            Self::Claim { .. } => Verb::Claim,
            Self::Grant { .. } => Verb::Grant,
            Self::Deny { .. } => Verb::Deny,
            Self::Heartbeat { .. } => Verb::Heartbeat,
            Self::Release { .. } => Verb::Release,
            Self::Assign { .. } => Verb::Assign,
            Self::Delegated { .. } => Verb::Delegated,
            Self::Lend { .. } => Verb::Lend,
            Self::Found { .. } => Verb::Found,
            Self::Item { .. } => Verb::Item,
            Self::Done { .. } => Verb::Done,
            Self::Review { .. } => Verb::Review,
            Self::Proposal { .. } => Verb::Proposal,
            Self::Plan { .. } => Verb::Plan,
            Self::Decision { .. } => Verb::Decision,
            Self::Ruling { .. } => Verb::Ruling,
            Self::PushRequest { .. } => Verb::PushRequest,
            Self::PushOk {} => Verb::PushOk,
            Self::PushNo {} => Verb::PushNo,
            Self::Alert { .. } => Verb::Alert,
            Self::Dm {} => Verb::Dm,
            Self::State { .. } => Verb::State,
        }
    }
}

/// Token counts in a STATUS.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema, TS,
)]
pub struct Tokens {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Tokens read from the prompt cache.
    pub cache_read: u64,
}

/// Why the hub refused a CLAIM.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// Another identity holds the item.
    HeldBy,
    /// The CLAIM arrived after its `valid_until`.
    Expired,
    /// The item is outside the claimant's lane.
    NotInLane,
    /// The item is closed.
    Closed,
}

/// How bad a FOUND problem is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Low.
    Low,
    /// Medium.
    Med,
    /// High.
    High,
    /// A security problem: the feed shows only its key and lane.
    Security,
}

/// One check result in a DONE.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
pub struct Check {
    /// The check's name, such as `nextest`.
    pub name: String,
    /// Tests that passed.
    pub passed: u32,
    /// Tests that failed.
    pub failed: u32,
    /// Tests that failed on `base_sha`, when known.
    pub baseline: Option<u32>,
}

/// One changed path in a DONE.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
pub struct Change {
    /// The `git diff --name-status` letter, such as `M` or `A`.
    pub status: String,
    /// The path, relative to the repository root.
    pub path: String,
}

/// A REVIEW's verdict.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The work passes.
    Pass,
    /// The work needs changes.
    Changes,
}

/// One finding in a REVIEW.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
pub struct Finding {
    /// How bad it is.
    pub severity: FindingSeverity,
    /// The file.
    pub path: String,
    /// The line, when one applies.
    pub line: Option<u32>,
    /// What is wrong.
    pub what: String,
    /// The input or state that shows it.
    pub trigger: String,
}

/// How bad a REVIEW finding is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    /// Low.
    Low,
    /// Medium.
    Med,
    /// High.
    High,
}

/// A DECISION's outcome.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    /// Go ahead.
    Go,
    /// Stop.
    Veto,
}

/// An ALERT's urgency.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
    TS,
)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    /// A notification.
    #[default]
    Normal,
    /// Adds the chime and the needs-you highlight.
    Urgent,
}

/// Who can clear an ALERT.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
    TS,
)]
#[serde(rename_all = "snake_case")]
pub enum Needs {
    /// The first answer from the owner or a coordinator clears it.
    #[default]
    Anyone,
    /// It stays open for the owner after a coordinator answers.
    Owner,
}

#[cfg(test)]
#[path = "verbs_tests.rs"]
mod tests;
