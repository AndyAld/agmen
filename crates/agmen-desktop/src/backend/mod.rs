//! What the window shows and what it can ask for, behind one small trait.
//!
//! [`Backend`] is the seam: [`FixtureBackend`] serves the mockup's data today,
//! and agmend's local API, the CLI and `agmen mcp` plug in behind the same
//! [`Draft`] and [`Action`] types later (ADR-0011). Messages cross it as
//! `agmen_proto::StampedEnvelope`, unchanged from the wire.

// Task 6: the live backend over agmend's socket.
#[cfg_attr(not(test), expect(dead_code, reason = "Task 7 opens the app on it"))]
mod agmend;
mod fixture;
mod fixture_data;
// Task 19: the planning room's spec texts, comments, caps and presence.
mod fixture_plan;
mod screen;
mod worktree;
// Task 15: the models each agent offers and the model a message ran on.
mod fixture_models;
mod models;
pub(crate) use models::{ChoiceScope, Effort, ModelChoice, ModelGroup};

use std::collections::BTreeMap;

use agmen_proto::{GitSha, IdentityId, MessageId, Seq, StampedEnvelope, Stream, Timestamp};
use futures::channel::mpsc::UnboundedReceiver;
use gpui::SharedString;

use crate::roles::{Edits, RoleKind, Tier};

#[expect(unused_imports, reason = "Task 7 opens the app on it")]
pub(crate) use agmend::AgmendBackend;
pub(crate) use fixture::FixtureBackend;
pub(crate) use fixture_data::REVIEWER_XML;
pub(crate) use worktree::{
    ChangeKind, CommitInfo, DiffLine, DiffLineKind, FileChange, FileText, GutterMark, Hunk,
    WorktreeHead, WorktreeInfo, WorktreeState,
};

/// The identity id the fixture gives a mockup handle, for tests.
#[cfg(test)]
pub(crate) fn fixture_id(handle: &str) -> IdentityId {
    fixture_data::id(handle).unwrap_or_else(|err| panic!("{err}"))
}
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
    /// The agent is missing, outdated or of unknown version;
    /// `why` is [`InstalledAgent::not_ready`]'s text.
    #[error("{agent} cannot run a worker: {why}")]
    AgentNotReady { agent: String, why: String },
    #[error("the built-in fixture is broken: {0}")]
    BadFixture(String),
    // Task 6: the live backend's refusals.
    /// agmend refused the request, or its socket is not the person's own;
    /// the text says which.
    #[cfg_attr(not(test), expect(dead_code, reason = "Task 7 opens the app on it"))]
    #[error("{0}")]
    Agmend(String),
    /// agmend cannot be reached now, or its hub stream is down.
    #[cfg_attr(not(test), expect(dead_code, reason = "Task 7 opens the app on it"))]
    #[error("not connected to agmend, so nothing was sent")]
    NotConnected,
}

/// The console's view of agmen: one snapshot, a way to send and act, and a
/// stream of what changes.
pub(crate) trait Backend: 'static {
    /// Everything the window shows now.
    fn snapshot(&self) -> Snapshot;
    /// Sends a message as the person. A draft with a model comes back in an
    /// [`Update::Snapshot`] whose `message_models` carries its tag; one
    /// without comes back as an [`Update::Message`].
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

impl BackendHandle {
    /// Sends `draft` through the app's backend: the one way a view sends.
    ///
    /// # Errors
    ///
    /// When the backend refuses the draft, or the app has no backend.
    pub(crate) fn send(draft: Draft, cx: &gpui::App) -> Result<(), BackendError> {
        match cx.try_global::<Self>() {
            Some(backend) => backend.0.send(draft),
            None => Err(BackendError::Unsupported("sending without a backend")),
        }
    }
}

/// A chat message the person writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Draft {
    /// Who it is addressed to; the listener unless the person picks.
    pub(crate) to: Vec<IdentityId>,
    pub(crate) body: String,
    /// The message it answers. A reply to an ASK or an ALERT goes out as
    /// an ANSWER, any other message as an FYI.
    pub(crate) reply_to: Option<MessageId>,
    /// The model the listener runs this message on (Task 15); `None` runs
    /// it on the listener's own settings.
    pub(crate) model: Option<ModelChoice>,
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
    SwitchAgent {
        worker: IdentityId,
        agent: String,
    },
    AnswerAlert {
        alert: MessageId,
        choice: String,
    },
    /// Looks for terminal agents again (`agmen agents scan`, Task 14).
    ScanAgents,
    SnoozeAlert(MessageId),
    ApproveSpec {
        path: String,
        sha: GitSha,
    },
    // Only the fixture's tests send ApproveShare; no view does yet.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "no view sends the share approval yet")
    )]
    ApproveShare,
    KickOff,
    // Task 18: a worker on an installed agent, from the command palette.
    SpawnWorker {
        agent: String,
    },
}

