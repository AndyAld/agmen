//! Running git in the person's checkout and reading its machine output
//! (Task 8). git runs directly, never through a shell, with
//! `-c core.quotepath=off`, `--no-optional-locks` and literal pathspecs,
//! and each call is bounded in time and in output.
//!
//! Every list is read from `-z` or porcelain v2 output, so paths with
//! spaces, quotes or newlines parse. A path that is not UTF-8 cannot be
//! named in the local API's JSON, so the readers skip it.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use agmen_local::console::{
    ChangeKindEntry, DiffLineEntry, DiffLineKindEntry, HunkEntry, MarkEntry, Upstream,
};
use tokio::io::AsyncReadExt as _;
use tokio::time::Instant;

/// The longest one git call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a git call gave no output.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GitError {
    /// git could not be started or read.
    #[error("git could not run: {0}")]
    Io(#[from] std::io::Error),
    /// git ran past its time.
    #[error("git {0} did not answer in time")]
    Timeout(String),
    /// git exited with an error, such as `HEAD` in a repository with no
    /// commits.
    #[error("git {0} failed")]
    Failed(String),
}

/// What a git call printed.
#[derive(Debug)]
pub(crate) struct Output {
    pub(crate) stdout: Vec<u8>,
    /// Whether git printed more than the cap, so `stdout` is cut short and
    /// its last record may be partial.
    pub(crate) truncated: bool,
}

/// git in one folder, with a deadline shared by every call.
#[derive(Debug, Clone)]
pub(crate) struct Git {
    dir: PathBuf,
    deadline: Instant,
}

impl Git {
    pub(crate) fn new(dir: impl Into<PathBuf>, deadline: Instant) -> Self {
        Self {
            dir: dir.into(),
            deadline,
        }
    }

    /// Runs `git <args>`, keeping at most `cap` bytes of its output; past
    /// the cap git is stopped and the output marked truncated.
    pub(crate) async fn run(&self, args: &[&OsStr], cap: usize) -> Result<Output, GitError> {
        let what = args
            .first()
            .map(|a| a.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut child = tokio::process::Command::new("git")
            .args(["-c", "core.quotepath=off", "-c", "core.fsmonitor=false"])
            .arg("--no-optional-locks")
            .args(args)
            .current_dir(&self.dir)
            // The checkout git finds from its folder, not one agmend's own
            // environment names.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let deadline = self.deadline.min(Instant::now() + CALL_TIMEOUT);
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| GitError::Failed(what.clone()))?;
        let work = async {
            let mut out = Vec::new();
            let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
            (&mut stdout).take(limit).read_to_end(&mut out).await?;
            let truncated = out.len() > cap;
            if truncated {
                out.truncate(cap);
                // git would block on a full pipe; the output is enough.
                child.start_kill()?;
                child.wait().await?;
                return Ok::<_, std::io::Error>((out, truncated, true));
            }
            let status = child.wait().await?;
            Ok((out, truncated, status.success()))
        };
        match tokio::time::timeout_at(deadline, work).await {
            Err(_) => Err(GitError::Timeout(what)),
            Ok(Err(err)) => Err(err.into()),
            Ok(Ok((_, _, false))) => Err(GitError::Failed(what)),
            Ok(Ok((stdout, truncated, true))) => Ok(Output { stdout, truncated }),
        }
    }

    /// As [`Git::run`], for arguments that are all plain text.
    pub(crate) async fn run_str(&self, args: &[&str], cap: usize) -> Result<Output, GitError> {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        self.run(&args, cap).await
    }
}

/// The NUL-terminated records of `-z` output. A record with no NUL after
/// it, as when the output was cut short, is dropped.
pub(crate) fn records(out: &Output) -> impl Iterator<Item = &[u8]> {
    let mut parts: Vec<&[u8]> = out.stdout.split(|b| *b == 0).collect();
    // The last part is empty after a complete output's final NUL, or the
    // partial record a cut one ends in: drop it either way.
    parts.pop();
    parts.into_iter()
}

/// `bytes` as UTF-8, or `None`: the local API's JSON cannot carry a path
/// that is not.
pub(crate) fn utf8(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

/// One `git worktree list --porcelain -z` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListedWorktree {
    pub(crate) path: PathBuf,
    /// The checked-out commit; all zeros on an unborn branch.
    pub(crate) head: Option<String>,
    /// The branch's full ref, such as `refs/heads/main`.
    pub(crate) branch: Option<String>,
    /// Bare, or prunable because its folder is gone: nothing to read.
    pub(crate) skip: bool,
}

