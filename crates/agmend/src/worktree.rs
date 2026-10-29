//! Private launcher worktree operations, using bounded T1 git calls.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio::time::Instant;

use crate::git::{self, Git, GitError, Output};

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_BYTES: usize = 1024 * 1024;
const SLUG_CHARS: usize = 40;

/// How the branch's changes reached the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Merged {
    Ancestor,
    Cherry,
    No,
}

async fn call(dir: &Path, args: &[&OsStr], index: Option<&Path>) -> Result<Output> {
    #[cfg(test)]
    COMMANDS.with_borrow_mut(|log| {
        assert!(log.len() < 4096, "test command log is full");
        log.push(args.iter().map(|arg| (*arg).to_owned()).collect());
    });
    let out = Git::new(dir, Instant::now() + CALL_TIMEOUT)
        .run_with_options(args, OUTPUT_BYTES, CALL_TIMEOUT, index)
        .await?;
    if out.truncated {
        bail!("git output exceeded {OUTPUT_BYTES} bytes; refusing incomplete worktree data");
    }
    #[cfg(test)]
    if args.starts_with(&[OsStr::new("worktree"), OsStr::new("add")]) {
        let hook = AFTER_ADD.with_borrow_mut(Option::take);
        if let Some(hook) = hook {
            hook();
        }
    }
    Ok(out)
}

async fn branch_ref(repo: &Path, branch: &str) -> Result<String> {
    // Validate a literal ref, not --branch's @{-1} shorthand or an option.
    let name = format!("refs/heads/{branch}");
    if branch.is_empty() || branch.starts_with('-') {
        bail!("invalid branch name");
    }
    call(repo, &["check-ref-format".as_ref(), name.as_ref()], None).await?;
    Ok(name)
}

async fn commit(repo: &Path, revision: &str) -> Result<String> {
    let revision = format!("{revision}^{{commit}}");
    let out = call(
        repo,
        &[
            "rev-parse".as_ref(),
            "--verify".as_ref(),
            "--end-of-options".as_ref(),
            revision.as_ref(),
        ],
        None,
    )
    .await?;
    object_id(&out)
}

fn object_id(out: &Output) -> Result<String> {
    let oid = std::str::from_utf8(&out.stdout)
        .context("git object ID is not UTF-8")?
        .trim();
    if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("git returned an invalid object ID");
    }
    Ok(oid.to_owned())
}

async fn refuse_default_branch(repo: &Path, branch: &str) -> Result<()> {
    // A remote's default branch may have no local ref yet, so -b alone
    // does not protect it. Existing local branches are still refused by git.
    let defaults = call(
        repo,
        &[
            "for-each-ref".as_ref(),
            "--format=%(refname)%00%(symref)".as_ref(),
            "refs/remotes/".as_ref(),
        ],
        None,
    )
    .await?;
    // The output cap bounds the number of remote refs examined.
    for line in std::str::from_utf8(&defaults.stdout)
        .context("reading remote default branches")?
        .lines()
    {
        if let Some((name, target)) = line.split_once('\0')
            && let Some(remote) = name.strip_suffix("/HEAD")
            && target == format!("{remote}/{branch}")
        {
            bail!("refusing worker operation on the default branch");
        }
    }
    Ok(())
}

/// The main checkout defines the worker root even when called from a worker.
pub(crate) async fn worktree_root(repo: &Path) -> Result<(PathBuf, Vec<git::ListedWorktree>)> {
    let out = call(
        repo,
        &[
            "worktree".as_ref(),
            "list".as_ref(),
            "--porcelain".as_ref(),
            "-z".as_ref(),
        ],
        None,
    )
    .await?;
    let mut trees = git::parse_worktrees(&out);
    let main = trees
        .first_mut()
        .context("repository has no main worktree")?;
    if main.skip {
        bail!("repository has no usable main worktree");
    }
    main.path = std::fs::canonicalize(&main.path).context("resolving main worktree")?;
    Ok((main.path.join(".worktrees"), trees))
}

