//! The mockup's data (`docs/mockups/scripts/console.py`), with the mockup
//! fixes Andy chose (plan, "Mockup fixes to build"). Messages are built as
//! wire JSON, so the fixture uses only the protocol's public format.
//!
//! The mockup names items `C015`; protocol item keys need a dash
//! (`^[A-Z]{1,4}-[0-9]{1,6}$`), so bodies keep the mockup's text and the
//! structured `item` fields say `C-015`.

use std::borrow::Cow;
use std::collections::BTreeMap;

use agmen_proto::{GitSha, IdentityId, StampedEnvelope};
use serde_json::{Value, json};

use gpui::SharedString;

use crate::roles;

use super::{
    AgentInfo, AgentRow, AgentSession, AgentStatus, Approval, BackendError, ChangeCard, ChangeId,
    ChangeKind, ChangeRow, ChangeState, CommitInfo, DiffLine, DiffLineKind, FileChange, FileText,
    GitFacts, GutterMark, Hunk, LaneInfo, ListenerInfo, PersonGroup, PlanInfo, RecentAction,
    RecentKind, RoleFile, RoleInfo, RoleLayer, SessionRow, SpecState, TeamRow, Tone, WorktreeHead,
    WorktreeInfo, WorktreeState,
};

use crate::roles::words;

fn agent(
    person: &str,
    depth: u8,
    name: &str,
    role: &str,
    harness: &str,
    status: AgentStatus,
) -> Result<AgentRow, BackendError> {
    let handle = format!("{person}/{name}");
    Ok(AgentRow {
        id: id(&handle)?,
        handle,
        name: name.to_owned(),
        role: role.to_owned(),
        harness: harness.to_owned(),
        status,
        depth,
        item: None,
    })
}

pub(super) fn roster() -> Result<Vec<PersonGroup>, BackendError> {
    use AgentStatus::{Idle, Reading, Working};
    Ok(vec![
        PersonGroup {
            id: id("andy")?,
            person: "andy".into(),
            you: true,
            agents: vec![
                agent("andy", 0, "listen", "listener", "codex", Idle)?,
                agent("andy", 1, "fx1", "engineer", "claude", Working)?,
                agent("andy", 1, "t7k", "tester", "opencode", Working)?,
                // Mockup fix 28: rv2 runs on a harness installed on this machine.
                AgentRow {
                    // Mockup fix 51: rv2 is reading C015's DONE.
                    item: Some("C015".into()),
                    ..agent("andy", 2, "rv2", "reviewer", "opencode", Reading)?
                },
                agent("andy", 0, "a1", "engineer", "claude", Idle)?,
            ],
        },
        PersonGroup {
            id: id("sean")?,
            person: "sean".into(),
            you: false,
            agents: vec![
                agent("sean", 0, "a1", "engineer", "codex", Working)?,
                agent("sean", 0, "plan", "planner", "gemini", Idle)?,
            ],
        },
        PersonGroup {
            id: id("adam")?,
            person: "adam".into(),
            you: false,
            agents: vec![
                agent("adam", 0, "coord", "coordinator", "claude", Working)?,
                agent("adam", 0, "res", "researcher", "agy", Working)?,
            ],
        },
    ])
}

pub(super) fn listener() -> ListenerInfo {
    let recent = |time: &str, text: &str, kind| RecentAction {
        time: time.into(),
        text: text.into(),
        kind,
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
        // Mockup fix 36: workers plus listener is the status bar's $14.20.
        workers_today_usd: 13.78,
        workers_cap_usd: 20.00,
        workers_running: 3,
        workers_max: 4,
        listener_today_usd: 0.42,
        listener_cap_usd: 2.00,
        tokens_in: 96_000,
        tokens_out: 8_000,
        uptime_minutes: 400,
        recent: vec![
            recent(
                "15:31",
                "delegated andy/fx1 · Engineer · claude",
                RecentKind::Delegated,
            ),
            recent(
                "14:06",
                "delegated andy/t7k · Tester · opencode",
                RecentKind::Delegated,
            ),
            recent(
                "13:52",
                "answered: is C012 merged? (git_log)",
                RecentKind::Answered,
            ),
        ],
    }
}

pub(super) fn fx1() -> Result<AgentInfo, BackendError> {
    Ok(AgentInfo {
        id: id("andy/fx1")?,
        role: "engineer".into(),
        status: AgentStatus::Working,
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
        // Mockup fix 39: delegated at 15:31, about 14 minutes before 15:45.
        since_launch_minutes: 14,
        working_minutes: 10,
        tokens_in: 418_000,
        tokens_out: 36_000,
        cache_read: 1_900_000,
    })
}

/// A stable identity id for a mockup handle: the handle without `/`,
/// padded to the protocol's 8 characters.
///
/// # Errors
///
/// When the handle does not make a valid id (it must be lowercase ASCII).
pub(super) fn id(handle: &str) -> Result<IdentityId, BackendError> {
    let flat: String = handle.chars().filter(|c| *c != '/').collect();
    format!("{flat:0<8}")
        .parse()
        .map_err(|_| BackendError::BadFixture(handle.to_owned()))
}