/// Parses `git worktree list --porcelain -z`: attributes end in NUL, and an
/// empty attribute ends a worktree.
pub(crate) fn parse_worktrees(out: &Output) -> Vec<ListedWorktree> {
    let mut list = Vec::new();
    let mut current: Option<ListedWorktree> = None;
    for field in records(out) {
        if field.is_empty() {
            list.extend(current.take());
            continue;
        }
        let (key, value) = match field.iter().position(|b| *b == b' ') {
            Some(at) => (&field[..at], &field[at + 1..]),
            None => (field, &[][..]),
        };
        match key {
            b"worktree" => {
                list.extend(current.take());
                current = Some(ListedWorktree {
                    path: PathBuf::from(OsStr::from_bytes(value)),
                    head: None,
                    branch: None,
                    skip: false,
                });
            }
            b"HEAD" => {
                if let Some(wt) = current.as_mut() {
                    wt.head = utf8(value);
                }
            }
            b"branch" => {
                if let Some(wt) = current.as_mut() {
                    wt.branch = utf8(value);
                }
            }
            b"bare" | b"prunable" => {
                if let Some(wt) = current.as_mut() {
                    wt.skip = true;
                }
            }
            _ => {}
        }
    }
    list.extend(current);
    list
}

/// What `git status --porcelain=v2 --branch -z` says of a worktree.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Status {
    pub(crate) upstream: Option<Upstream>,
    /// Changed paths and how, in git's order; untracked ones are
    /// [`ChangeKindEntry::Added`] and flagged.
    pub(crate) entries: Vec<StatusEntry>,
}

/// One changed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusEntry {
    pub(crate) path: String,
    pub(crate) kind: ChangeKindEntry,
    pub(crate) untracked: bool,
}

/// How a porcelain v2 `XY` pair reads against `HEAD`.
fn kind_of(xy: &[u8]) -> ChangeKindEntry {
    if xy.contains(&b'D') {
        ChangeKindEntry::Deleted
    } else if xy.contains(&b'A') {
        ChangeKindEntry::Added
    } else {
        ChangeKindEntry::Modified
    }
}

/// The last of `count` space-separated fields, which may hold spaces.
fn field_tail(record: &[u8], count: usize) -> Option<&[u8]> {
    record.splitn(count, |b| *b == b' ').nth(count - 1)
}

/// Parses `git status --porcelain=v2 --branch -z`. A path that is not
/// UTF-8 is skipped.
pub(crate) fn parse_status(out: &Output) -> Status {
    let mut status = Status::default();
    let mut fields = records(out);
    while let Some(record) = fields.next() {
        let path = match record.first() {
            Some(b'#') => {
                if let Some(ab) = record.strip_prefix(b"# branch.ab ") {
                    status.upstream = parse_ab(ab);
                }
                continue;
            }
            // 1 XY sub mH mI mW hH hI path
            Some(b'1') => field_tail(record, 9),
            // 2 XY sub mH mI mW hH hI Xscore path, then the old path.
            Some(b'2') => {
                fields.next();
                field_tail(record, 10)
            }
            // u XY sub m1 m2 m3 mW h1 h2 h3 path
            Some(b'u') => field_tail(record, 11),
            Some(b'?') => {
                if let Some(path) = record.get(2..).and_then(utf8) {
                    status.entries.push(StatusEntry {
                        path,
                        kind: ChangeKindEntry::Added,
                        untracked: true,
                    });
                }
                continue;
            }
            _ => continue,
        };
        let kind = match record.first() {
            Some(b'u') => ChangeKindEntry::Modified,
            _ => kind_of(record.get(2..4).unwrap_or_default()),
        };
        if let Some(path) = path.and_then(utf8) {
            status.entries.push(StatusEntry {
                path,
                kind,
                untracked: false,
            });
        }
    }
    status
}

/// `+A -B` as an upstream's ahead and behind counts.
fn parse_ab(ab: &[u8]) -> Option<Upstream> {
    let text = std::str::from_utf8(ab).ok()?;
    let (ahead, behind) = text.split_once(' ')?;
    Some(Upstream {
        ahead: ahead.strip_prefix('+')?.parse().ok()?,
        behind: behind.strip_prefix('-')?.parse().ok()?,
    })
}

