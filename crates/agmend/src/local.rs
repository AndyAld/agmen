//! Task 8: what agmend reads from the person's own disk for the desktop:
//! role files (spec 3.5), the worktrees of their checkout, git facts, and
//! one file's text.
//!
//! agmend knows a project by name only, so the desktop names the checkout
//! it found (`root`). agmend reads it only when agmen-client's
//! `find_identity` trusts its `.agmen/identity.json` and that file names
//! the project; any other root reads as no checkout.
//!
//! `file_text` reads only inside a worktree agmend lists for that root,
//! and only a path that stays inside it once symlinks are followed (ADR
//! 0011, review C297): a committed `notes.md` that links to
//! `~/.ssh/id_ed25519` is refused.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use agmen_local::console::{
    ChangeEntry, CommitEntry, FileTextEntry, GitFactsEntry, HeadEntry, MarkEntry, Me, RoleFileInfo,
    RoleLayer, WorktreeEntry,
};
use agmen_local::roles;
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{GitSha, ProjectId, Timestamp};
use tokio::time::Instant;

use crate::git::{self, Git, ListedWorktree};

/// How long all the git work of one snapshot may take; worktrees not read
/// by then are left out.
const LOCAL_BUDGET: Duration = Duration::from_secs(10);
/// The brief's cap on commits per worktree.
const MAX_COMMITS: usize = 20;
/// The largest `file_text` answer (the brief's cap).
pub(crate) const MAX_TEXT: usize = 512 * 1024;
// shortcut: fixed caps that keep a snapshot's local data near 8 MiB of the
// 16 MiB frame, so chat still fits; revisit if large monorepos need more
// files, or move file lists to their own paged method.
/// The most worktrees one snapshot lists.
const MAX_WORKTREES: usize = 64;
/// The most tracked files listed per worktree.
const MAX_FILES: usize = 5000;
/// The bytes of file lists one snapshot carries across all worktrees.
const FILES_BUDGET: usize = 4 * 1024 * 1024;
/// The most changed files listed per worktree.
const MAX_CHANGES: usize = 200;
/// The bytes of `git status` and `--numstat` output read per worktree.
const STATUS_BYTES: usize = 1024 * 1024;
/// The bytes of one file's diff kept as hunks; a longer one has none.
const FILE_DIFF_BYTES: usize = 64 * 1024;
/// The bytes of patch one worktree reads.
const PATCH_BYTES: usize = 1024 * 1024;
/// The bytes of patch one snapshot reads across all worktrees.
const DIFF_BUDGET: usize = 2 * 1024 * 1024;
/// The most paths in the git facts.
const MAX_FACT_FILES: usize = 20_000;
/// The most local branches in the git facts.
const MAX_BRANCHES: usize = 1000;
/// The most role files read from one layer.
const MAX_ROLE_FILES: usize = 64;
/// The largest role file read; a real one is a few KiB.
const MAX_ROLE_BYTES: u64 = 64 * 1024;

/// What the disk reads need to know.
#[derive(Debug)]
pub(crate) struct Local {
    /// `~/.config/agmen/roles`, or `None` to read no personal roles.
    pub(crate) personal_roles: Option<PathBuf>,
}

/// The worktrees and git facts of one checkout.
#[derive(Debug, Default)]
pub(crate) struct CheckoutData {
    pub(crate) worktrees: Vec<WorktreeEntry>,
    pub(crate) git: GitFactsEntry,
}

fn not_found(message: impl Into<String>) -> ApiError {
    ApiError {
        code: ErrorCode::NotFound,
        message: message.into(),
    }
}

fn refused(message: impl Into<String>) -> ApiError {
    ApiError {
        code: ErrorCode::BadRequest,
        message: message.into(),
    }
}

/// Runs blocking disk work off the runtime's workers.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| ApiError {
            code: ErrorCode::Hub,
            message: format!("agmend's disk read stopped: {err}"),
        })
}

/// The checkout root `root` names, when its trusted identity file is of
/// `project`: git's own spelling of the worktree root.
pub(crate) async fn checkout(project: &ProjectId, root: &Path) -> Option<PathBuf> {
    let root = root.to_owned();
    let found = blocking(move || agmen_client::find_identity(&root)).await;
    match found {
        Ok(Ok(Some(joined))) if joined.identity.project == *project => Some(joined.root),
        Ok(Ok(_)) => None,
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "the checkout's identity file is not usable");
            None
        }
        Err(err) => {
            tracing::debug!(error = %err.message, "reading the checkout's identity failed");
            None
        }
    }
}

