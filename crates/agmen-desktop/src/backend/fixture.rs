//! The mockup's data for the chat screen, from `docs/mockups/scripts/console.py`.

use std::collections::BTreeMap;

use super::{
    AgentInfo, AgentRow, AgentStatus, ListenerInfo, PersonGroup, RecentAction, Snapshot,
    StatusSummary,
};

fn words(list: &str) -> Vec<String> {
    list.split_whitespace().map(str::to_owned).collect()
}

fn agent(
    person: &str,
    depth: u8,
    name: &str,
    role: &str,
    harness: &str,
    status: AgentStatus,
) -> AgentRow {
    AgentRow {
        handle: format!("{person}/{name}"),
        name: name.to_owned(),
        role: role.to_owned(),
        harness: harness.to_owned(),
        status,
        depth,
    }
}

fn roster() -> Vec<PersonGroup> {
    use AgentStatus::{Idle, Reading, Working};
    vec![
        PersonGroup {
            person: "andy".into(),
            you: true,
            agents: vec![
                agent("andy", 0, "listen", "listener", "codex", Idle),
                agent("andy", 1, "fx1", "engineer", "claude", Working),
                agent("andy", 1, "t7k", "tester", "opencode", Working),
                agent("andy", 2, "rv2", "reviewer", "grok", Reading),
                agent("andy", 0, "a1", "engineer", "claude", Idle),
            ],
        },
        PersonGroup {
            person: "sean".into(),
            you: false,
            agents: vec![
                agent("sean", 0, "a1", "engineer", "codex", Working),
                agent("sean", 0, "plan", "planner", "gemini", Idle),
            ],
        },
        PersonGroup {
            person: "adam".into(),
            you: false,
            agents: vec![
                agent("adam", 0, "coord", "coordinator", "claude", Working),
                agent("adam", 0, "res", "researcher", "agy", Working),
            ],
        },
    ]
}

fn listener() -> ListenerInfo {
    let recent = |time: &str, text: &str| RecentAction {
        time: time.into(),
        text: text.into(),
    };
    ListenerInfo {
        handle: "andy/listen".into(),
        agent: "codex".into(),
        state: "idle".into(),
        harness_label: "Codex 0.164.0 over ACP".into(),
        role_label: "Listener (built-in)".into(),
        can_use: words("Read Grep Glob git_log git_show git_diff git_status"),
        never: words("Edit Write Bash"),
        takes_work_from: "andy, coordinators".into(),
        per_worker_usd: 5.00,
        workers_today_usd: 11.20,
        workers_cap_usd: 20.00,
        workers_running: 3,
        workers_max: 4,
        listener_today_usd: 0.42,
        listener_cap_usd: 2.00,
        tokens_in: 96_000,
        tokens_out: 8_000,
        uptime_minutes: 400,
        recent: vec![
            recent("15:31", "delegated andy/fx1 · Engineer · claude"),
            recent("14:06", "delegated andy/t7k · Tester · opencode"),
            recent("13:52", "answered: is C012 merged? (git_log)"),
        ],
    }
}

fn fx1() -> AgentInfo {
    AgentInfo {
        handle: "andy/fx1".into(),
        role_label: "Engineer (built-in)".into(),
        harness_label: "Claude Code 2.1.310 · CLI adapter".into(),
        model: "sonnet (standard tier)".into(),
        delegated_by: Some("andy/listen at 15:31".into()),
        state: "working".into(),
        tools: words("Read Edit Write Bash Grep Glob"),
        edits: "lane L-CORE".into(),
        verbs: words("CLAIM DONE PUSH_REQUEST FOUND chat"),
        refused: words("ASSIGN RULING LEND REVIEW"),
        branch: "fix/c015-main-conflict".into(),
        base: "origin/main @4d9fdc8".into(),
        worktree_state: "1 commit, pushed, clean".into(),
        cleanup: "removed with its branch after merge".into(),
        spent_usd: 1.20,
        budget_usd: 5.00,
        since_launch_minutes: 72,
        working_minutes: 41,
        tokens_in: 418_000,
        tokens_out: 36_000,
        cache_read: 1_900_000,
    }
}

/// The state the `desktop-chat` render shows.
#[must_use]
pub(crate) fn chat_snapshot() -> Snapshot {
    let fx1 = fx1();
    Snapshot {
        project: "pr-3331".into(),
        me: "andy".into(),
        roster: roster(),
        listener: listener(),
        agents: BTreeMap::from([(fx1.handle.clone(), fx1)]),
        status: StatusSummary {
            needs_you: 0,
            online: 9,
            alerts: 2,
            worktrees: 4,
            spend_today_usd: 14.20,
        },
    }
}
