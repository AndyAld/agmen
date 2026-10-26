//! The fixture's take-over (Task 14, V66): andy/fx1's screens as the
//! take-over frames draw them, the terminal agents on andy's machine, and
//! how the fixture acts on `TakeOver`, `HandBack`, `SwitchAgent` and
//! `ScanAgents`. The types are the backend's own, in `take_over.rs`.

use agmen_proto::IdentityId;

use super::{
    Action, AgentFound, AgentInfo, AgentSource, BackendError, InstalledAgent, SessionStart,
    Snapshot, TakeOver, TermColor, TerminalScreen, can_take_over,
};

/// The fixture's session id for andy/fx1's headless run (console.py
/// `claude_screen`).
const FX1_SESSION: &str = "3f9c2e1a-7b40-4c1e-9d6a-2f0e8c5b1a77";

/// The fixture's worktree for andy/fx1.
const FX1_WORKTREE: &str = "~/agmen/worktrees/pr-3331/fix-c015-main-conflict";

/// The mockup's terminal grid: 104 by 19 (console.py `embedded_terminal`).
const GRID: (u16, u16) = (104, 19);

// The mockup's grid colors with no terminal token (reference sheet section
// 2): what Claude Code and Codex draw in 24-bit color.
const FAINT: TermColor = TermColor::Rgb([0x92, 0x83, 0x74]);
const ORANGE: TermColor = TermColor::Rgb([0xea, 0x89, 0x4b]);
const BRIGHT: TermColor = TermColor::Rgb([0xe0, 0xc8, 0x9e]);
const DIM: TermColor = TermColor::Rgb([0x7c, 0x6f, 0x64]);
/// Box rules: ANSI bright black.
const LINE: TermColor = TermColor::Ansi(8);
/// The brief's title: ANSI blue.
const BLUE: TermColor = TermColor::Ansi(4);
const TEXT: TermColor = TermColor::Default;

/// Writes `text` on `row` of `g` so it ends at the last column.
fn put_right(g: &mut TerminalScreen, row: u16, text: &str, fg: TermColor) {
    let len = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
    g.put(row, g.cols().saturating_sub(len), text, fg, false);
}

/// The input box at the bottom of an agent's screen, with `prompt` and the
/// cursor after it (console.py `claude_screen` and `codex_screen`).
fn input_box(g: &mut TerminalScreen, prompt: &str) {
    let (cols, rows) = (g.cols(), g.rows());
    let bottom = rows.saturating_sub(4);
    let rule = "─".repeat(usize::from(cols.saturating_sub(2)));
    g.put(bottom, 0, &format!("╭{rule}╮"), LINE, false);
    g.put(bottom + 1, 0, "│", LINE, false);
    g.put(bottom + 1, 2, prompt, TEXT, true);
    g.set_cursor(bottom + 1, 4);
    g.put(bottom + 1, cols.saturating_sub(1), "│", LINE, false);
    g.put(bottom + 2, 0, &format!("╰{rule}╯"), LINE, false);
    put_right(g, bottom + 3, "messages here stay on this machine", FAINT);
}

/// Claude Code resumed on fx1's session (console.py `claude_screen`,
/// compact).
fn claude_screen() -> TerminalScreen {
    let mut g = TerminalScreen::new(GRID.0, GRID.1);
    g.put(
        0,
        1,
        &format!("$ claude --resume {FX1_SESSION}"),
        FAINT,
        false,
    );
    g.put(
        2,
        1,
        &format!("● Resumed session · {FX1_WORKTREE}"),
        ORANGE,
        false,
    );
    g.put(
        4,
        1,
        "> why did you drop the old tabs import?",
        BRIGHT,
        false,
    );
    g.put(
        6,
        1,
        "● Sean's 016f235 moved the tabs module to src/ui/tabs.rs, so the old `mod tabs;` line points at a",
        TEXT,
        false,
    );
    g.put(
        7,
        1,
        "file that no longer exists. The new import is on line 7.",
        TEXT,
        false,
    );
    input_box(&mut g, ">");
    g.put(GRID.1 - 1, 2, "? for shortcuts", FAINT, false);
    g
}