impl Local {
    /// The built-in, project and personal role files, in that order; the
    /// project layer only for a trusted `checkout`.
    pub(crate) async fn roles(&self, checkout: Option<&Path>) -> Vec<RoleFileInfo> {
        let mut files: Vec<RoleFileInfo> = roles::BUILT_IN
            .iter()
            .map(|(name, text)| RoleFileInfo {
                name: (*name).to_owned(),
                layer: RoleLayer::BuiltIn,
                text: (*text).to_owned(),
            })
            .collect();
        let project = checkout.map(Path::to_owned);
        let personal = self.personal_roles.clone();
        let read = blocking(move || {
            let mut more = Vec::new();
            if let Some(root) = project {
                more.extend(project_roles(&root));
            }
            if let Some(dir) = personal {
                more.extend(role_dir(&dir, RoleLayer::Personal, true));
            }
            more
        })
        .await;
        files.extend(read.unwrap_or_default());
        files
    }
}

/// The checkout's `.agmen/roles/*.xml`. They come with the repository, so
/// a link in them could point anywhere: the folder must stay inside the
/// checkout once links are followed, and only regular files are read.
fn project_roles(root: &Path) -> Vec<RoleFileInfo> {
    let dir = root.join(".agmen").join("roles");
    let inside = match (std::fs::canonicalize(&dir), std::fs::canonicalize(root)) {
        (Ok(dir), Ok(root)) => dir.starts_with(root),
        _ => false,
    };
    if inside {
        role_dir(&dir, RoleLayer::Project, false)
    } else {
        Vec::new()
    }
}

/// The `*.xml` role files in `dir`, by file name, at most
/// [`MAX_ROLE_FILES`]. A file that is too large, not UTF-8, or (unless
/// `follow`) a link is skipped; a missing folder is no files.
fn role_dir(dir: &Path, layer: RoleLayer, follow: bool) -> Vec<RoleFileInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension() == Some(OsStr::new("xml")))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .take(MAX_ROLE_FILES)
        .filter_map(|path| {
            let meta = if follow {
                std::fs::metadata(&path)
            } else {
                std::fs::symlink_metadata(&path)
            };
            if !meta.is_ok_and(|m| m.is_file()) {
                return None;
            }
            let mut text = String::new();
            File::open(&path)
                .ok()?
                .take(MAX_ROLE_BYTES + 1)
                .read_to_string(&mut text)
                .ok()?;
            if text.len() as u64 > MAX_ROLE_BYTES {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_owned();
            let name = roles::parse(&text).map_or(stem, |spec| spec.name);
            Some(RoleFileInfo { name, layer, text })
        })
        .collect()
}

/// How much of the snapshot's byte budgets is left.
#[derive(Debug)]
struct Budget {
    files: usize,
    diff: usize,
}

/// The worktrees `git worktree list` gives for `root`, without bare or
/// prunable ones.
async fn listed(root: &Path, deadline: Instant) -> Vec<ListedWorktree> {
    let git = Git::new(root, deadline);
    match git
        .run_str(&["worktree", "list", "--porcelain", "-z"], STATUS_BYTES)
        .await
    {
        Ok(out) => git::parse_worktrees(&out)
            .into_iter()
            .filter(|w| !w.skip)
            .take(MAX_WORKTREES)
            .collect(),
        Err(err) => {
            tracing::debug!(error = %err, "listing worktrees failed");
            Vec::new()
        }
    }
}

/// Every worktree of the checkout at `root` with its changes and commits,
/// and the git facts across them.
pub(crate) async fn checkout_data(root: &Path) -> CheckoutData {
    let deadline = Instant::now() + LOCAL_BUDGET;
    let mut budget = Budget {
        files: FILES_BUDGET,
        diff: DIFF_BUDGET,
    };
    let mut worktrees = Vec::new();
    for wt in listed(root, deadline).await {
        if Instant::now() >= deadline {
            break;
        }
        if let Some(entry) = worktree(wt, &mut budget, deadline).await {
            worktrees.push(entry);
        }
    }
    let branches = branches(root, deadline).await;
    let commits: BTreeSet<String> = worktrees
        .iter()
        .flat_map(|w| &w.commits)
        .map(|c| c.sha.to_string())
        .collect();
    let files: BTreeSet<String> = worktrees
        .iter()
        .flat_map(|w| &w.files)
        .take(MAX_FACT_FILES)
        .cloned()
        .collect();
    CheckoutData {
        worktrees,
        git: GitFactsEntry {
            branches,
            commits: commits.into_iter().collect(),
            files: files.into_iter().collect(),
        },
    }
}

