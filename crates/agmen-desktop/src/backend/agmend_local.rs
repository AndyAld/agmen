//! Task 8: agmend's role files, worktrees and file texts as the window's
//! types, for [`super::agmend`]'s snapshots.

use std::collections::BTreeMap;
use std::path::Path;

use agmen_local::console::{
    ChangeEntry, ChangeKindEntry, DiffLineKindEntry, FileTextEntry, HeadEntry, MarkEntry, Me,
    RoleLayer as WireLayer, WorktreeEntry,
};
use agmen_proto::IdentityId;

use super::{
    ChangeKind, CommitInfo, DiffLine, DiffLineKind, FileChange, FileText, GutterMark, Hunk,
    RoleLayer, WorktreeHead, WorktreeInfo, WorktreeState,
};

/// A role file's layer as the Roles panel names it.
pub(super) const fn role_layer(layer: WireLayer) -> RoleLayer {
    match layer {
        WireLayer::BuiltIn => RoleLayer::BuiltIn,
        WireLayer::Project => RoleLayer::Project,
        WireLayer::Personal => RoleLayer::Personal,
    }
}

/// agmend's worktrees as the Project and Git panels take them. One with a
/// trusted identity file is that identity's; any other is the person's
/// own, named `{person}/{folder}`, with a number added when two would
/// share a handle (views find worktrees by handle).
pub(super) fn worktrees_from(entries: Vec<WorktreeEntry>, me: &Me) -> Vec<WorktreeInfo> {
    let mut taken: Vec<String> = Vec::new();
    entries
        .into_iter()
        .map(|entry| {
            let (owner, mut handle) = match &entry.identity {
                Some(identity) => (identity.id.clone(), identity.handle.clone()),
                None => {
                    let folder = Path::new(&entry.path)
                        .file_name()
                        .map_or_else(|| entry.path.clone(), |f| f.to_string_lossy().into_owned());
                    (me.id.clone(), format!("{}/{folder}", me.handle))
                }
            };
            if taken.contains(&handle) {
                handle = format!("{handle}-{}", taken.len() + 1);
            }
            taken.push(handle.clone());
            worktree_from(entry, owner, handle)
        })
        .collect()
}

fn worktree_from(entry: WorktreeEntry, owner: IdentityId, handle: String) -> WorktreeInfo {
    let state = match (&entry.head, entry.upstream) {
        (HeadEntry::Detached { .. }, _) => WorktreeState::Reading,
        (_, Some(upstream)) if upstream.ahead == 0 => WorktreeState::Pushed,
        _ => WorktreeState::LocalOnly,
    };
    let head = match entry.head {
        HeadEntry::Branch { name } | HeadEntry::Unborn { name } => WorktreeHead::Branch(name),
        HeadEntry::Detached { sha } => WorktreeHead::Detached {
            // The label shows git's usual 7 digits.
            at: sha
                .as_str()
                .get(..7)
                .and_then(|s| s.parse().ok())
                .unwrap_or(sha),
            item: String::new(),
        },
    };
    WorktreeInfo {
        owner,
        handle,
        head,
        files: entry.files,
        changes: entry.changes.into_iter().map(change_from).collect(),
        commits: entry
            .commits
            .into_iter()
            .map(|c| CommitInfo {
                sha: c.short,
                subject: c.subject,
                author: c.author,
                at: c.at,
            })
            .collect(),
        state,
        texts: BTreeMap::new(),
        root: Some(entry.path),
    }
    .normalized()
}

fn change_from(change: ChangeEntry) -> FileChange {
    FileChange {
        path: change.path,
        kind: match change.kind {
            ChangeKindEntry::Added => ChangeKind::Added,
            ChangeKindEntry::Modified => ChangeKind::Modified,
            ChangeKindEntry::Deleted => ChangeKind::Deleted,
        },
        added: change.added,
        removed: change.removed,
        hunks: change
            .hunks
            .into_iter()
            .map(|hunk| Hunk {
                old_start: hunk.old_start,
                new_start: hunk.new_start,
                lines: hunk
                    .lines
                    .into_iter()
                    .map(|line| DiffLine {
                        kind: match line.kind {
                            DiffLineKindEntry::Context => DiffLineKind::Context,
                            DiffLineKindEntry::Added => DiffLineKind::Added,
                            DiffLineKindEntry::Removed => DiffLineKind::Removed,
                        },
                        text: line.text,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// `file_text`'s answer as the file view shows it.
pub(super) fn file_text_from(entry: FileTextEntry) -> FileText {
    FileText {
        full_size: entry.truncated.then_some(entry.size),
        text: entry.text,
        marks: entry
            .marks
            .into_iter()
            .map(|mark| match mark {
                MarkEntry::Added { from, to } => GutterMark::Added(from..to),
                MarkEntry::Modified { from, to } => GutterMark::Modified(from..to),
                MarkEntry::Deleted { after } => GutterMark::Deleted(after),
            })
            .collect(),
    }
}