/// What a subscriber hears.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Update {
    Message(Box<StampedEnvelope>),
    Snapshot(Box<Snapshot>),
    // Task 6: what only the live backend says.
    /// How the link to agmend and the hub stands now. A new subscriber
    /// hears the current state first; Task 7 shows it in the status bar.
    #[cfg_attr(not(test), expect(dead_code, reason = "Task 7 opens the app on it"))]
    Link(Link),
    /// A draft `send` accepted that agmend or the hub then refused.
    #[cfg_attr(not(test), expect(dead_code, reason = "Task 7 opens the app on it"))]
    NotSent {
        draft: Draft,
        why: String,
    },
}

/// How far the live backend reaches (Task 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Link {
    /// agmend and its hub stream are up, and the chat is current.
    Live,
    /// agmend is up, but its hub stream is not, or the chat is still
    /// catching up. `why` is set when the hub refuses the stream for a
    /// reason a retry soon will not fix, such as being signed out.
    HubDown { why: Option<String> },
    /// agmend cannot be reached; the backend keeps retrying.
    AgmendDown { why: String },
}

/// Whether `reply_to` names an ASK or an ALERT among `messages` (Task 15):
/// a reply to one goes out as an ANSWER, anything else as an FYI.
fn answers_a_question<'a>(
    messages: impl IntoIterator<Item = &'a StampedEnvelope>,
    reply_to: Option<&MessageId>,
) -> bool {
    reply_to.is_some_and(|parent| {
        messages.into_iter().any(|m| {
            &m.client.id == parent
                && matches!(
                    m.client.payload.verb(),
                    Some(agmen_proto::Verb::Ask | agmen_proto::Verb::Alert)
                )
        })
    })
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
    /// The agent's identity, which the hub makes at kick-off; `None` while
    /// the plan is a plan.
    pub(crate) id: Option<IdentityId>,
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
    // Task 19: what the spec tab, the Review panel, the kick-off dialog and
    // the planning room show beyond the checklist's inputs.
    /// Each spec's text, approval times and comments, by path.
    pub(crate) reviews: Vec<SpecReview>,
    /// Per owner: their daily spend cap, when they set one.
    pub(crate) caps_usd: BTreeMap<String, u32>,
    /// Per owner: when they approved their own agents and spend.
    pub(crate) shares_approved_at: BTreeMap<String, Timestamp>,
    /// Per person in the planning room: whether they are at work.
    pub(crate) presence: BTreeMap<String, AgentStatus>,
}

// Task 19: a spec's review, as the spec tab and the Review panel show it.
/// One comment on a spec under review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpecComment {
    /// A person or agent handle, such as `sean/plan`.
    pub(crate) author: String,
    pub(crate) body: String,
}

/// A spec file's text at its approved commit, who approved it when, and
/// the comments on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpecReview {
    pub(crate) path: String,
    /// The commit `text` is at, which an approval from the tab names.
    pub(crate) sha: GitSha,
    pub(crate) text: SharedString,
    pub(crate) approved_at: BTreeMap<String, Timestamp>,
    pub(crate) comments: Vec<SpecComment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AgentStatus {
    Working,
    Idle,
    Reading,
}

impl AgentStatus {
    /// The word for it in notes such as "opencode · reading".
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Reading => "reading",
        }
    }
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
    /// The item it works on or reads, as the hub's roster reports it.
    pub(crate) item: Option<String>,
}

/// One person and their agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersonGroup {
    pub(crate) id: IdentityId,
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
    /// It applied a settings change, which the person can undo.
    Applied,
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
    /// Task 14: whether the harness cannot enforce the role's tool list, so
    /// Tools says what it enforces instead (spec 5.5).
    pub(crate) tools_lossy: bool,
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

/// The status bar's counts. Task 13: the needs-you and alert counts are
/// not stored; the chat derives them from its messages (mockup fix 34).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatusSummary {
    pub(crate) online: u32,
    pub(crate) worktrees: u32,
    pub(crate) spend_today_usd: f64,
}