/// The id as wire text, for JSON; an invalid handle makes JSON the protocol
/// refuses, which `stamp` reports.
fn id_text(handle: &str) -> String {
    let flat: String = handle.chars().filter(|c| *c != '/').collect();
    format!("{flat:0<8}")
}

fn kind(handle: &str) -> &'static str {
    match handle {
        "hub" => "hub",
        "andy/listen" => "listener",
        // Task 13: adam is a coordinator as a person too ("human ·
        // coordinator" in frame 6122, "adam (coordinator)" in its banner).
        "adam/coord" | "andy/coord" | "adam" => "coordinator",
        h if h.contains('/') => "agent",
        _ => "human",
    }
}

/// The mockup's worktree history: what `git` on andy's machine knows.
pub(super) fn git_facts() -> GitFacts {
    let set = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect();
    GitFacts {
        branches: set(&BRANCHES),
        // Every commit worktrees() lists (C191).
        commits: set(&[
            "7c8835e", "4d9fdc8", "016f235", "9e2a71c", "a41c0d2", "5be3f19",
        ]),
        files: set(&[
            ".github/workflows/release.yml",
            "Cargo.lock",
            "Cargo.toml",
            "README.md",
            "src/app.rs",
            "src/cylinder.rs",
            "src/document.rs",
            "src/main.rs",
            "src/minimap.rs",
            "src/ui/mod.rs",
            "src/ui/tabs.rs",
            "tests/it_main.rs",
            "tests/it_tabs.rs",
        ]),
    }
}

/// Every handle in the mockup's chats, which the hub knows from its roster.
const ROSTER: &[&str] = &[
    "andy",
    "sean",
    "adam",
    "andy/listen",
    "andy/fx1",
    "andy/t7k",
    "andy/rv2",
    "andy/a1",
    "sean/a1",
    "sean/plan",
    "adam/coord",
    "adam/res",
    "andy/coord",
    "andy/core",
    "andy/app",
    "andy/rev",
    "sean/sync",
    "sean/qa",
    "sean/tiles",
];

/// What the hub stamps as `refs` for a body: identities it has in its
/// roster, items, lanes and PRs (protocol section 3). Git names are left to
/// the client.
// shortcut: a stand-in for the hub's own stamp_refs; once the fixture is replaced by agmend, the hub's refs arrive on the wire and this goes.
fn hub_refs(body: &str) -> Vec<Value> {
    static WORDS: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?P<who>@?\b[a-z]+(?:/[a-z0-9]+)?\b)|(?P<item>\b[A-Z](?:-\d{1,6}|\d{3})\b)|(?P<lane>\bL-[A-Z]+\b)|(?P<pr>#\d+)",
        )
        // A pattern that failed to compile would stamp no refs rather than
        // panic; refs_find_every_kind_in_chat_fixture proves it compiles.
        .ok()
    });
    let Some(re) = WORDS.as_ref() else {
        return Vec::new();
    };
    let mut refs = Vec::new();
    for caps in re.captures_iter(body) {
        let Some(m) = caps.get(0) else { continue };
        // A word that is part of a path or a longer name is not a ref.
        let after = body[m.end()..].chars().next();
        if after.is_some_and(|c| {
            c == '/' || c == '.' && body[m.end() + 1..].starts_with(|n: char| n.is_alphanumeric())
        }) {
            continue;
        }
        let text = m.as_str();
        let (kind, target) = if caps.name("who").is_some() {
            let handle = text.trim_start_matches('@');
            if !ROSTER.contains(&handle) {
                continue;
            }
            ("identity", id_text(handle))
        } else if caps.name("item").is_some() {
            // The mockup writes C015; the protocol's key is C-015.
            let key = if text.contains('-') {
                text.to_owned()
            } else {
                format!("{}-{}", &text[..1], &text[1..])
            };
            ("item", key)
        } else if caps.name("lane").is_some() {
            ("lane", text.to_owned())
        } else {
            ("pr", text.trim_start_matches('#').to_owned())
        };
        refs.push(json!({"kind": kind, "start": m.start(), "end": m.end(), "target": target}));
    }
    refs
}

/// One chat message, before it is stamped.
pub(super) struct Msg {
    pub(super) time: &'static str,
    pub(super) handle: &'static str,
    pub(super) verb: &'static str,
    pub(super) data: Value,
    pub(super) body: Cow<'static, str>,
    /// The index of the earlier message this one answers.
    pub(super) reply_to: Option<usize>,
    pub(super) to: Vec<&'static str>,
    pub(super) mentioned: Vec<&'static str>,
    pub(super) items: Vec<&'static str>,
}

fn msg(
    time: &'static str,
    handle: &'static str,
    verb: &'static str,
    body: impl Into<Cow<'static, str>>,
) -> Msg {
    Msg {
        time,
        handle,
        verb,
        data: json!({}),
        body: body.into(),
        reply_to: None,
        to: Vec::new(),
        mentioned: Vec::new(),
        items: Vec::new(),
    }
}

