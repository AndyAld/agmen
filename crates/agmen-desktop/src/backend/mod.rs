//! What the window shows and what it can ask for, behind one small trait.
//!
//! [`Backend`] is the seam: [`FixtureBackend`] serves the mockup's data today,
//! and agmend's local API, the CLI and `agmen mcp` plug in behind the same
//! [`Draft`] and [`Action`] types later (ADR-0011). Messages cross it as
//! `agmen_proto::StampedEnvelope`, unchanged from the wire.

mod fixture;
mod fixture_data;
mod screen;

use std::collections::BTreeMap;

use agmen_proto::{GitSha, IdentityId, MessageId, StampedEnvelope};
use futures::channel::mpsc::UnboundedReceiver;

pub(crate) use fixture::FixtureBackend;
#[cfg(test)]
pub(crate) use fixture::chat_snapshot;
pub(crate) use screen::Screen;

/// What a backend can refuse.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BackendError {
    #[error("the message body is {len} bytes; the hub takes at most {max}")]
    BodyTooLarge { len: usize, max: usize },
    #[error("no change card {0}")]
    NoSuchChange(String),
    #[error("{0} cannot be done from this console yet")]
    Unsupported(&'static str),
    #[error("the built-in fixture is broken: {0}")]
    BadFixture(String),
}

/// The console's view of agmen: one snapshot, a way to send and act, and a
/// stream of what changes.
pub(crate) trait Backend: 'static {
    /// Everything the window shows now.
    fn snapshot(&self) -> Snapshot;
    /// Sends a message as the person.
    ///
    /// # Errors
    ///
    /// When the message cannot be sent, such as a body over the hub's limit.
    fn send(&self, draft: Draft) -> Result<(), BackendError>;
    /// Does something that is not a chat message.
    ///
    /// # Errors
    ///
    /// When the action names something that does not exist or is not possible.
    fn act(&self, action: Action) -> Result<(), BackendError>;
    /// Updates from now on, until the receiver is dropped.
    fn subscribe(&self) -> UnboundedReceiver<Update>;
}

/// The app's backend as a GPUI global, so any view can read and act
/// through it.
pub(crate) struct BackendHandle(pub(crate) std::rc::Rc<dyn Backend>);

impl gpui::Global for BackendHandle {}

/// A chat message the person writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Draft {
    /// Who it is addressed to; the listener unless the person picks.
    pub(crate) to: Vec<IdentityId>,
    pub(crate) body: String,
    pub(crate) reply_to: Option<MessageId>,
}

/// Something the person does that is not a chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum Action {
    Apply(ChangeId),
    Cancel(ChangeId),
    Undo(ChangeId),
    TakeOver(IdentityId),
    HandBack(IdentityId),
    SwitchAgent { worker: IdentityId, agent: String },
    AnswerAlert { alert: MessageId, choice: String },
    SnoozeAlert(MessageId),
    ApproveSpec { path: String, sha: GitSha },
    ApproveShare,
    KickOff,
}

/// What a subscriber hears.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Update {
    Message(Box<StampedEnvelope>),
    Snapshot(Box<Snapshot>),
}

/// A settings change the listener proposed or made (spec 4).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ChangeId(pub(crate) String);

impl std::fmt::Display for ChangeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ChangeState {
    /// A widening change waiting for the person's Apply.
    Pending,
    Applied,
    Cancelled,
    /// Applied, then undone by the person (Andy, review C93).
    Undone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangeRow {
    pub(crate) target: String,
    pub(crate) setting: String,
    pub(crate) before: String,
    pub(crate) after: String,
}

/// The card under a listener message that changed or wants to change settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangeCard {
    pub(crate) id: ChangeId,
    pub(crate) requested_by: String,
    /// Widening changes wait for Apply; narrowing ones apply at once.
    pub(crate) widens: bool,
    pub(crate) rows: Vec<ChangeRow>,
    pub(crate) state: ChangeState,
}

/// One member of a plan's team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeamRow {
    pub(crate) role: String,
    pub(crate) role_label: String,
    pub(crate) handle: String,
    /// Agent, and model when the plan names one: "claude · sonnet".
    pub(crate) harness: String,
    pub(crate) lane: String,
    pub(crate) owner: String,
    pub(crate) usd_per_day: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Approval {
    Approved,
    Waiting,
}

/// A spec or plan file and who approved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpecState {
    pub(crate) path: String,
    /// The commit approvals refer to (spec 7: approvers read the exact commit).
    pub(crate) sha: GitSha,
    /// Shown instead of approvals while the file is still a draft.
    pub(crate) draft_note: Option<String>,
    pub(crate) approvals: BTreeMap<String, Approval>,
}