// Task 14: take-over in the chat and the terminal agents on this machine;
// the fixture's side of it in its own module.
mod fixture_take_over;
mod take_over;
pub(crate) use take_over::{
    AgentFound, AgentSource, InstalledAgent, SessionStart, TakeOver, TermCell, TermColor,
    TerminalScreen, can_take_over,
};

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
    /// What local git knows, so the client can confirm the branches,
    /// commits and files message text names (spec 9.3).
    pub(crate) git: GitFacts,
    /// Live sessions of agents on this machine, by identity.
    pub(crate) sessions: BTreeMap<IdentityId, AgentSession>,
    /// Role files from every layer (spec 3.5).
    pub(crate) roles: Vec<RoleFile>,
    /// The worktrees on this machine, the person's own first.
    pub(crate) worktrees: Vec<WorktreeInfo>,
    /// Task 14: sessions the person took over in the chat, by identity.
    pub(crate) take_overs: BTreeMap<IdentityId, TakeOver>,
    /// Task 14: the terminal agents agmend looked for on this machine.
    pub(crate) installed_agents: Vec<InstalledAgent>,
    /// Task 14: how many agent scans agmend has finished, so Check again
    /// knows its scan's result from any other update.
    pub(crate) agent_scans: u64,
    // Task 15: the model picker's models, and message tags.
    /// The models each installed agent offers, in the picker's order.
    pub(crate) models: Vec<ModelGroup>,
    /// The model a message was sent with or answered on, by hub position.
    /// The wire carries no model (agmen-proto's FYI and ANSWER have no
    /// field), so agmend reports it beside the message.
    pub(crate) message_models: BTreeMap<(Stream, Seq), ModelChoice>,
    /// The person's "until I change it" model, which the composer's chip
    /// opens on; `None` opens it on the listener's own model.
    pub(crate) standing_model: Option<ModelChoice>,
}

/// One row of an agent's session, as the agent view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionRow {
    /// A tool call: its name and argument.
    Tool { name: String, arg: String },
    /// What a tool printed.
    Out { text: String, tone: Tone },
    /// What the agent said.
    Say(String),
    /// A turn's cost line.
    Cost(String),
}

/// How a line of tool output reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Tone {
    Plain,
    Success,
    Accent,
}

/// An agent's live session: what it is on, and what it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSession {
    pub(crate) item: String,
    pub(crate) branch: String,
    pub(crate) rows: Vec<SessionRow>,
}

/// Where a role file lives (spec 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RoleLayer {
    BuiltIn,
    /// `.agmen/roles/` in the project's repo.
    Project,
    /// `~/.config/agmen/roles/`.
    Personal,
}

/// One role file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoleFile {
    pub(crate) name: String,
    pub(crate) layer: RoleLayer,
    /// The XML; built-in roles borrow the text compiled into the binary.
    pub(crate) text: SharedString,
    /// What agmen enforces for it; `None` for base, which has no kind, and
    /// for a file that does not parse.
    pub(crate) info: Option<RoleInfo>,
    /// The built-in role whose color its dot takes (itself, or the nearest
    /// built-in it extends); `None` for base.
    pub(crate) palette_role: Option<String>,
}

/// A resolved role, as the Role panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoleInfo {
    pub(crate) kind: RoleKind,
    pub(crate) tier: Tier,
    /// Tool classes, the same for every agent (spec 3.2).
    pub(crate) tools: Vec<String>,
    /// A limit agmen states but cannot enforce, such as Bash read-only.
    pub(crate) bash_note: Option<String>,
    pub(crate) edits: Edits,
    /// The role's own verbs, then base's.
    pub(crate) verbs: Vec<String>,
    /// Verbs the role's kind could send that the role file withholds (D12).
    pub(crate) refused: Vec<String>,
    /// Who holds it, and a short note on each.
    pub(crate) used_by: Vec<(String, String)>,
    /// The roles that extend it.
    pub(crate) extended_by: Vec<String>,
    /// The role mention and what it expands to.
    pub(crate) mention: (String, String),
}

/// Branches, commits and files in the person's local worktrees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GitFacts {
    pub(crate) branches: std::collections::BTreeSet<String>,
    pub(crate) commits: std::collections::BTreeSet<String>,
    /// Paths relative to a worktree root.
    pub(crate) files: std::collections::BTreeSet<String>,
}