impl Msg {
    fn data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }
    fn reply_to(mut self, ix: usize) -> Self {
        self.reply_to = Some(ix);
        self
    }
    fn to(mut self, handle: &'static str) -> Self {
        self.to.push(handle);
        self
    }
    fn mentions(mut self, handles: &[&'static str]) -> Self {
        self.mentioned.extend_from_slice(handles);
        self
    }
    fn items(mut self, items: &[&'static str]) -> Self {
        self.items.extend_from_slice(items);
        self
    }
}

/// The ULID-shaped id of the message at `seq`.
pub(super) fn message_id(seq: usize) -> String {
    format!("01JC{seq:022}")
}

/// Mountain time 15:27 on 2026-10-25 is 21:27 UTC.
fn utc(time: &str) -> String {
    let (h, m) = time.split_once(':').unwrap_or(("0", "0"));
    let h: u32 = h.parse().unwrap_or(0);
    format!("2026-10-25T{:02}:{m}:00Z", (h + 6) % 24)
}

/// Stamps `msgs` as the hub would, numbering from `first_seq`.
///
/// # Errors
///
/// When a message is not one the protocol accepts.
pub(super) fn stamp(
    msgs: Vec<Msg>,
    first_seq: usize,
) -> Result<Vec<StampedEnvelope>, BackendError> {
    msgs.into_iter()
        .enumerate()
        .map(|(i, m)| {
            let seq = first_seq + i;
            let ids =
                |handles: &[&str]| -> Vec<String> { handles.iter().map(|h| id_text(h)).collect() };
            let value = json!({
                "v": 1,
                "id": message_id(seq),
                "reply_to": m.reply_to.map(|r| message_id(first_seq + r)),
                "to": ids(&m.to),
                "items": m.items,
                "body": &*m.body,
                "sent_at": utc(m.time),
                "verb": m.verb,
                "data": m.data,
                "stream": "feed",
                "seq": seq,
                "from": id_text(m.handle),
                "from_handle": m.handle,
                "from_kind": kind(m.handle),
                "mentioned": ids(&m.mentioned),
                "refs": hub_refs(&m.body),
                "hub_at": utc(m.time),
            });
            serde_json::from_value(value)
                .map_err(|err| BackendError::BadFixture(format!("message {seq}: {err}")))
        })
        .collect()
}

fn done(item: &str, branch: &str, commit: &str, passed: u32) -> Value {
    json!({
        "item": item, "branch": branch, "commit": commit, "base_sha": "4d9fdc8",
        "checks": [{"name": "cargo test", "passed": passed, "failed": 0, "baseline": null}],
        "changes": [{"status": "M", "path": "src/main.rs"}],
    })
}

/// `CHAT` in console.py: the desktop-chat story.
pub(super) fn chat_messages() -> Vec<Msg> {
    vec![
        msg("15:27", "andy/a1", "DONE",
            "C014 feat/c014-release-matrix@7c8835e · PR #9 · release.yml matrix: windows, macos-26, linux")
            .data(done("C-014", "feat/c014-release-matrix", "7c8835e", 42)).items(&["C-014"]),
        msg("15:28", "andy/a1", "ALERT",
            "main does not compile: 4d9fdc8 committed conflict markers at src/main.rs:2-8. \
             @sean/a1 please hold pushes to main until the fix lands.")
            .data(json!({"need": "hold pushes to main", "urgency": "normal", "needs": "anyone", "actions": []}))
            .mentions(&["sean/a1"]),
        msg("15:31", "andy", "FYI", "Spawn another worker to fix main and keep Sean's changes")
            .to("andy/listen").mentions(&["andy/listen"]),
        msg("15:31", "andy/listen", "ANSWER",
            "git_show: Sean's 016f235 and 4d9fdc8 are on main; 4d9fdc8 left markers at src/main.rs:2-8. Delegating.")
            .reply_to(2).mentions(&["andy"]),
        msg("15:31", "andy/listen", "DELEGATED",
            "andy/fx1 · Engineer · claude · sonnet · $5.00 · fix/c015-main-conflict")
            .data(json!({"worker": id_text("andy/fx1"), "agent": "claude", "role": "engineer",
                         "model": "sonnet", "tier": "standard", "budget_usd": 5.0, "item": "C-015"}))
            .reply_to(2).items(&["C-015"]),
        msg("15:33", "hub", "GRANT", "C015 to andy/fx1, lease 30 min")
            .data(json!({"item": "C-015", "lease_until": "2026-10-25T22:03:00Z"}))
            .items(&["C-015"]),
        msg("15:41", "andy/fx1", "DONE",
            "C015 fix/c015-main-conflict@9e2a71c · cargo test 71 passed, 0 failed · keeps cylinder, document, minimap and tabs from both sides")
            .data(done("C-015", "fix/c015-main-conflict", "9e2a71c", 71)).items(&["C-015"]),
        msg("15:44", "andy/rv2", "REVIEW", "pass · 0 findings · traced the mod list and both callers")
            .data(json!({"verdict": "pass", "findings": [], "traced": "the mod list and both callers"}))
            .reply_to(6).mentions(&["andy/fx1"]),
        msg("15:45", "sean", "FYI", "@andy merging #10 after CI. Sorry about the markers.")
            .mentions(&["andy"]),
    ]
}

// Task 18: the palette's spawn rows play the 15:31 DELEGATED exchange.
/// andy asks the listener for a worker on `agent`, and the listener answers
/// and delegates it to `worker` (such as `andy/fx2`), as at 15:31. The model
/// is the agent's own default: the mockup draws only claude's.
pub(super) fn spawn_exchange(agent: &str, worker: &str) -> Vec<Msg> {
    vec![
        msg(
            "16:00",
            "andy",
            "FYI",
            format!("Spawn a worker with {agent}"),
        )
        .to("andy/listen")
        .mentions(&["andy/listen"]),
        msg(
            "16:00",
            "andy/listen",
            "ANSWER",
            format!("Delegating to a {agent} worker."),
        )
        .reply_to(0)
        .mentions(&["andy"]),
        msg(
            "16:00",
            "andy/listen",
            "DELEGATED",
            format!("{worker} · Engineer · {agent} · default · $5.00"),
        )
        .data(
            json!({"worker": id_text(worker), "agent": agent, "role": "engineer",
                     "model": "default", "tier": "standard", "budget_usd": 5.0, "item": null}),
        )
        .reply_to(0),
    ]
}

/// `ALERT_MSG`: adam/coord's urgent alert (desktop-needs-you).
pub(super) fn urgent_alert() -> Msg {
    msg("15:58", "adam/coord", "ALERT",
        "@andy CI on main is red after #10 merged: it_tabs, 3 failed. The release tag is blocked. Revert #10, or hotfix on a branch?")
        .data(json!({
            "need": "decide: revert #10 or hotfix on a branch",
            "urgency": "urgent",
            "needs": "anyone",
            "actions": ["Hotfix: spawn an engineer", "Revert #10", "Snooze 15 min"],
        }))
        .mentions(&["andy"])
}

/// `ANSWER_MSG`: adam answers his own alert; `alert` is its index.
pub(super) fn alert_answer(alert: usize) -> Msg {
    msg(
        "15:59",
        "adam",
        "ANSWER",
        "Revert #10 now. @sean/a1 hotfixes it_tabs on fix/c016-it-tabs. Clearing this.",
    )
    .reply_to(alert)
    .mentions(&["sean/a1"])
}

/// `SETTINGS_CHAT`: the listener changes settings from chat. Returns the
/// messages and the index of the listener's settings message, which carries
/// the change card.
pub(super) fn settings_messages(first: usize, applied: bool) -> (Vec<Msg>, usize) {
    let listener_says = if applied {
        "Done. andy/fx1 can edit L-INFRA deploy/** until 18:00, and its budget is $8.00."
    } else {
        "Both changes widen what andy/fx1 may do, so they wait for your Apply."
    };
    let msgs = vec![
        msg("15:50", "andy", "FYI",
            "Give andy/fx1 edit access to L-INFRA until 18:00 and raise its budget to $8.")
            .to("andy/listen").mentions(&["andy/listen"]),
        msg("15:50", "andy/listen", "ANSWER", listener_says)
            .reply_to(first).mentions(&["andy"]),
        msg("15:51", "sean", "FYI", "@andy/listen turn off push approval for andy's agents")
            .mentions(&["andy/listen"]),
        msg("15:51", "andy/listen", "ANSWER",
            "Only andy can change andy's settings, so I changed nothing. @andy, sean asked for this.")
            .reply_to(first + 2).mentions(&["andy", "sean"]),
    ];
    (msgs, first + 1)
}

pub(super) fn settings_card(state: ChangeState) -> ChangeCard {
    let row = |setting: &str, before: &str, after: &str| ChangeRow {
        target: "andy/fx1".into(),
        setting: setting.into(),
        before: before.into(),
        after: after.into(),
    };
    ChangeCard {
        id: ChangeId("chg-fx1-lanes-budget".into()),
        requested_by: "andy".into(),
        widens: true,
        rows: vec![
            row("lanes", "L-CORE", "L-CORE, L-INFRA deploy/** until 18:00"),
            row("budget", "$5.00", "$8.00"),
        ],
        state,
    }
}

/// `ASK_CHAT`: a question to the listener on another model.
pub(super) fn ask_messages(first: usize) -> Vec<Msg> {
    vec![
        msg("15:52", "andy", "FYI",
            "Why does C014's release matrix skip linux-musl, and should C016 add it?")
            .to("andy/listen").mentions(&["andy/listen"]),
        msg("15:52", "andy/listen", "ANSWER",
            "release.yml builds x86_64-unknown-linux-gnu only; 7c8835e dropped musl and its message gives no reason. Adding it is a CI change in lane L-INFRA. Want me to delegate C016 to a Tester? (git_show 7c8835e · 41k tokens · $0.31 · 18 s)")
            .reply_to(first).mentions(&["andy"]),
    ]
}

/// `PLAN_CHAT`, `PLAN_READY` and `PLAN_RUN`: planning Ridgeline.
pub(super) fn plan_messages(ready: bool, running: bool) -> Vec<Msg> {
    let mut msgs = vec![
        msg("15:40", "andy", "FYI",
            "Greenfield: Ridgeline, offline trail maps for iOS and Android on a Rust core. Can you draft the team and the first specs?")
            .to("sean/plan").mentions(&["sean/plan"]),
        // Mockup fix 53: two specs, not three.
        msg("15:41", "sean/plan", "PROPOSAL",
            "Plan v1 is on the right: 7 agents, 2 specs, 4 lanes, about $105/day. Open question: who owns map tiles? The license decides whether we self-host them.")
            .data(json!({"options": [], "deadline": null})),
        msg("15:43", "sean", "FYI",
            "Run tile research on my machine with agy. I'll take sync too, on opencode."),
        msg("15:44", "sean/plan", "PROPOSAL",
            "Plan v2: tiles (new tile-researcher role) and sync run on sean's machine; tile-sources.md added for the researcher to fill in.")
            .data(json!({"options": [], "deadline": null})).reply_to(2),
        msg("15:47", "andy", "FYI",
            "Approved ridgeline-design.md. Make the reviewer deep tier and give core to codex.")
            .to("sean/plan").mentions(&["sean/plan"]),
        msg("15:48", "sean/plan", "PROPOSAL",
            "Plan v3: reviewer on opus, core on codex. Waiting on sean's approval of the design spec and his 3 agents.")
            .data(json!({"options": [], "deadline": null})).reply_to(4),
    ];
    if ready || running {
        msgs.push(
            msg("15:52", "sean", "DECISION", "Approved ridgeline-design.md and my 3 agents ($40/day).")
                .data(json!({"outcome": "GO", "option": null, "subject": "docs/specs/ridgeline-design.md"}))
                .reply_to(5),
        );
    }
    if running {
        let first = msgs.len();
        msgs.extend([
            msg("15:55", "andy", "DECISION", "Kick off plan v3.")
                .data(json!({"outcome": "GO", "option": null, "subject": null})),
            msg("15:55", "hub", "STATE",
                "lanes L-CORE L-APP L-SYNC L-TESTS and items R-1 to R-12 created from docs/plans/m0.md")
                .data(json!({"key": "plan.ridgeline", "v": 3, "as_of_seq": first})),
            msg("15:55", "andy/coord", "ASSIGN",
                "R-1 to @andy/core, R-2 to @andy/app, R-3 to @sean/sync, R-4 to @sean/qa")
                .data(json!({"item": "R-1", "to": id_text("andy/core")}))
                .items(&["R-1", "R-2", "R-3", "R-4"])
                .mentions(&["andy/core", "andy/app", "sean/sync", "sean/qa"]),
            msg("15:56", "andy/core", "DM",
                "@sean/sync the tile cache API lands in core first. I'll post its signature before you build on it, so we don't both write one.")
                .to("sean/sync").mentions(&["sean/sync"]),
            msg("15:56", "sean/sync", "ANSWER",
                "Agreed. I'll stub sync against your trait and swap in the real one when R-1 is DONE.")
                .reply_to(first + 3).mentions(&["andy/core"]),
            msg("15:57", "sean/tiles", "FYI", "Starting the tile licensing survey for tile-sources.md."),
        ]);
    }
    msgs
}

/// `TEAM` and the plan panel (desktop-plan, -plan-ready, -running).
pub(super) fn plan(ready: bool, running: bool) -> Result<PlanInfo, BackendError> {
    let sha = |s: &str| -> Result<GitSha, BackendError> {
        s.parse()
            .map_err(|_| BackendError::BadFixture(format!("sha {s}")))
    };
    let team =
        |role: &str, label: &str, handle: &str, harness: &str, lane: &str, owner: &str, usd| {
            TeamRow {
                role: role.into(),
                role_label: label.into(),
                handle: handle.into(),
                harness: harness.into(),
                lane: lane.into(),
                owner: owner.into(),
                usd_per_day: usd,
            }
        };
    let both = |sean: Approval| {
        BTreeMap::from([("andy".into(), Approval::Approved), ("sean".into(), sean)])
    };
    let sean = if ready || running {
        Approval::Approved
    } else {
        Approval::Waiting
    };
    let lane = |name: &str, path: &str| LaneInfo {
        name: name.into(),
        paths: vec![path.into()],
    };
    Ok(PlanInfo {
        name: "ridgeline".into(),
        version: 3,
        goal: "Offline trail maps for iOS and Android on a shared Rust core. First milestone: a downloaded region renders with no network.".into(),
        team: vec![
            team("coordinator", "Coordinator", "coord", "claude · sonnet", "board", "andy", 10),
            team("engineer", "Engineer", "core", "codex", "L-CORE", "andy", 20),
            team("engineer", "Engineer", "app", "claude · sonnet", "L-APP", "andy", 20),
            team("reviewer", "Reviewer", "rev", "claude · opus", "all", "andy", 15),
            team("engineer", "Engineer", "sync", "opencode", "L-SYNC", "sean", 20),
            team("tester", "Tester", "qa", "gemini", "L-TESTS", "sean", 10),
            team("researcher", "Tile Researcher", "tiles", "agy", "none", "sean", 10),
        ],
        specs: vec![
            SpecState { path: "docs/specs/ridgeline-design.md".into(), sha: sha("3f1c0d2")?, draft_note: None, approvals: both(sean) },
            SpecState {
                path: "docs/specs/tile-sources.md".into(),
                sha: sha("3f1c0d2")?,
                draft_note: Some("draft, researcher fills in".into()),
                approvals: BTreeMap::new(),
            },
            SpecState { path: "docs/plans/m0.md".into(), sha: sha("3f1c0d2")?, draft_note: None, approvals: both(Approval::Approved) },
            SpecState { path: ".agmen/roles/tile-researcher.xml".into(), sha: sha("3f1c0d2")?, draft_note: None, approvals: both(Approval::Approved) },
        ],
        lanes: vec![
            lane("L-CORE", "core/**"),
            lane("L-APP", "app/**"),
            lane("L-SYNC", "sync/**"),
            lane("L-TESTS", "tests/**"),
        ],
        repo_exists: true,
        branch_merged: ready || running,
        shares_approved: BTreeMap::from([("andy".into(), true), ("sean".into(), ready || running)]),
        running,
    })
}

/// The planning room roster (desktop-plan's left dock).
pub(super) fn plan_roster() -> Result<Vec<PersonGroup>, BackendError> {
    use AgentStatus::{Idle, Working};
    let row = |handle: &str, role: &str, harness: &str, status| -> Result<AgentRow, BackendError> {
        Ok(AgentRow {
            id: id(handle)?,
            handle: handle.into(),
            name: handle.rsplit('/').next().unwrap_or(handle).into(),
            role: role.into(),
            harness: harness.into(),
            status,
            depth: 0,
            item: None,
        })
    };
    Ok(vec![
        // Mockup fix 52: sean/plan keeps its pr-3331 harness, gemini.
        PersonGroup {
            id: id("andy")?,
            person: "andy".into(),
            you: true,
            agents: vec![row("andy/listen", "listener", "codex", Idle)?],
        },
        PersonGroup {
            id: id("sean")?,
            person: "sean".into(),
            you: false,
            agents: vec![row("sean/plan", "planner", "gemini", Working)?],
        },
    ])
}

/// `SESSION` in console.py: andy/fx1's turn, with mockup fix 41 (+2 -5).
pub(super) fn fx1_session() -> AgentSession {
    let tool = |name: &str, arg: &str| SessionRow::Tool {
        name: name.into(),
        arg: arg.into(),
    };
    let out = |text: &str, tone| SessionRow::Out {
        text: text.into(),
        tone,
    };
    AgentSession {
        item: "C015".into(),
        branch: "fix/c015-main-conflict".into(),
        rows: vec![
            tool("Read", "src/main.rs"),
            tool("Bash", "git show 4d9fdc8 --stat"),
            out("2 files changed, 14 insertions(+), 3 deletions(-)", Tone::Plain),
            SessionRow::Say(
                "Both sides add modules. Keeping cylinder, document, minimap and tabs; dropping the conflict markers.".into(),
            ),
            tool("Edit", "src/main.rs  +2 -5"),
            tool("Bash", "cargo test"),
            out("test result: ok. 71 passed; 0 failed", Tone::Success),
            tool("Bash", "git push -u origin fix/c015-main-conflict"),
            out("ok", Tone::Plain),
            SessionRow::Cost("turn 4 · $0.31 · 44 s".into()),
            tool("agmen", "done C015"),
            out("recorded #1866 (feed)", Tone::Accent),
        ],
    }
}

/// The built-in role whose color a role's dot takes: itself, or the
/// nearest built-in it extends. Base has no role color.
fn palette_role(
    name: &str,
    files: &[(&str, RoleLayer, &'static str)],
    specs: &[roles::RoleSpec],
) -> Option<String> {
    let mut current = name;
    // Bounded like resolve: a chain longer than the files is a loop.
    for _ in 0..=files.len() {
        if current == "base" {
            return None;
        }
        if files
            .iter()
            .any(|(n, layer, _)| *n == current && *layer == RoleLayer::BuiltIn)
        {
            return Some(current.to_owned());
        }
        current = specs
            .iter()
            .find(|s| s.name == current)?
            .extends
            .as_deref()?;
    }
    None
}

/// The built-in reviewer role, which the code view's dev window also shows.
pub(crate) const REVIEWER_XML: &str = include_str!("../../../../roles/reviewer.xml");

/// The built-in roles ship in the binary from the repo's `roles/` folder
/// (spec 3.2); the project and personal ones are the mockup's examples.
/// Each role's panel is resolved from the files, the way agmend will.
pub(super) fn role_files(roster: &[PersonGroup]) -> Vec<RoleFile> {
    let files: [(&str, RoleLayer, &'static str); 10] = [
        (
            "base",
            RoleLayer::BuiltIn,
            include_str!("../../../../roles/base.xml"),
        ),
        (
            "listener",
            RoleLayer::BuiltIn,
            include_str!("../../../../roles/listener.xml"),
        ),
        (
            "coordinator",
            RoleLayer::BuiltIn,
            include_str!("../../../../roles/coordinator.xml"),
        ),
        (
            "planner",
            RoleLayer::BuiltIn,
            include_str!("../../../../roles/planner.xml"),
        ),
        (
            "researcher",
            RoleLayer::BuiltIn,
            include_str!("../../../../roles/researcher.xml"),
        ),
        (
            "engineer",
            RoleLayer::BuiltIn,
            include_str!("../../../../roles/engineer.xml"),
        ),
        (
            "tester",
            RoleLayer::BuiltIn,
            include_str!("../../../../roles/tester.xml"),
        ),
        ("reviewer", RoleLayer::BuiltIn, REVIEWER_XML),
        (
            "security-reviewer",
            RoleLayer::Project,
            "<role name=\"security-reviewer\" extends=\"reviewer\" kind=\"agent\" tier=\"deep\" version=\"1\">\n  <summary>Reviews a DONE for security only.</summary>\n</role>\n",
        ),
        (
            "docs-writer",
            RoleLayer::Personal,
            "<role name=\"docs-writer\" extends=\"engineer\" kind=\"agent\" tier=\"standard\" version=\"1\">\n  <summary>Writes docs in docs/**.</summary>\n  <edits scope=\"paths\" globs=\"docs/**\" />\n</role>\n",
        ),
    ];
    let specs: Vec<roles::RoleSpec> = files
        .iter()
        .filter_map(|(_, _, text)| roles::parse(text).ok())
        .collect();
    let info = |name: &str| -> Option<RoleInfo> {
        // A role that does not resolve shows its file with no panel.
        let resolved = roles::resolve(name, &specs).ok()?;
        // Ruling D12: what the kind could send but this role cannot.
        let refused = agmen_proto::Verb::ALL
            .iter()
            .filter(|v| roles::kind_may_send(resolved.kind, **v))
            .map(|v| v.as_str())
            .filter(|v| !resolved.verbs.iter().any(|own| own == v))
            .map(str::to_owned)
            .collect();
        let used_by: Vec<(String, String)> = roster
            .iter()
            .flat_map(|group| &group.agents)
            .filter(|agent| agent.role == name)
            .map(|agent| {
                let mut note = format!("{} · {}", agent.harness, agent.status.label());
                if let Some(item) = &agent.item {
                    note = format!("{note} {item}");
                }
                (agent.handle.clone(), note)
            })
            .collect();
        let extended_by = specs
            .iter()
            .filter(|s| s.extends.as_deref() == Some(name))
            .map(|s| s.name.clone())
            .collect();
        // Spec 3.4: no agent can limit which commands a shell runs.
        let read_only_shell =
            resolved.tools.iter().any(|t| t == "shell") && resolved.edits == roles::Edits::None;
        Some(RoleInfo {
            kind: resolved.kind,
            tier: resolved.tier,
            tools: resolved.tools,
            bash_note: read_only_shell
                .then(|| "read-only by rule only; not enforceable".to_owned()),
            edits: resolved.edits,
            verbs: resolved.verbs,
            refused,
            used_by,
            extended_by,
            mention: (
                format!("@{name}s"),
                format!("role:{name} on the wire · humans and coordinators"),
            ),
        })
    };
    files
        .iter()
        .map(|(name, layer, text)| RoleFile {
            name: (*name).to_owned(),
            layer: *layer,
            text: SharedString::new_static(text),
            info: info(name),
            palette_role: palette_role(name, &files, &specs),
        })
        .collect()
}

/// The worktrees on Andy's machine, by person, Andy's first: fx1's files
/// are `PROJECT` and `TESTS` in console.py, and its commits and the other
/// worktrees are the Git panel's (mockup fix 51: rv2 reads C015 detached at
/// fx1's commit). Every worktree is of the same repo, so they share files.
pub(super) fn worktrees() -> Result<Vec<WorktreeInfo>, BackendError> {
    let files: Vec<String> = [
        ".github/workflows/release.yml",
        "Cargo.lock",
        "Cargo.toml",
        "README.md",
        // Mockup fix 50: main.rs declares `mod app;`.
        "src/app.rs",
        "src/cylinder.rs",
        "src/document.rs",
        "src/main.rs",
        "src/minimap.rs",
        "src/ui/mod.rs",
        "src/ui/tabs.rs",
        "tests/it_main.rs",
        "tests/it_tabs.rs",
    ]
    .iter()
    .map(|f| (*f).to_owned())
    .collect();
    let worktree = |handle: &str, head, commits, state| -> Result<_, BackendError> {
        Ok(WorktreeInfo {
            owner: id(handle)?,
            handle: handle.into(),
            head,
            files: files.clone(),
            changes: Vec::new(),
            commits,
            state,
            texts: BTreeMap::new(),
        })
    };
    let branch = |name: &str| WorktreeHead::Branch(name.into());
    let resolve = commit("9e2a71c", "Resolve mod list", "andy/fx1", "21:31")?;
    let fx1 = WorktreeInfo {
        changes: fx1_changes(),
        texts: BTreeMap::from([("src/main.rs".to_owned(), fx1_main_rs())]),
        ..worktree(
            "andy/fx1",
            branch("fix/c015-main-conflict"),
            vec![
                resolve.clone(),
                commit("4d9fdc8", "Merge sean/tabs", "sean/a1", "20:58")?,
                commit("016f235", "Move tabs to src/ui", "sean/a1", "20:40")?,
            ],
            WorktreeState::Pushed,
        )?
    };
    let all = vec![
        fx1,
        worktree(
            "andy/t7k",
            branch("test/b-415-replay-paging"),
            vec![commit(
                "7c8835e",
                "Page replay by seq",
                "andy/t7k",
                "21:12",
            )?],
            WorktreeState::LocalOnly,
        )?,
        worktree(
            "andy/rv2",
            WorktreeHead::Detached {
                at: resolve.sha.clone(),
                item: "C015".into(),
            },
            vec![resolve],
            WorktreeState::Reading,
        )?,
        worktree(
            "sean/a1",
            branch("fix/c016-it-tabs"),
            vec![commit(
                "a41c0d2",
                "Keep it_tabs in tab order",
                "sean/a1",
                "21:48",
            )?],
            WorktreeState::LocalOnly,
        )?,
        worktree(
            "sean/plan",
            branch("plan/ridgeline"),
            vec![commit(
                "5be3f19",
                "Draft the ridgeline plan",
                "sean/plan",
                "21:44",
            )?],
            WorktreeState::Pushed,
        )?,
    ];
    Ok(all.into_iter().map(WorktreeInfo::normalized).collect())
}

/// A commit at `utc`, a time on the mockup's day (2026-10-25) in UTC.
fn commit(sha: &str, subject: &str, author: &str, utc: &str) -> Result<CommitInfo, BackendError> {
    let bad = |what: &str| BackendError::BadFixture(format!("commit {sha}: {what}"));
    Ok(CommitInfo {
        sha: sha.parse().map_err(|err| bad(&format!("sha: {err}")))?,
        subject: subject.into(),
        author: author.into(),
        at: format!("2026-10-25T{utc}:00Z")
            .parse()
            .map_err(|err| bad(&format!("time: {err}")))?,
    })
}

/// fx1's changes against main, as the Git panel lists them.
fn fx1_changes() -> Vec<FileChange> {
    let main_rs = hunk(
        1,
        1,
        &[
            " mod app;",
            "-<<<<<<< HEAD",
            " mod cylinder;",
            " mod document;",
            "-=======",
            " mod minimap;",
            "-mod tabs;",
            "->>>>>>> 4d9fdc8",
            "+mod ui;",
            " ",
            "-use tabs::Tabs;",
            "+use ui::tabs::Tabs;",
            " ",
            " fn main() -> anyhow::Result<()> {",
            "     let tabs = Tabs::default();",
            "     app::run(tabs)",
            " }",
        ],
    );
    let cargo_lock = hunk(
        41,
        41,
        &[
            " [[package]]",
            " name = \"tabs\"",
            "-version = \"0.3.1\"",
            "+version = \"0.4.0\"",
        ],
    );
    let change = |path: &str, added, removed, hunks| FileChange {
        path: path.into(),
        kind: ChangeKind::Modified,
        added,
        removed,
        hunks,
    };
    vec![
        change("src/main.rs", 2, 5, vec![main_rs]),
        change("Cargo.lock", 1, 1, vec![cargo_lock]),
    ]
}

/// A hunk from unified diff lines: a `+`, `-` or space, then the text.
fn hunk(old_start: u32, new_start: u32, lines: &[&str]) -> Hunk {
    let lines = lines
        .iter()
        .map(|line| {
            let (kind, text) = match line.split_at_checked(1) {
                Some(("+", text)) => (DiffLineKind::Added, text),
                Some(("-", text)) => (DiffLineKind::Removed, text),
                Some((_, text)) => (DiffLineKind::Context, text),
                None => (DiffLineKind::Context, ""),
            };
            DiffLine {
                kind,
                text: text.to_owned(),
            }
        })
        .collect();
    Hunk {
        old_start,
        new_start,
        lines,
    }
}

/// Every local branch, 14 in all, as the Git panel's Branches section
/// counts them.
pub(super) const BRANCHES: [&str; 14] = [
    "docs/c011-readme",
    "feat/c009-ui-module",
    "feat/c012-minimap",
    "feat/c013-cylinder-view",
    "feat/c014-release-matrix",
    "fix/b-409-lock-drift",
    "fix/c010-document-save",
    "fix/c015-main-conflict",
    "fix/c016-it-tabs",
    "main",
    "plan/ridgeline",
    "sean/tabs",
    "test/b-412-tabs-order",
    "test/b-415-replay-paging",
];

/// console.py `MAIN_RS`: fx1's main.rs after the fix. The fix added `mod
/// ui;` and the new `use`, and removed the conflict markers, `mod tabs;` and
/// the old `use` (mockup fix 50).
fn fx1_main_rs() -> FileText {
    FileText {
        text: "mod app;\nmod cylinder;\nmod document;\nmod minimap;\nmod ui;\n\nuse ui::tabs::Tabs;\n\nfn main() -> anyhow::Result<()> {\n    let tabs = Tabs::default();\n    app::run(tabs)\n}\n".into(),
        marks: vec![
            GutterMark::Deleted(1),
            GutterMark::Deleted(3),
            GutterMark::Deleted(4),
            GutterMark::Added(5..6),
            GutterMark::Added(7..8),
        ],
    }
}