/// Resolve existing ancestors too, so symlinks cannot send a new checkout outside.
fn contained_path(root: &Path, path: &Path) -> Result<PathBuf> {
    if path
        .components()
        .any(|part| part == std::path::Component::ParentDir)
    {
        bail!("worker path contains a parent traversal");
    }
    // Each iteration removes a component, bounded by the input path length.
    for ancestor in path.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(parent) => {
                let path = parent.join(path.strip_prefix(ancestor)?);
                if path == root || !path.starts_with(root) {
                    bail!("worker path is outside the main worktree's .worktrees root");
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("resolving worker path"),
        }
    }
    bail!("worker path has no existing ancestor")
}

/// Creates a new branch, never resets an existing one, including the default.
pub(crate) async fn create(repo: &Path, base: &str, branch: &str) -> Result<PathBuf> {
    branch_ref(repo, branch).await?;
    refuse_default_branch(repo, branch).await?;
    let (root, _) = worktree_root(repo).await?;
    let base = commit(repo, base).await?;
    let path = reserve_leaf(&root, &root.join(branch))?;
    add_worktree(
        repo,
        &[
            "worktree".as_ref(),
            "add".as_ref(),
            "-b".as_ref(),
            branch.as_ref(),
            path.as_os_str(),
            base.as_ref(),
        ],
        &path,
    )
    .await?;
    check_added_worktree(repo, &root, &path).await?;
    Ok(path)
}

/// Creates the Reviewer's checkout at a commit without attaching a branch.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "detached listener placement is deferred")
)]
pub(crate) async fn create_detached(repo: &Path, base: &str, path: &Path) -> Result<PathBuf> {
    let (root, trees) = worktree_root(repo).await?;
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        trees[0].path.join(path)
    };
    let base = commit(repo, base).await?;
    let path = reserve_leaf(&root, &path)?;
    add_worktree(
        repo,
        &[
            "worktree".as_ref(),
            "add".as_ref(),
            "--detach".as_ref(),
            path.as_os_str(),
            base.as_ref(),
        ],
        &path,
    )
    .await?;
    check_added_worktree(repo, &root, &path).await?;
    Ok(path)
}

/// Reserves a new leaf under the root, which may be nested (`fix/b-1-slug`).
/// Every existing component from the root to the leaf's parent must be a real
/// directory, never a symlink; missing ones are created one at a time, and the
/// leaf itself with `create_dir`, which fails if anything exists there.
fn reserve_leaf(root: &Path, path: &Path) -> Result<PathBuf> {
    const OUTSIDE: &str = "worker path is outside the main worktree's .worktrees root";
    let main = root.parent().context("worker root has no parent")?;
    if path
        .components()
        .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        bail!("worker path must be absolute without . or .. components");
    }
    // The shallowest ancestor that is the main worktree: symlinks above it
    // (such as /var on macOS) are allowed, and nothing below it is followed.
    // One canonicalize per component bounds this scan.
    let ancestors: Vec<&Path> = path.ancestors().collect();
    let start = ancestors
        .into_iter()
        .rev()
        .find(|ancestor| std::fs::canonicalize(ancestor).is_ok_and(|real| real == main))
        .context(OUTSIDE)?;
    let mut parts = path.strip_prefix(start)?.components();
    if parts.next() != Some(Component::Normal(OsStr::new(".worktrees"))) {
        bail!(OUTSIDE);
    }
    let names: Vec<&OsStr> = parts.map(Component::as_os_str).collect();
    let Some((leaf, parents)) = names.split_last() else {
        bail!(OUTSIDE);
    };
    let mut current = root.to_path_buf();
    real_dir(&current)?;
    for name in parents {
        current.push(name);
        real_dir(&current)?;
    }
    current.push(leaf);
    std::fs::create_dir(&current).context("reserving worker leaf directory")?;
    let resolved =
        std::fs::canonicalize(&current).context("resolving reserved worker directory")?;
    if resolved != current {
        bail!("{OUTSIDE}: reserved directory resolved elsewhere");
    }
    Ok(current)
}