async fn branches(root: &Path, deadline: Instant) -> Vec<String> {
    let git = Git::new(root, deadline);
    let Ok(out) = git
        .run_str(
            &["for-each-ref", "--format=%(refname)", "refs/heads/"],
            256 * 1024,
        )
        .await
    else {
        return Vec::new();
    };
    // Ref names hold no newline, so lines are safe here; a cut last line
    // is dropped.
    let mut lines: Vec<&[u8]> = out.stdout.split(|b| *b == b'\n').collect();
    lines.pop();
    let mut names: Vec<String> = lines
        .into_iter()
        .filter_map(git::utf8)
        .filter_map(|r| r.strip_prefix("refs/heads/").map(str::to_owned))
        .take(MAX_BRANCHES)
        .collect();
    names.sort();
    names
}

/// The worktree's head from its list entry.
fn head(wt: &ListedWorktree) -> Option<HeadEntry> {
    let unborn = wt
        .head
        .as_deref()
        .is_none_or(|sha| sha.bytes().all(|b| b == b'0'));
    match &wt.branch {
        Some(full) => {
            let name = full.strip_prefix("refs/heads/").unwrap_or(full).to_owned();
            Some(if unborn {
                HeadEntry::Unborn { name }
            } else {
                HeadEntry::Branch { name }
            })
        }
        None => Some(HeadEntry::Detached {
            sha: wt.head.as_deref()?.parse().ok()?,
        }),
    }
}

/// The trusted identity of the worktree at `path`, if it has one of its own.
async fn identity_of(path: &Path) -> Option<Me> {
    let at = path.to_owned();
    let joined = blocking(move || agmen_client::find_identity(&at))
        .await
        .ok()?
        .ok()??;
    (joined.root == path).then_some(Me {
        id: joined.identity.identity_id,
        handle: joined.identity.handle,
    })
}

async fn worktree(
    wt: ListedWorktree,
    budget: &mut Budget,
    deadline: Instant,
) -> Option<WorktreeEntry> {
    // A folder the JSON cannot name is skipped, as paths are.
    let path = wt.path.to_str()?.to_owned();
    let head = head(&wt)?;
    let git = Git::new(&wt.path, deadline);
    let status = git
        .run_str(
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=all",
                "--no-renames",
            ],
            STATUS_BYTES,
        )
        .await
        .ok();
    let mut truncated = status.as_ref().is_some_and(|out| out.truncated);
    let status = status
        .map(|out| git::parse_status(&out))
        .unwrap_or_default();
    let (changes, cut) = changes(&git, status.entries, budget).await;
    truncated |= cut;
    let commits = commits(&git).await;
    let (files, cut) = files(&git, budget).await;
    truncated |= cut;
    Some(WorktreeEntry {
        identity: identity_of(&wt.path).await,
        path,
        head,
        upstream: status.upstream,
        files,
        changes,
        commits,
        truncated,
    })
}