impl SpecState {
    pub(crate) fn approved_by_all(&self) -> bool {
        self.draft_note.is_none() && self.approvals.values().all(|a| *a == Approval::Approved)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaneInfo {
    pub(crate) name: String,
    pub(crate) paths: Vec<String>,
}

/// A planning room's plan (spec 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanInfo {
    pub(crate) name: String,
    pub(crate) version: u32,
    pub(crate) goal: String,
    pub(crate) team: Vec<TeamRow>,
    pub(crate) specs: Vec<SpecState>,
    pub(crate) lanes: Vec<LaneInfo>,
    pub(crate) repo_exists: bool,
    /// Whether the plan branch is merged into the default branch.
    pub(crate) branch_merged: bool,
    /// Per owner: whether they approved their own agents and spend.
    pub(crate) shares_approved: BTreeMap<String, bool>,
    pub(crate) running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AgentStatus {
    Working,
    Idle,
    Reading,
}

/// One agent in the Agents dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentRow {
    pub(crate) id: IdentityId,
    /// The full handle, such as `andy/fx1`.
    pub(crate) handle: String,
    /// The name after the slash, such as `fx1`.
    pub(crate) name: String,
    /// The lowercase role name, such as `engineer`.
    pub(crate) role: String,
    /// The harness that runs it, such as `claude`.
    pub(crate) harness: String,
    pub(crate) status: AgentStatus,
    /// How far the row is nested under the agent that delegated to it.
    pub(crate) depth: u8,
}

/// One person and their agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersonGroup {
    pub(crate) person: String,
    /// The person running this app, shown as "andy (you)".
    pub(crate) you: bool,
    pub(crate) agents: Vec<AgentRow>,
}

/// One line of the listener's recent actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecentAction {
    pub(crate) time: String,
    pub(crate) text: String,
    pub(crate) kind: RecentKind,
}

/// What a listener's recent action was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RecentKind {
    /// It started a worker.
    Delegated,
    /// It answered from what it looked up.
    Answered,
    /// A settings change waits for the person.
    Waiting,
}

/// The right dock's Listener panel.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ListenerInfo {
    pub(crate) handle: String,
    pub(crate) agent: String,
    pub(crate) state: String,
    pub(crate) harness_label: String,
    pub(crate) role_label: String,
    pub(crate) can_use: Vec<String>,
    pub(crate) never: Vec<String>,
    pub(crate) takes_work_from: String,
    pub(crate) per_worker_usd: f64,
    pub(crate) workers_today_usd: f64,
    pub(crate) workers_cap_usd: f64,
    pub(crate) workers_running: u32,
    pub(crate) workers_max: u32,
    pub(crate) listener_today_usd: f64,
    pub(crate) listener_cap_usd: f64,
    pub(crate) tokens_in: u64,
    pub(crate) tokens_out: u64,
    pub(crate) uptime_minutes: u32,
    pub(crate) recent: Vec<RecentAction>,
}

/// The right dock's Agent panel, for one selected agent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AgentInfo {
    pub(crate) id: IdentityId,
    /// The role's name, such as `engineer`.
    pub(crate) role: String,
    pub(crate) status: AgentStatus,
    pub(crate) handle: String,
    pub(crate) role_label: String,
    pub(crate) harness_label: String,
    pub(crate) model: String,
    pub(crate) delegated_by: Option<String>,
    pub(crate) state: String,
    pub(crate) tools: Vec<String>,
    pub(crate) edits: String,
    pub(crate) verbs: Vec<String>,
    pub(crate) refused: Vec<String>,
    pub(crate) branch: String,
    pub(crate) base: String,
    pub(crate) worktree_state: String,
    pub(crate) cleanup: String,
    pub(crate) spent_usd: f64,
    pub(crate) budget_usd: f64,
    pub(crate) since_launch_minutes: u32,
    pub(crate) working_minutes: u32,
    pub(crate) tokens_in: u64,
    pub(crate) tokens_out: u64,
    pub(crate) cache_read: u64,
}

/// The status bar's counts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatusSummary {
    pub(crate) needs_you: u32,
    pub(crate) online: u32,
    pub(crate) alerts: u32,
    pub(crate) worktrees: u32,
    pub(crate) spend_today_usd: f64,
}

/// Everything the window shows at one moment.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) project: String,
    /// The person's handle, such as `andy`.
    pub(crate) me: String,
    pub(crate) me_id: IdentityId,
    pub(crate) listener_id: IdentityId,
    pub(crate) roster: Vec<PersonGroup>,
    pub(crate) listener: ListenerInfo,
    /// Keyed by identity id.
    pub(crate) agents: BTreeMap<IdentityId, AgentInfo>,
    pub(crate) status: StatusSummary,
    /// The project chat, oldest first.
    pub(crate) messages: Vec<StampedEnvelope>,
    /// Change cards, keyed by the listener message they belong to.
    pub(crate) changes: BTreeMap<MessageId, ChangeCard>,
    pub(crate) plan: Option<PlanInfo>,
}