/// Accepts an existing real directory, or creates a missing one; refuses a
/// symlink or any other entry without following it.
fn real_dir(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(()),
        Ok(_) => bail!("worker path component is not a real directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).context("creating worker parent directory")
        }
        Err(error) => Err(error).context("reading worker path component"),
    }
}

/// Runs `git worktree add`; on failure, removes the leaf reserved for it.
/// `remove_dir` only removes an empty directory, so it cannot delete work.
async fn add_worktree(repo: &Path, args: &[&OsStr], path: &Path) -> Result<()> {
    if let Err(error) = call(repo, args, None).await {
        if let Err(cleanup) = std::fs::remove_dir(path)
            && cleanup.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.context(format!("leaving reserved worker directory: {cleanup}")));
        }
        return Err(error);
    }
    Ok(())
}

async fn check_added_worktree(repo: &Path, root: &Path, path: &Path) -> Result<()> {
    let checked = std::fs::canonicalize(path)
        .context("resolving added worker worktree")
        .and_then(|resolved| contained_path(root, &resolved));
    if let Err(error) = checked {
        call(
            repo,
            &[
                "worktree".as_ref(),
                "remove".as_ref(),
                "--".as_ref(),
                path.as_os_str(),
            ],
            None,
        )
        .await
        .with_context(|| format!("{error:#}; removing escaped worktree failed"))?;
        return Err(error);
    }
    Ok(())
}

pub(crate) async fn merged_state(repo: &Path, branch: &str, base: &str) -> Result<Merged> {
    let branch = branch_ref(repo, branch).await?;
    let tip = commit(repo, &branch).await?;
    let base = commit(repo, base).await?;
    match call(
        repo,
        &[
            "merge-base".as_ref(),
            "--is-ancestor".as_ref(),
            tip.as_ref(),
            base.as_ref(),
        ],
        None,
    )
    .await
    {
        Ok(_) => return Ok(Merged::Ancestor),
        Err(error)
            if matches!(
                error.downcast_ref::<GitError>(),
                Some(GitError::Failed { .. })
            ) =>
        {
            // T1 does not expose exit codes. Confirm the negative ancestry
            // result independently so a fatal merge-base error is not hidden.
            let range = format!("{base}..{tip}");
            let out = call(
                repo,
                &[
                    "rev-list".as_ref(),
                    "--max-count=1".as_ref(),
                    range.as_ref(),
                ],
                None,
            )
            .await?;
            if out.stdout.is_empty() {
                return Err(error.context("merge-base failed despite ancestor relationship"));
            }
        }
        Err(error) => return Err(error),
    }
    let out = call(
        repo,
        &["cherry".as_ref(), base.as_ref(), tip.as_ref()],
        None,
    )
    .await?;
    let text = std::str::from_utf8(&out.stdout).context("reading git cherry output")?;
    let mut applied = false;
    // The output cap bounds both the number and length of these records.
    for line in text.lines() {
        if line.starts_with("+ ") {
            return Ok(Merged::No);
        }
        if !line.starts_with("- ") {
            bail!("git cherry returned an invalid record");
        }
        applied = true;
    }
    Ok(if applied { Merged::Cherry } else { Merged::No })
}

/// Verifies both repository membership and the branch before any mutation.
async fn checked_worktree(repo: &Path, path: &Path, branch: &str) -> Result<PathBuf> {
    let reference = branch_ref(repo, branch).await?;
    let (root, trees) = worktree_root(repo).await?;
    if std::fs::canonicalize(repo).context("resolving worktree repository")? != trees[0].path {
        bail!("repository must be the main worktree");
    }
    refuse_default_branch(repo, branch).await?;
    if trees[0].branch.as_deref() == Some(&reference) {
        bail!("refusing the main worktree's current branch");
    }
    let path = std::fs::canonicalize(path).context("resolving worker worktree")?;
    let path = contained_path(&root, &path)?;
    let found = trees.iter().enumerate().any(|(i, tree)| {
        i != 0
            && !tree.skip
            && tree.branch.as_deref() == Some(&reference)
            && std::fs::canonicalize(&tree.path).is_ok_and(|candidate| candidate == path)
    });
    if !found {
        bail!("path is not a linked worktree for the requested branch");
    }
    Ok(path)
}

