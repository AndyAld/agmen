//! Local worktrees as the Project and Git panels show them (Tasks 11 and
//! 12): files, changes, commits and state, from local git.

use agmen_proto::{GitSha, IdentityId};

/// Whether a worktree's commits have left this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WorktreeState {
    /// Its branch is pushed and up to date.
    Pushed,
    /// Commits that exist only here.
    LocalOnly,
    /// A detached checkout for reading, such as a reviewer's.
    Reading,
}

/// How a file differs from the worktree's base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

/// One changed file and its line counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileChange {
    /// Relative to the worktree root, with `/` separators.
    pub(crate) path: String,
    pub(crate) kind: ChangeKind,
    pub(crate) added: u32,
    pub(crate) removed: u32,
}

/// One commit on the worktree's branch, newest first in a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommitInfo {
    pub(crate) sha: GitSha,
    pub(crate) subject: String,
    pub(crate) author: String,
    /// When, as the panel shows it, such as "15:31".
    pub(crate) when: String,
}

/// One worktree on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeInfo {
    /// The identity working in it.
    pub(crate) owner: IdentityId,
    /// The owner's handle, such as `andy/fx1`.
    pub(crate) handle: String,
    /// The branch, or for a detached checkout the item it reads.
    pub(crate) branch: String,
    /// Every tracked file, relative to the root with `/` separators, in
    /// path order; folders are the paths' parents.
    pub(crate) files: Vec<String>,
    pub(crate) changes: Vec<FileChange>,
    pub(crate) commits: Vec<CommitInfo>,
    pub(crate) state: WorktreeState,
}