/// The worktree's changes against `HEAD` in path order, with counts and,
/// within the byte budgets, hunks; and whether the list was cut.
async fn changes(
    git: &Git,
    entries: Vec<git::StatusEntry>,
    budget: &mut Budget,
) -> (Vec<ChangeEntry>, bool) {
    let cut = entries.len() > MAX_CHANGES;
    let diff_args = [
        "diff",
        "HEAD",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
    ];
    let numstat = git
        .run_str(
            &[&diff_args[..], &["--numstat", "-z"]].concat(),
            STATUS_BYTES,
        )
        .await
        .map(|out| git::parse_numstat(&out))
        .unwrap_or_default();
    let cap = PATCH_BYTES.min(budget.diff);
    let patch = if cap == 0 || numstat.is_empty() {
        None
    } else {
        git.run_str(&[&diff_args[..], &["--no-color", "-U3"]].concat(), cap)
            .await
            .ok()
    };
    let mut hunks = std::collections::HashMap::new();
    if let Some(patch) = &patch {
        budget.diff = budget.diff.saturating_sub(patch.stdout.len());
        let sections = git::split_patch(&patch.stdout, patch.truncated);
        // The patch lists files in numstat's order; anything else and the
        // pairing is unsafe, so no hunks.
        if sections.len() <= numstat.len() {
            for ((path, _, _), section) in numstat.iter().zip(sections) {
                if section.len() <= FILE_DIFF_BYTES {
                    hunks.insert(path.clone(), git::parse_hunks(section));
                }
            }
        }
    }
    let counts: std::collections::HashMap<&[u8], (u32, u32)> = numstat
        .iter()
        .map(|(path, added, removed)| (path.as_slice(), (*added, *removed)))
        .collect();
    let mut changes: Vec<ChangeEntry> = entries
        .into_iter()
        .take(MAX_CHANGES)
        .map(|entry| {
            let key = entry.path.as_bytes();
            let (added, removed) = counts.get(key).copied().unwrap_or((0, 0));
            ChangeEntry {
                hunks: hunks.remove(key).unwrap_or_default(),
                path: entry.path,
                kind: entry.kind,
                added,
                removed,
            }
        })
        .collect();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    (changes, cut)
}

/// The newest [`MAX_COMMITS`] commits on `HEAD`; none on an unborn branch.
async fn commits(git: &Git) -> Vec<CommitEntry> {
    let count = format!("-n{MAX_COMMITS}");
    let Ok(out) = git
        .run_str(&["log", &count, "-z", git::LOG_FORMAT, "HEAD"], 256 * 1024)
        .await
    else {
        return Vec::new();
    };
    git::parse_log(&out)
        .into_iter()
        .filter_map(|c| {
            Some(CommitEntry {
                sha: c.sha.parse::<GitSha>().ok()?,
                short: c.short.parse::<GitSha>().ok()?,
                subject: c.subject,
                author: c.author,
                at: Timestamp::from_second(c.at).ok()?,
            })
        })
        .collect()
}

/// The tracked files in path order, within [`MAX_FILES`] and the budget,
/// and whether the list was cut.
async fn files(git: &Git, budget: &mut Budget) -> (Vec<String>, bool) {
    if budget.files == 0 {
        return (Vec::new(), true);
    }
    let Ok(out) = git.run_str(&["ls-files", "-z"], budget.files).await else {
        return (Vec::new(), false);
    };
    budget.files = budget.files.saturating_sub(out.stdout.len());
    let all: Vec<&[u8]> = git::records(&out).collect();
    let cut = out.truncated || all.len() > MAX_FILES;
    let files = all
        .into_iter()
        .take(MAX_FILES)
        .filter_map(git::utf8)
        .collect();
    (files, cut)
}

/// `file_text {project, root, worktree, path}`: the text of `path` in a
/// worktree agmend lists for the checkout at `root`, and its marks.
pub(crate) async fn file_text(
    project: &ProjectId,
    root: &Path,
    worktree: &Path,
    path: &str,
) -> Result<FileTextEntry, ApiError> {
    let Some(root) = checkout(project, root).await else {
        return Err(not_found(format!(
            "{} is not a checkout of {project} that agmen trusts",
            root.display()
        )));
    };
    let deadline = Instant::now() + LOCAL_BUDGET;
    let wanted = worktree.to_owned();
    let listed: Vec<PathBuf> = listed(&root, deadline)
        .await
        .into_iter()
        .map(|w| w.path)
        .collect();
    let matched = blocking(move || {
        let want = std::fs::canonicalize(&wanted).ok()?;
        listed
            .into_iter()
            .find(|p| std::fs::canonicalize(p).ok().as_ref() == Some(&want))
    })
    .await?;
    let Some(worktree) = matched else {
        return Err(not_found(format!(
            "{} is not a worktree of this checkout",
            worktree.display()
        )));
    };
    let (rel, text, truncated) = {
        let (worktree, path) = (worktree.clone(), path.to_owned());
        blocking(move || read_inside(&worktree, &path)).await??
    };
    let marks = marks(&Git::new(&worktree, deadline), &rel, &text).await;
    Ok(FileTextEntry {
        text,
        marks,
        truncated,
    })
}