/// Keeps dirty work with a WIP ref, and refuses branches unmerged into repo HEAD.
/// Requires the main checkout as repo because HEAD is the merge base.
/// Patch-equivalent tips also stay when safe branch deletion would refuse.
pub(crate) async fn remove(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    let path = checked_worktree(repo, path, branch).await?;
    let status = call(
        &path,
        &[
            "status".as_ref(),
            "--porcelain=v1".as_ref(),
            "-z".as_ref(),
            "--untracked-files=all".as_ref(),
        ],
        None,
    )
    .await?;
    if !status.stdout.is_empty() {
        save_wip(repo, &path, branch)
            .await
            .context("saving uncommitted work before keeping worktree")?;
        bail!("uncommitted work; saved refs/wip/{branch} and kept worktree");
    }
    match merged_state(repo, branch, "HEAD").await? {
        Merged::No => {
            save_wip(repo, &path, branch)
                .await
                .context("saving WIP before keeping unmerged branch")?;
            bail!("not merged; saved refs/wip/{branch} and kept worktree and branch");
        }
        Merged::Cherry => {
            bail!("patch-equivalent; git branch -d requires ancestry, kept worktree and branch");
        }
        Merged::Ancestor => {}
    }
    // A worker's own upstream does not prove a merge into the base. Check
    // it only after the base proof, to predict branch -d's additional refusal.
    let upstream = format!("{branch}@{{upstream}}");
    let target = match commit(repo, &upstream).await {
        Ok(oid) => oid,
        Err(error)
            if matches!(
                error.downcast_ref::<GitError>(),
                Some(GitError::Failed { .. })
            ) =>
        {
            commit(repo, "HEAD").await?
        }
        Err(error) => return Err(error),
    };
    if merged_state(repo, branch, &target).await? != Merged::Ancestor {
        bail!("not merged into configured upstream; kept worktree and branch");
    }
    call(
        repo,
        &[
            "worktree".as_ref(),
            "remove".as_ref(),
            "--".as_ref(),
            path.as_os_str(),
        ],
        None,
    )
    .await?;
    call(
        repo,
        &[
            "branch".as_ref(),
            "-d".as_ref(),
            "--".as_ref(),
            branch.as_ref(),
        ],
        None,
    )
    .await
    .context("worktree removed but safe branch deletion refused")?;
    Ok(())
}

/// Prunes one released agent worktree after checking the requested base.
/// Dirty work always stays. Exact confirmation preserves an unmerged tip in
/// WIP before removal. `None` means removed; `Some` explains why it stayed.
pub(crate) async fn prune(
    repo: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    confirm: Option<&str>,
) -> Result<Option<&'static str>> {
    if !branch.starts_with("agent/") {
        bail!("prune only removes agent/ branches");
    }
    let path = checked_worktree(repo, path, branch).await?;
    let reference = branch_ref(repo, branch).await?;
    let tip = commit(repo, &reference).await?;
    let status = call(
        &path,
        &[
            "status".as_ref(),
            "--porcelain=v1".as_ref(),
            "-z".as_ref(),
            "--untracked-files=all".as_ref(),
        ],
        None,
    )
    .await?;
    if !status.stdout.is_empty() {
        save_wip(repo, &path, branch)
            .await
            .context("saving uncommitted work before keeping worktree")?;
        return Ok(Some("uncommitted"));
    }
    match merged_state(repo, branch, base).await? {
        Merged::No if confirm != Some(branch) => return Ok(Some("not merged")),
        Merged::No | Merged::Cherry => {
            save_wip(repo, &path, branch)
                .await
                .context("preserving branch before pruning worktree")?;
        }
        Merged::Ancestor => {}
    }
    call(
        repo,
        &[
            "worktree".as_ref(),
            "remove".as_ref(),
            "--".as_ref(),
            path.as_os_str(),
        ],
        None,
    )
    .await?;
    // branch -d only accepts ancestry into HEAD/upstream. Prune also accepts
    // the requested base, patch equivalence and an explicitly confirmed WIP.
    // Compare the checked tip so a concurrently changed ref is never deleted.
    call(
        repo,
        &[
            "update-ref".as_ref(),
            "-d".as_ref(),
            reference.as_ref(),
            tip.as_ref(),
        ],
        None,
    )
    .await
    .context("worktree removed but branch changed; kept branch")?;
    Ok(None)
}