/// A new session of `agent` started with agmen's handoff brief (console.py
/// `codex_screen`).
fn handoff_screen(agent: &str) -> TerminalScreen {
    let mut g = TerminalScreen::new(GRID.0, GRID.1);
    let end = g.put(0, 1, &format!("$ {agent}"), FAINT, false);
    g.put(
        0,
        end + 1,
        &format!("# started by agmen as andy/fx1 in {FX1_WORKTREE}"),
        DIM,
        false,
    );
    g.put(2, 1, "agmen handoff brief", BLUE, true);
    let brief = [
        "item C015 · lane L-CORE · branch fix/c015-main-conflict, 1 commit, pushed",
        "done: conflict markers removed, cargo test 71 passed, DONE #1866, REVIEW pass",
        "next: when Sean merges #10, rebase if main moved and rerun cargo test",
        "earlier session: claude 3f9c2e1a, still resumable",
    ];
    for (row, line) in (3..).zip(brief) {
        g.put(row, 3, line, TEXT, false);
    }
    g.put(
        8,
        1,
        "• main has not moved since 9e2a71c, so there is nothing to rebase yet.",
        TEXT,
        false,
    );
    g.put(9, 1, "  Watching for the #10 merge.", TEXT, false);
    input_box(&mut g, "›");
    g
}

/// The terminal agents on andy's machine (console.py `INSTALLED`, spec 5.2).
pub(super) fn installed_agents() -> Vec<InstalledAgent> {
    let agent = |name: &str, title: &str, found: AgentFound, acp: &str| InstalledAgent {
        name: name.into(),
        title: title.into(),
        found,
        acp: acp.into(),
        source: None,
    };
    let ready = |version: &str| AgentFound::Ready {
        version: version.into(),
    };
    vec![
        agent(
            "claude",
            "Claude Code",
            ready("2.1.310"),
            "claude-agent-acp",
        ),
        agent("codex", "Codex", ready("0.164.0"), "codex-acp"),
        agent("opencode", "OpenCode", ready("2.1.4"), "opencode acp"),
        agent("gemini", "Gemini CLI", ready("0.46.0"), "gemini --acp"),
        agent(
            "agy",
            "Antigravity",
            AgentFound::Outdated {
                version: "1.1.7".into(),
                minimum: "1.1.15".into(),
                why: "JSON".into(),
            },
            "agy_acp_server",
        ),
        InstalledAgent {
            source: Some(AgentSource {
                vendor: "xAI".into(),
                about: "terminal coding agent".into(),
                url: "github.com/xai-org/grok-build".into(),
                look_alike: Some("the community npm package @vibe-kit/grok-cli".into()),
            }),
            ..agent(
                "grok",
                "Grok Build",
                AgentFound::Missing,
                "grok agent stdio",
            )
        },
    ]
}

/// What the Agent panel shows for fx1 running `agent`: harness, model, the
/// tools it enforces, and whether that list says less than the role file
/// (spec 5.5).
fn harness_details(agent: &str) -> (&'static str, &'static str, &'static str, bool) {
    match agent {
        "codex" => (
            "Codex 0.164.0 · ACP, since 15:52",
            "Codex default (standard tier)",
            "sandbox workspace-write (Codex has no tool list)",
            true,
        ),
        "opencode" => (
            "OpenCode 2.1.4 · ACP, since 15:52",
            "OpenCode default (standard tier)",
            "agent permissions (not verified)",
            true,
        ),
        "gemini" => (
            "Gemini CLI 0.46.0 · ACP, since 15:52",
            "Gemini default (standard tier)",
            "approval mode plan and policy (Gemini has no tool list)",
            true,
        ),
        "agy" => (
            "Antigravity 1.1.7 · ACP, since 15:52",
            "Antigravity default (standard tier)",
            "settings allow and deny rules (agy has no tool list)",
            true,
        ),
        _ => (
            "Claude Code 2.1.310 · CLI adapter",
            "sonnet (standard tier)",
            "Read Edit Write Bash Grep Glob",
            false,
        ),
    }
}

fn set_harness(info: &mut AgentInfo, agent: &str) {
    let (harness, model, tools, lossy) = harness_details(agent);
    info.harness_label = harness.into();
    info.model = model.into();
    // A lossy list is one phrase; a tool list is one tool per word.
    info.tools = if lossy {
        vec![tools.into()]
    } else {
        tools.split(' ').map(Into::into).collect()
    };
    info.tools_lossy = lossy;
}

