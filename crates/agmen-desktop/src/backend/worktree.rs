//! Local worktrees as the Project and Git panels show them (Tasks 11 and
//! 12): files, changes, commits and state, from local git.

use std::collections::BTreeMap;
use std::ops::Range;

use agmen_proto::{GitSha, IdentityId, Timestamp};

/// A worktree path with empty and `.` segments dropped, as the Project
/// panel's tree builds it, so `src//main.rs` and `src/main.rs` match.
fn normalize_path(path: &str) -> String {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

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
    /// The diff against the base, in file order; empty when git has none
    /// to show, as for a binary file.
    pub(crate) hunks: Vec<Hunk>,
}

/// One commit on the worktree's branch, newest first in a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommitInfo {
    pub(crate) sha: GitSha,
    pub(crate) subject: String,
    /// The author's handle, such as `sean/a1`.
    pub(crate) author: String,
    /// When it was committed; views show it in the person's time zone.
    pub(crate) at: Timestamp,
}

/// One worktree on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeInfo {
    /// The identity working in it.
    pub(crate) owner: IdentityId,
    /// The owner's handle, such as `andy/fx1`.
    pub(crate) handle: String,
    /// What is checked out.
    pub(crate) head: WorktreeHead,
    /// Every tracked file, relative to the root with `/` separators, in
    /// path order; folders are the paths' parents.
    pub(crate) files: Vec<String>,
    pub(crate) changes: Vec<FileChange>,
    pub(crate) commits: Vec<CommitInfo>,
    pub(crate) state: WorktreeState,
    /// File contents the backend has read, by path, with their change
    /// marks; a file not here is asked of the backend when it opens
    /// (Task 8), or shows a placeholder.
    pub(crate) texts: BTreeMap<String, FileText>,
    /// Task 8: its folder on disk as agmend listed it, which `file_text`
    /// takes; `None` in the fixture.
    pub(crate) root: Option<String>,
}

/// A change mark beside a line. Line numbers start at 1; ranges are
/// end-exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GutterMark {
    Added(Range<u32>),
    Modified(Range<u32>),
    /// Lines were deleted just after this line; 0 means above line 1.
    Deleted(u32),
}

impl WorktreeInfo {
    /// The same worktree with every path normalized (files, changes and
    /// text keys), so views compare paths as they are. Backends call it
    /// once, where they build the worktree (C199).
    #[must_use]
    pub(crate) fn normalized(mut self) -> Self {
        for file in &mut self.files {
            *file = normalize_path(file);
        }
        for change in &mut self.changes {
            change.path = normalize_path(&change.path);
        }
        // Two raw keys that name the same file (`a//b` and `a/b`) keep the
        // first in path order; git never reports both (C215).
        let mut texts = BTreeMap::new();
        for (path, text) in std::mem::take(&mut self.texts) {
            texts.entry(normalize_path(&path)).or_insert(text);
        }
        self.texts = texts;
        self
    }
}

/// One file's text and the marks its diff gives the gutter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileText {
    pub(crate) text: String,
    pub(crate) marks: Vec<GutterMark>,
}

/// What a worktree has checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorktreeHead {
    /// A branch, such as `fix/c015-main-conflict`.
    Branch(String),
    /// A detached commit, such as a reviewer's checkout of the item it reads.
    Detached { at: GitSha, item: String },
}

impl WorktreeHead {
    /// The head as the panels name it: the branch, or `detached @9e2a71c`.
    #[must_use]
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Branch(name) => name.clone(),
            Self::Detached { at, .. } => format!("detached @{at}"),
        }
    }
}

/// How a diff line differs from the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DiffLineKind {
    Context,
    Added,
    Removed,
}

/// One line of a hunk, without its `+`, `-` or space prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub(crate) kind: DiffLineKind,
    pub(crate) text: String,
}

/// One hunk of a unified diff. Line numbers start at 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub(crate) old_start: u32,
    pub(crate) new_start: u32,
    pub(crate) lines: Vec<DiffLine>,
}

impl Hunk {
    /// The lines the hunk covers in the base (context and removed) and in
    /// the worktree (context and added).
    #[must_use]
    pub(crate) fn line_counts(&self) -> (usize, usize) {
        self.lines
            .iter()
            .fold((0, 0), |(old, new), line| match line.kind {
                DiffLineKind::Context => (old + 1, new + 1),
                DiffLineKind::Removed => (old + 1, new),
                DiffLineKind::Added => (old, new + 1),
            })
    }

    /// The unified diff header, such as `@@ -1,15 +1,12 @@`.
    #[must_use]
    pub(crate) fn header(&self) -> String {
        let (old, new) = self.line_counts();
        format!("@@ -{},{old} +{},{new} @@", self.old_start, self.new_start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_header_counts_each_side() {
        let line = |kind, text: &str| DiffLine {
            kind,
            text: text.to_owned(),
        };
        let hunk = Hunk {
            old_start: 4,
            new_start: 5,
            lines: vec![
                line(DiffLineKind::Context, "a"),
                line(DiffLineKind::Removed, "b"),
                line(DiffLineKind::Added, "c"),
                line(DiffLineKind::Added, "d"),
            ],
        };
        assert_eq!(hunk.header(), "@@ -4,2 +5,3 @@");
    }

    #[test]
    fn detached_head_names_its_commit() {
        let at: GitSha = "9e2a71c".parse().unwrap();
        let head = WorktreeHead::Detached {
            at,
            item: "C015".into(),
        };
        assert_eq!(head.label(), "detached @9e2a71c");
        assert_eq!(WorktreeHead::Branch("main".into()).label(), "main");
    }
}