/// Snapshots working files, including untracked files, with a private index.
/// HEAD, the branch, the real index and working files stay untouched.
pub(crate) async fn save_wip(repo: &Path, path: &Path, branch: &str) -> Result<String> {
    let path = checked_worktree(repo, path, branch).await?;
    let head = commit(&path, "HEAD").await?;
    let wip = format!("refs/wip/{branch}");
    let previous = match call(
        repo,
        &[
            "rev-parse".as_ref(),
            "--verify".as_ref(),
            "--quiet".as_ref(),
            wip.as_ref(),
        ],
        None,
    )
    .await
    {
        Ok(out) => Some(object_id(&out)?),
        Err(error) if matches!(error.downcast_ref::<GitError>(), Some(GitError::Failed { stderr, .. }) if stderr.is_empty()) => {
            None
        }
        Err(error) => return Err(error).context("reading previous WIP ref"),
    };
    let temp = tempfile::tempdir().context("creating private WIP index directory")?;
    let index = temp.path().join("index");
    call(&path, &["read-tree".as_ref(), head.as_ref()], Some(&index)).await?;
    call(
        &path,
        &["add".as_ref(), "-A".as_ref(), "--".as_ref(), ".".as_ref()],
        Some(&index),
    )
    .await?;
    let tree = object_id(&call(&path, &["write-tree".as_ref()], Some(&index)).await?)?;
    if let Some(previous) = &previous {
        let head_tree = object_id(
            &call(
                &path,
                &[
                    "rev-parse".as_ref(),
                    "--verify".as_ref(),
                    "HEAD^{tree}".as_ref(),
                ],
                None,
            )
            .await?,
        )?;
        if tree == head_tree && merged_state(repo, branch, previous).await? == Merged::Ancestor {
            return Ok(previous.clone());
        }
    }
    let mut args: Vec<&OsStr> = vec![
        "-c".as_ref(),
        "commit.gpgsign=false".as_ref(),
        "commit-tree".as_ref(),
        tree.as_ref(),
        "-p".as_ref(),
        head.as_ref(),
    ];
    if let Some(previous) = &previous {
        args.push("-p".as_ref());
        args.push(previous.as_ref());
    }
    args.push("-m".as_ref());
    args.push("agmen WIP".as_ref());
    let oid = object_id(&call(&path, &args, Some(&index)).await?)?;
    let old = previous.unwrap_or_else(|| "0".repeat(head.len()));
    call(
        repo,
        &[
            "update-ref".as_ref(),
            wip.as_ref(),
            oid.as_ref(),
            old.as_ref(),
        ],
        Some(&index),
    )
    .await?;
    Ok(oid)
}

/// Brief words as ASCII branch text, capped at 40 characters.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "brief-based branch naming is deferred")
)]
pub(crate) fn slug(brief: &str) -> String {
    let mut slug = String::with_capacity(SLUG_CHARS);
    // Read at most 4096 input characters, even for an untrusted brief.
    for ch in brief.chars().take(4096) {
        if slug.len() == SLUG_CHARS {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "work".to_owned()
    } else {
        slug.to_owned()
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static COMMANDS: std::cell::RefCell<Vec<Vec<std::ffi::OsString>>> = const { std::cell::RefCell::new(Vec::new()) };
    static AFTER_ADD: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;