/// The roster row of agent `id`, when there is one.
fn roster_harness<'a>(snapshot: &'a mut Snapshot, id: &IdentityId) -> Option<&'a mut String> {
    snapshot
        .roster
        .iter_mut()
        .flat_map(|group| group.agents.iter_mut())
        .find(|agent| &agent.id == id)
        .map(|agent| &mut agent.harness)
}

/// Takes andy/fx1 over, as the take-over frames show it (V66); with
/// `codex`, then switches it to codex, as "Desktop · chat + codex".
///
/// # Errors
///
/// When the fixture has no andy/fx1 session to take over.
pub(super) fn fx1_taken_over(
    mut snapshot: Snapshot,
    codex: bool,
) -> Result<Snapshot, BackendError> {
    let worker = super::fixture_data::id("andy/fx1")?;
    act(&mut snapshot, Action::TakeOver(worker.clone()))?;
    if codex {
        let agent = "codex".into();
        act(&mut snapshot, Action::SwitchAgent { worker, agent })?;
    }
    Ok(snapshot)
}

/// Does a take-over action on the fixture's `snapshot`. `TakeOver` resumes
/// the agent's own Claude session, or hands another agent the brief again; `HandBack` ends it; `SwitchAgent` starts a new
/// session of an installed agent with a handoff brief; `ScanAgents` finds
/// the same agents again and counts the scan. Other actions are not take-over actions.
///
/// # Errors
///
/// When the agent cannot be taken over, is not taken over, or the agent to
/// switch to is not installed.
pub(super) fn act(snapshot: &mut Snapshot, action: Action) -> Result<(), BackendError> {
    match action {
        Action::TakeOver(id) => {
            if snapshot.take_overs.contains_key(&id) {
                return Ok(());
            }
            if !can_take_over(snapshot, &id) {
                return Err(BackendError::Unsupported(
                    "taking over an agent with no session on this machine",
                ));
            }
            let harness = roster_harness(snapshot, &id)
                .map(|h| h.clone())
                .unwrap_or_else(|| "claude".into());
            // Only Claude has the headless run's session to resume; another
            // agent the person switched to starts from the handoff again.
            let (start, screen) = if harness == "claude" {
                (SessionStart::Resumed(FX1_SESSION.into()), claude_screen())
            } else {
                (SessionStart::Handoff, handoff_screen(&harness))
            };
            snapshot.take_overs.insert(
                id,
                TakeOver {
                    harness,
                    start,
                    screen,
                },
            );
            Ok(())
        }
        Action::HandBack(id) => {
            snapshot
                .take_overs
                .remove(&id)
                .map(drop)
                .ok_or(BackendError::Unsupported(
                    "handing back an agent that is not taken over",
                ))
        }
        Action::SwitchAgent { worker, agent } => {
            let installed = snapshot
                .installed_agents
                .iter()
                .any(|a| a.name == agent && a.is_installed());
            if !installed {
                return Err(BackendError::Unsupported(
                    "switching to an agent that is not installed",
                ));
            }
            let Some(current) = snapshot.take_overs.get_mut(&worker) else {
                return Err(BackendError::Unsupported(
                    "switching an agent that is not taken over",
                ));
            };
            if current.harness == agent {
                return Ok(());
            }
            *current = TakeOver {
                screen: handoff_screen(&agent),
                harness: agent.clone(),
                start: SessionStart::Handoff,
            };
            if let Some(harness) = roster_harness(snapshot, &worker) {
                harness.clone_from(&agent);
            }
            if let Some(info) = snapshot.agents.get_mut(&worker) {
                set_harness(info, &agent);
            }
            Ok(())
        }
        // The fixture's machine never changes, so a scan finds the same
        // agents; it still counts, so Check again sees its result.
        Action::ScanAgents => {
            snapshot.agent_scans = snapshot.agent_scans.wrapping_add(1);
            Ok(())
        }
        _ => Err(BackendError::Unsupported("that action here")),
    }
}

#[cfg(test)]
#[path = "fixture_take_over_tests.rs"]
mod tests;