/// Whether a path part names git's own folder, in any case (a
/// case-insensitive file system opens `.GIT` as `.git`).
fn is_git_dir(part: &OsStr) -> bool {
    part.to_str()
        .is_some_and(|p| p.eq_ignore_ascii_case(".git"))
}

/// Reads `path` inside `worktree`: its path from the root once links are
/// followed, at most [`MAX_TEXT`] bytes of its text, and whether it was
/// cut. Refuses a path that leaves the root, before or after following
/// links, or that reaches into `.git`.
fn read_inside(worktree: &Path, path: &str) -> Result<(PathBuf, String, bool), ApiError> {
    let given = Path::new(path);
    let plain = !path.is_empty()
        && given.components().all(|part| match part {
            Component::Normal(name) => !is_git_dir(name),
            Component::CurDir => true,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => false,
        });
    if !plain {
        return Err(refused(format!(
            "{path:?} is not a path inside the worktree (no .., no leading /, nothing in .git)"
        )));
    }
    let root = std::fs::canonicalize(worktree)
        .map_err(|err| not_found(format!("the worktree cannot be read: {err}")))?;
    let full = std::fs::canonicalize(worktree.join(given))
        .map_err(|err| not_found(format!("{path} cannot be read: {err}")))?;
    let rel = match full.strip_prefix(&root) {
        Ok(rel) if !rel.components().any(|p| is_git_dir(p.as_os_str())) => rel.to_owned(),
        _ => {
            return Err(refused(format!(
                "{path} leads outside the worktree through a link, so agmend will not read it"
            )));
        }
    };
    // Checked before opening too: opening a named pipe would block.
    let is_file = |meta: std::io::Result<std::fs::Metadata>| meta.is_ok_and(|m| m.is_file());
    if !is_file(std::fs::metadata(&full)) {
        return Err(refused(format!("{path} is not a file")));
    }
    let mut file =
        File::open(&full).map_err(|err| not_found(format!("{path} cannot be read: {err}")))?;
    if !is_file(file.metadata()) {
        return Err(refused(format!("{path} is not a file")));
    }
    let mut bytes = Vec::new();
    let limit = u64::try_from(MAX_TEXT).unwrap_or(u64::MAX) + 1;
    file.by_ref()
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|err| not_found(format!("{path} cannot be read: {err}")))?;
    let truncated = bytes.len() > MAX_TEXT;
    bytes.truncate(MAX_TEXT);
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        // Cut inside a character: keep the whole ones.
        Err(err) if truncated && err.utf8_error().error_len().is_none() => {
            let valid = err.utf8_error().valid_up_to();
            let mut bytes = err.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).map_err(|_| refused(format!("{path} is not UTF-8 text")))?
        }
        Err(_) => return Err(refused(format!("{path} is not UTF-8 text"))),
    };
    Ok((rel, text, truncated))
}

/// The gutter marks of `rel` against `HEAD`; every line is new in an
/// untracked file.
async fn marks(git: &Git, rel: &Path, text: &str) -> Vec<MarkEntry> {
    let rel = rel.as_os_str();
    let args: Vec<&OsStr> = [
        "diff",
        "HEAD",
        "-U0",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--",
    ]
    .iter()
    .map(OsStr::new)
    .chain([rel])
    .collect();
    if let Ok(out) = git.run(&args, FILE_DIFF_BYTES * 4).await {
        let marks = git::parse_marks(&out.stdout);
        if !marks.is_empty() {
            return marks;
        }
    }
    let args: Vec<&OsStr> = [
        "status",
        "--porcelain=v2",
        "-z",
        "--untracked-files=all",
        "--",
    ]
    .iter()
    .map(OsStr::new)
    .chain([rel])
    .collect();
    let untracked = git
        .run(&args, STATUS_BYTES)
        .await
        .is_ok_and(|out| git::parse_status(&out).entries.iter().any(|e| e.untracked));
    let lines = u32::try_from(text.lines().count()).unwrap_or(u32::MAX);
    if untracked && lines > 0 {
        vec![MarkEntry::Added {
            from: 1,
            to: lines.saturating_add(1),
        }]
    } else {
        Vec::new()
    }
}
