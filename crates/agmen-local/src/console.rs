//! Types for the desktop console: a snapshot of a project's chat, then events.

use agmen_proto::{GitSha, IdentityId, IdentityKind, StampedEnvelope, Timestamp};
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
    /// The role supplied when the identity was minted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
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
    /// Role files from the built-in, project and personal layers (spec
    /// 3.5), in that order. The project layer is empty unless the request
    /// named a checkout of this project.
    #[serde(default)]
    pub roles: Vec<RoleFileInfo>,
    /// The worktrees of the checkout the request named; empty without one.
    #[serde(default)]
    pub worktrees: Vec<WorktreeEntry>,
    /// What local git knows across those worktrees.
    #[serde(default)]
    pub git: GitFactsEntry,
}

/// Where a role file comes from (spec 3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleLayer {
    /// Compiled into agmen.
    BuiltIn,
    /// `.agmen/roles/*.xml` in the project's repo.
    Project,
    /// `~/.config/agmen/roles/*.xml`.
    Personal,
}

/// One role file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleFileInfo {
    /// The `<role name>`, or the file name without `.xml` when the file
    /// does not parse.
    pub name: String,
    /// Its layer.
    pub layer: RoleLayer,
    /// The XML as written.
    pub text: String,
}

/// What a worktree has checked out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HeadEntry {
    /// A branch, by its short name.
    Branch {
        /// Such as `fix/c015-main-conflict`.
        name: String,
    },
    /// A detached commit.
    Detached {
        /// The full commit id.
        sha: GitSha,
    },
    /// A branch with no commits yet.
    Unborn {
        /// The branch's short name.
        name: String,
    },
}

/// How a branch stands against its upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Upstream {
    /// Commits here and not upstream.
    pub ahead: u32,
    /// Commits upstream and not here.
    pub behind: u32,
}

/// How a file differs from `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKindEntry {
    /// New, staged or untracked.
    Added,
    /// Changed, renamed or in conflict.
    Modified,
    /// Removed.
    Deleted,
}

/// How a diff line differs from `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKindEntry {
    /// The same on both sides.
    Context,
    /// Only in the worktree.
    Added,
    /// Only in `HEAD`.
    Removed,
}

/// One line of a hunk, without its `+`, `-` or space prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLineEntry {
    /// Which side it is on.
    pub kind: DiffLineKindEntry,
    /// The line, lossily decoded when it is not UTF-8.
    pub text: String,
}

/// One hunk of a unified diff. Line numbers start at 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkEntry {
    /// Its first line in `HEAD`.
    pub old_start: u32,
    /// Its first line in the worktree.
    pub new_start: u32,
    /// Its lines, in order.
    pub lines: Vec<DiffLineEntry>,
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeEntry {
    /// Relative to the worktree root, with `/` separators.
    pub path: String,
    /// How it changed.
    pub kind: ChangeKindEntry,
    /// Lines added; 0 for a binary or untracked file.
    pub added: u32,
    /// Lines removed; 0 for a binary or untracked file.
    pub removed: u32,
    /// The diff against `HEAD`; empty when agmend did not read it (binary,
    /// untracked, or past its caps).
    #[serde(default)]
    pub hunks: Vec<HunkEntry>,
}

/// One commit, newest first in a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitEntry {
    /// The full commit id.
    pub sha: GitSha,
    /// git's shortest unambiguous abbreviation of it.
    pub short: GitSha,
    /// The first line of its message.
    pub subject: String,
    /// The author's name as git has it.
    pub author: String,
    /// When it was committed.
    pub at: Timestamp,
}

/// One worktree of the checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeEntry {
    /// Its folder, as `git worktree list` names it. `file_text` takes it.
    pub path: String,
    /// The identity its `.agmen/identity.json` names, when that file passes
    /// agmen's trust test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Me>,
    /// What is checked out.
    pub head: HeadEntry,
    /// The branch's upstream, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<Upstream>,
    /// Tracked files in path order, relative to the root.
    pub files: Vec<String>,
    /// Changes against `HEAD`, in path order.
    pub changes: Vec<ChangeEntry>,
    /// The newest commits on `HEAD`, newest first.
    pub commits: Vec<CommitEntry>,
    /// Whether `files` or `changes` stopped at agmend's caps.
    #[serde(default)]
    pub truncated: bool,
}

/// Branches, commits and files local git knows, sorted, each once.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFactsEntry {
    /// Local branch names.
    pub branches: Vec<String>,
    /// Full ids of the commits the worktrees list.
    pub commits: Vec<String>,
    /// Tracked paths in any worktree.
    pub files: Vec<String>,
}

/// A change mark beside a line. Lines start at 1; ranges are
/// end-exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MarkEntry {
    /// Lines `from..to` are new.
    Added {
        /// The first line.
        from: u32,
        /// One past the last line.
        to: u32,
    },
    /// Lines `from..to` replace others.
    Modified {
        /// The first line.
        from: u32,
        /// One past the last line.
        to: u32,
    },
    /// Lines were removed just after line `after`; 0 means above line 1.
    Deleted {
        /// The line above the removal.
        after: u32,
    },
}

/// `file_text`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTextEntry {
    /// The file's text, at most 512 KiB of it.
    pub text: String,
    /// The gutter's marks against `HEAD`.
    pub marks: Vec<MarkEntry>,
    /// Whether the text stops at the cap.
    #[serde(default)]
    pub truncated: bool,
    /// The whole file's size in bytes; 0 from an agmend that does not say.
    #[serde(default)]
    pub size: u64,
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
    /// The daemon's hub connection went up or down, or the reason it stays
    /// down changed.
    Hub {
        /// Whether the hub is reachable.
        connected: bool,
        /// Why the hub refuses the connection, when a retry soon will not
        /// help by itself: not signed in, no access to the project, or too
        /// many streams. Left out while connected or merely unreachable.
        /// The daemon keeps retrying either way.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<crate::ApiError>,
    },
    /// What agmend reads from the watch's checkout changed (Task 8); this
    /// is all of it. Sent only to a watch that named a `root`. An older
    /// reader skips an event it does not know.
    Local(Box<LocalData>),
}

/// The role files, worktrees and git facts agmend read from disk, as a
/// snapshot carries them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalData {
    /// As [`ConsoleSnapshot::roles`].
    #[serde(default)]
    pub roles: Vec<RoleFileInfo>,
    /// As [`ConsoleSnapshot::worktrees`].
    #[serde(default)]
    pub worktrees: Vec<WorktreeEntry>,
    /// As [`ConsoleSnapshot::git`].
    #[serde(default)]
    pub git: GitFactsEntry,
}
