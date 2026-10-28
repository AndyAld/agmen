//! Claude Code's saved conversations for a folder, found by file name only.
//!
//! Claude Code keeps each conversation as
//! `<claude home>/projects/<encoded folder>/<session id>.jsonl`, where the
//! encoded folder is the absolute path with every character other than an
//! ASCII letter or digit replaced by `-` (checked against this machine's
//! `~/.claude/projects`: `/` and `.` both become `-`). agmend only lists these
//! names and reads their modification times; it never reads a transcript's
//! contents, which are untrusted data.

use std::path::{Path, PathBuf};

/// The most directory entries one lookup reads.
const MAX_ENTRIES: usize = 4096;

/// Claude Code's own folder: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub(crate) fn home_from_env() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".claude")))
}

/// `folder` as Claude Code names its transcript folder, when it is UTF-8.
pub(crate) fn encode(folder: &Path) -> Option<String> {
    let text = folder.to_str()?;
    let mut encoded = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            encoded.push(c);
        } else {
            // JavaScript replaces each UTF-16 unit, so a character outside
            // the basic plane becomes two dashes.
            for _ in 0..c.len_utf16() {
                encoded.push('-');
            }
        }
    }
    Some(encoded)
}

fn project_dir(home: &Path, folder: &Path) -> Option<PathBuf> {
    Some(home.join("projects").join(encode(folder)?))
}

/// Whether `id` names a session id agmend may pass as one argv value.
pub(crate) fn is_session_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

/// Whether Claude Code saved session `id` for `folder`.
pub(crate) async fn exists(home: Option<&Path>, folder: &Path, id: &str) -> bool {
    let Some(path) = home
        .and_then(|home| project_dir(home, folder))
        .filter(|_| is_session_id(id))
        .map(|dir| dir.join(format!("{id}.jsonl")))
    else {
        return false;
    };
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|meta| meta.is_file())
}

/// The session id of `folder`'s most recently written transcript.
pub(crate) async fn newest(home: Option<&Path>, folder: &Path) -> Option<String> {
    let dir = project_dir(home?, folder)?;
    tokio::task::spawn_blocking(move || {
        let mut best: Option<(std::time::SystemTime, String)> = None;
        for entry in std::fs::read_dir(dir).ok()?.take(MAX_ENTRIES) {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|name| name.strip_suffix(".jsonl"))
                .filter(|id| is_session_id(id))
            else {
                continue;
            };
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            if meta.is_file() && best.as_ref().is_none_or(|(time, _)| modified > *time) {
                best = Some((modified, id.to_owned()));
            }
        }
        best.map(|(_, id)| id)
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn folders_encode_as_this_machines_claude_projects() {
        // Both pairs are real folders and their ~/.claude/projects names.
        assert_eq!(
            encode(Path::new("/Users/andyaldrich/Projects/Marked-sol")).unwrap(),
            "-Users-andyaldrich-Projects-Marked-sol"
        );
        assert_eq!(
            encode(Path::new(
                "/Users/andyaldrich/Projects/Marked/.delta-worktrees/gntbv2afc8qa/Marked"
            ))
            .unwrap(),
            "-Users-andyaldrich-Projects-Marked--delta-worktrees-gntbv2afc8qa-Marked"
        );
    }

    #[test]
    fn only_uuid_shaped_names_are_session_ids() {
        assert!(is_session_id("d2021aaf-efaf-49bc-9838-a2f0bee14a50"));
        for id in [
            "",
            "--dangerously-skip-permissions",
            "d2021aaf-efaf-49bc-9838-a2f0bee14a5",
            "d2021aaf-efaf-49bc-9838-a2f0bee14a5g",
            "d2021aafxefaf-49bc-9838-a2f0bee14a50",
        ] {
            assert!(!is_session_id(id), "{id}");
        }
    }

    #[tokio::test]
    async fn newest_transcript_wins_and_other_files_are_ignored() {
        let home = tempfile::tempdir().unwrap();
        let folder = Path::new("/work/my.repo");
        let dir = home.path().join("projects/-work-my-repo");
        std::fs::create_dir_all(&dir).unwrap();
        let old = "11111111-1111-4111-8111-111111111111";
        let new = "22222222-2222-4222-8222-222222222222";
        std::fs::write(dir.join(format!("{old}.jsonl")), "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join(format!("{new}.jsonl")), "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("notes.jsonl"), "{}").unwrap();
        std::fs::create_dir(dir.join("33333333-3333-4333-8333-333333333333.jsonl")).unwrap();
        assert_eq!(
            newest(Some(home.path()), folder).await.as_deref(),
            Some(new)
        );
        assert!(exists(Some(home.path()), folder, old).await);
        assert!(
            !exists(
                Some(home.path()),
                folder,
                "33333333-3333-4333-8333-333333333333"
            )
            .await
        );
        assert!(!exists(None, folder, old).await);
        assert_eq!(
            newest(Some(home.path()), Path::new("/elsewhere")).await,
            None
        );
    }
}
