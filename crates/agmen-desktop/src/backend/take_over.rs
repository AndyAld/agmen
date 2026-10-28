//! Take-over (spec 5.4) and switching agents (spec 5.5): the terminal
//! screen of an agent session the person took over and the terminal agents
//! found on this machine (spec 5.2). The fixture's screens and how it acts
//! on them live in `fixture_take_over.rs`.
//!
// shortcut: the screens are fixture grids; spike S10 picks the PTY and the
// terminal emulator that fill `TerminalScreen` behind `Backend`, and keys
// typed into the terminal reach nothing until then.

use agmen_proto::IdentityId;

use super::Snapshot;

/// The widest and tallest screen kept; a bigger one is cut to this.
pub(crate) const MAX_COLS: u16 = 1000;
pub(crate) const MAX_ROWS: u16 = 500;

/// A cell's color, as a terminal emulator reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum TermColor {
    /// The terminal's own foreground or background.
    #[default]
    Default,
    /// One of the 16 ANSI colors: black, red, green, yellow, blue, magenta,
    /// cyan, white, then the bright ones.
    Ansi(u8),
    /// A 24-bit color the program chose itself.
    Rgb([u8; 3]),
}

/// One character cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TermCell {
    pub(crate) ch: char,
    pub(crate) fg: TermColor,
    pub(crate) bg: TermColor,
    pub(crate) bold: bool,
}

impl Default for TermCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: TermColor::Default,
            bg: TermColor::Default,
            bold: false,
        }
    }
}

/// What a terminal shows: `rows` rows of `cols` cells, and the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalScreen {
    cols: u16,
    rows: u16,
    /// Row by row, `cols` cells each.
    cells: Vec<TermCell>,
    /// Row and column of the block cursor, when it shows.
    cursor: Option<(u16, u16)>,
}

impl TerminalScreen {
    /// A blank screen, at most [`MAX_COLS`] by [`MAX_ROWS`].
    #[must_use]
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = (cols.min(MAX_COLS), rows.min(MAX_ROWS));
        Self {
            cols,
            rows,
            cells: vec![TermCell::default(); usize::from(cols) * usize::from(rows)],
            cursor: None,
        }
    }

    pub(crate) const fn cols(&self) -> u16 {
        self.cols
    }

    pub(crate) const fn rows(&self) -> u16 {
        self.rows
    }

    pub(crate) const fn cursor(&self) -> Option<(u16, u16)> {
        self.cursor
    }

    /// Row `row`'s cells, or nothing past the last row.
    pub(crate) fn row(&self, row: u16) -> &[TermCell] {
        let start = usize::from(row) * usize::from(self.cols);
        self.cells
            .get(start..start + usize::from(self.cols))
            .unwrap_or_default()
    }

    /// Writes `text` from `col` on `row` in `fg`; characters past the edge
    /// are dropped. Returns the column after the text.
    pub(crate) fn put(&mut self, row: u16, col: u16, text: &str, fg: TermColor, bold: bool) -> u16 {
        let mut c = col;
        for ch in text.chars() {
            if row < self.rows && c < self.cols {
                let ix = usize::from(row) * usize::from(self.cols) + usize::from(c);
                if let Some(cell) = self.cells.get_mut(ix) {
                    *cell = TermCell {
                        ch,
                        fg,
                        bg: cell.bg,
                        bold,
                    };
                }
            }
            c = c.saturating_add(1);
        }
        c
    }

    /// Shows the cursor at `row` and `col`, when that is on the screen.
    pub(crate) fn set_cursor(&mut self, row: u16, col: u16) {
        self.cursor = (row < self.rows && col < self.cols).then_some((row, col));
    }
}

/// How the taken-over session started.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SessionStart {
    /// The headless run's own session, resumed by its id.
    Resumed(String),
    /// A new session of another agent, started with a handoff brief.
    Handoff,
}

/// An agent session the person has taken over in the chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TakeOver {
    /// The agent's binary, such as `claude`.
    pub(crate) harness: String,
    pub(crate) start: SessionStart,
    pub(crate) screen: TerminalScreen,
}

/// Whether a terminal agent is on this machine, and new enough.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum AgentFound {
    Ready {
        version: String,
    },
    /// Found, but older than what agmen needs `why` for.
    Outdated {
        version: String,
        minimum: String,
        why: String,
    },
    /// On PATH, but `--version` timed out or printed nothing agmen could
    /// read, so agmen cannot tell whether it is new enough.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "FixtureBackend has no such agent; agmend's version probe makes it"
        )
    )]
    VersionUnknown,
    Missing,
}

/// Where an agent comes from, from agmend's list of known agents.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AgentSource {
    /// Who makes it, such as `xAI`.
    pub(crate) vendor: String,
    /// What it is, after the vendor: "terminal coding agent".
    pub(crate) about: String,
    /// The official install instructions.
    pub(crate) url: String,
    /// Another package that installs the same command, to warn about.
    pub(crate) look_alike: Option<String>,
}

/// A terminal agent agmend looked for on this machine (spec 5.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct InstalledAgent {
    /// The command, such as `grok`.
    pub(crate) name: String,
    /// The product name, such as `Grok Build`.
    pub(crate) title: String,
    pub(crate) found: AgentFound,
    /// The command that starts it over ACP.
    pub(crate) acp: String,
    pub(crate) source: Option<AgentSource>,
}

impl InstalledAgent {
    /// Whether the agent is on this machine at all, new enough or not: the
    /// one rule for listing it. The palette shows its spawn row, the switch
    /// menu lets a session switch to it and the model picker shows its
    /// models; a missing agent is in none of them.
    #[must_use]
    pub(crate) fn is_installed(&self) -> bool {
        self.found != AgentFound::Missing
    }

    /// Why agmen cannot run it as a worker, as the switch menu and the
    /// palette show it and the backend refuses its spawn; `None` when it
    /// is ready.
    #[must_use]
    pub(crate) fn not_ready(&self) -> Option<String> {
        match &self.found {
            AgentFound::Ready { .. } => None,
            AgentFound::Outdated { minimum, why, .. } => {
                Some(format!("update to {minimum}+ for {why}"))
            }
            AgentFound::VersionUnknown => Some("could not read its version".into()),
            AgentFound::Missing => Some("not installed".into()),
        }
    }

    /// What `--version` printed; `None` when it is not on PATH.
    #[must_use]
    pub(crate) fn version(&self) -> Option<&str> {
        match &self.found {
            AgentFound::Ready { version } | AgentFound::Outdated { version, .. } => Some(version),
            AgentFound::VersionUnknown | AgentFound::Missing => None,
        }
    }
}

/// Whether the person can take `id`'s session over now: an agent with a
/// live session on this machine (spec 5.4) that is not taken over yet.
#[must_use]
pub(crate) fn can_take_over(snapshot: &Snapshot, id: &IdentityId) -> bool {
    snapshot.sessions.contains_key(id) && !snapshot.take_overs.contains_key(id)
}

#[cfg(test)]
#[path = "take_over_tests.rs"]
mod tests;
