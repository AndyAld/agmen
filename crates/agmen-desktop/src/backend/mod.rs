//! What the views show, as plain data. Task 6 of the desktop plan replaces
//! this file with the `Backend` trait and its fixture; the names and fields
//! here stay the same.

mod fixture;

pub(crate) use fixture::chat_snapshot;

/// What an agent is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AgentStatus {
    Working,
    Idle,
    Reading,
}

/// One agent in the Agents dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentRow {
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
    pub(crate) me: String,
    pub(crate) roster: Vec<PersonGroup>,
    pub(crate) listener: ListenerInfo,
    /// Keyed by handle.
    pub(crate) agents: std::collections::BTreeMap<String, AgentInfo>,
    pub(crate) status: StatusSummary,
}