/// One `git diff --numstat -z --no-renames` record: added, removed and the
/// raw path; a binary file counts 0 and 0.
pub(crate) fn parse_numstat(bytes: &[u8]) -> Vec<NumstatRow> {
    let mut parts: Vec<&[u8]> = bytes.split(|b| *b == 0).collect();
    // Empty after the final NUL, or a record cut short: dropped either way.
    parts.pop();
    parts
        .into_iter()
        .filter_map(|record| {
            let mut parts = record.splitn(3, |b| *b == b'\t');
            let added = parts.next()?;
            let removed = parts.next()?;
            let path = parts.next()?;
            let count = |n: &[u8]| {
                std::str::from_utf8(n)
                    .ok()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            };
            Some((path.to_vec(), count(added), count(removed)))
        })
        .collect()
}

/// A numstat row: the raw path, lines added and lines removed.
pub(crate) type NumstatRow = (Vec<u8>, u32, u32);

/// One commit of `git log -z --format=%H%x00%h%x00%an%x00%ct%x00%s`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoggedCommit {
    pub(crate) sha: String,
    pub(crate) short: String,
    pub(crate) author: String,
    pub(crate) at: i64,
    pub(crate) subject: String,
}

/// The `--format` [`parse_log`] reads.
pub(crate) const LOG_FORMAT: &str = "--format=%H%x00%h%x00%an%x00%ct%x00%s";

/// Parses [`LOG_FORMAT`] output with `-z`: five NUL-separated fields per
/// commit, commits separated by NUL. Names and subjects that are not UTF-8
/// are decoded lossily.
pub(crate) fn parse_log(out: &Output) -> Vec<LoggedCommit> {
    let fields: Vec<&[u8]> = records(out).collect();
    fields
        .as_chunks::<5>()
        .0
        .iter()
        .filter_map(|c| {
            let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
            Some(LoggedCommit {
                sha: utf8(c[0])?,
                short: utf8(c[1])?,
                author: text(c[2]),
                at: std::str::from_utf8(c[3]).ok()?.parse().ok()?,
                subject: text(c[4]),
            })
        })
        .collect()
}

/// A hunk header's `start[,count]`, with count 1 when left out.
fn range(text: &str) -> Option<(u32, u32)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// `@@ -a,b +c,d @@ ...` as `((a, b), (c, d))`.
fn hunk_header(line: &[u8]) -> Option<((u32, u32), (u32, u32))> {
    let text = std::str::from_utf8(line.strip_prefix(b"@@ -")?).ok()?;
    let (old, rest) = text.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    Some((range(old)?, range(new)?))
}

/// The hunks of one file's unified diff, read by the counts in each
/// header, so a content line that looks like a header stays content.
pub(crate) fn parse_hunks(diff: &[u8]) -> Vec<HunkEntry> {
    let mut hunks = Vec::new();
    let mut lines = diff.split(|b| *b == b'\n').peekable();
    while let Some(line) = lines.next() {
        let Some(((old_start, mut old), (new_start, mut new))) = hunk_header(line) else {
            continue;
        };
        let mut hunk = HunkEntry {
            old_start,
            new_start,
            lines: Vec::new(),
        };
        while old > 0 || new > 0 {
            let Some(line) = lines.next() else { break };
            let text = || String::from_utf8_lossy(line.get(1..).unwrap_or_default()).into_owned();
            let kind = match line.first() {
                Some(b' ') => {
                    old = old.saturating_sub(1);
                    new = new.saturating_sub(1);
                    DiffLineKindEntry::Context
                }
                Some(b'-') => {
                    old = old.saturating_sub(1);
                    DiffLineKindEntry::Removed
                }
                Some(b'+') => {
                    new = new.saturating_sub(1);
                    DiffLineKindEntry::Added
                }
                // "\ No newline at end of file" counts for neither side.
                Some(b'\\') => continue,
                _ => break,
            };
            hunk.lines.push(DiffLineEntry { kind, text: text() });
        }
        // The marker can follow a hunk's last line too.
        if lines.peek().is_some_and(|l| l.first() == Some(&b'\\')) {
            lines.next();
        }
        hunks.push(hunk);
    }
    hunks
}

/// The gutter's marks from a `-U0` diff's hunk headers.
pub(crate) fn parse_marks(diff: &[u8]) -> Vec<MarkEntry> {
    diff.split(|b| *b == b'\n')
        .filter_map(hunk_header)
        .map(|((_, old), (start, new))| {
            let to = start.saturating_add(new);
            if new == 0 {
                MarkEntry::Deleted { after: start }
            } else if old == 0 {
                MarkEntry::Added { from: start, to }
            } else {
                MarkEntry::Modified { from: start, to }
            }
        })
        .collect()
}

/// `git diff --numstat --patch -z` output as its numstat records and its
/// patch, which git separates with an empty record. One run gives both,
/// so they describe the same state of the worktree.
pub(crate) fn split_numstat_patch(bytes: &[u8]) -> (Vec<NumstatRow>, &[u8]) {
    match bytes.windows(2).position(|w| w == b"\0\0") {
        Some(at) => (
            parse_numstat(bytes.get(..=at).unwrap_or_default()),
            bytes.get(at + 2..).unwrap_or_default(),
        ),
        // No patch yet: the output was cut inside the numstat part.
        None => (parse_numstat(bytes), &[]),
    }
}

/// A C-quoted name as git writes one (`"a/q\"t\303\251"`): its bytes, and
/// the rest of the line after the closing quote.
fn unquote(text: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let mut out = Vec::new();
    let mut i = 1;
    while let Some(&b) = text.get(i) {
        match b {
            b'"' => return Some((out, text.get(i + 1..).unwrap_or_default())),
            b'\\' => {
                let next = *text.get(i + 1)?;
                i += 2;
                out.push(match next {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 0x0b,
                    b'f' => 0x0c,
                    b'r' => b'\r',
                    b'0'..=b'7' => {
                        let digits = text.get(i - 1..i + 2)?;
                        i += 2;
                        let octal = std::str::from_utf8(digits).ok()?;
                        u8::from_str_radix(octal, 8).ok()?
                    }
                    other => other,
                });
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    None
}

/// The path a `diff --git a/P b/P` header names, as raw bytes. With
/// `--no-renames` both sides name the same path; a quoted header is
/// unquoted. The prefixes are fixed with `--src-prefix` and `--dst-prefix`.
fn section_path(section: &[u8]) -> Option<Vec<u8>> {
    let header = section.split(|b| *b == b'\n').next()?;
    let rest = header.strip_prefix(b"diff --git ")?;
    if rest.first() == Some(&b'"') {
        let (old, _) = unquote(rest)?;
        return old.strip_prefix(b"a/").map(<[u8]>::to_vec);
    }
    // "a/P b/P": both halves the same length.
    let len = rest.len().checked_sub(5)? / 2;
    let old = rest.get(2..2 + len)?;
    let new = rest.get(len + 5..)?;
    (rest.get(..2)? == b"a/" && rest.get(2 + len..len + 5)? == b" b/" && old == new)
        .then(|| old.to_vec())
}

/// A patch's sections by the path each one's own header names, so a file
/// set that differs from numstat's (a submodule shown as a log, or a file
/// git lists in one and not the other) cannot shift hunks onto another
/// file. A section whose header cannot be read is left out.
pub(crate) fn sections_by_path(
    patch: &[u8],
    truncated: bool,
) -> std::collections::HashMap<Vec<u8>, &[u8]> {
    split_patch(patch, truncated)
        .into_iter()
        .filter_map(|section| Some((section_path(section)?, section)))
        .collect()
}

/// A patch split at each `diff --git` header, one section per file in the
/// order git printed them. A content line always starts with a space, `+`
/// or `-`, so only a header starts with `diff --git `. When `truncated`,
/// the last section may be partial and is dropped.
fn split_patch(patch: &[u8], truncated: bool) -> Vec<&[u8]> {
    const HEADER: &[u8] = b"diff --git ";
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0;
    for line in patch.split(|b| *b == b'\n') {
        if line.starts_with(HEADER) {
            starts.push(at);
        }
        at += line.len() + 1;
    }
    let mut sections: Vec<&[u8]> = starts
        .iter()
        .enumerate()
        .map(|(i, start)| {
            let end = starts.get(i + 1).copied().unwrap_or(patch.len());
            patch.get(*start..end).unwrap_or_default()
        })
        .collect();
    if truncated {
        sections.pop();
    }
    sections
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
