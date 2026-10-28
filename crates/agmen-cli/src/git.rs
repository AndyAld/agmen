//! The git calls the CLI makes: the worktree root, the commit a claim
//! starts from, and DONE's commit, branch and changed paths.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use agmen_proto::{Change, GitSha};
use anyhow::{Context as _, Result, bail};
use tokio::process::Command;

/// How long one git call may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// The most changed paths a DONE lists; past this the envelope would not
/// fit in 64 KiB anyway.
const MAX_CHANGES: usize = 2_000;

/// Runs `git <args>` in `dir` and returns its stdout.
async fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let name = args.first().copied().unwrap_or_default();
    let output = tokio::time::timeout(
        GIT_TIMEOUT,
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .with_context(|| {
        format!(
            "git {name} did not answer within {} s",
            GIT_TIMEOUT.as_secs()
        )
    })?
    .with_context(|| format!("git {name} could not run"))?;
    if !output.status.success() {
        // Errors reach the model through agmen mcp, so they carry a fixed
        // reason and git's own words go to the debug log (ruling R44a).
        tracing::debug!(
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "git {name} failed"
        );
        match output.status.code() {
            Some(code) => bail!("git {name} exited {code}"),
            None => bail!("git {name} was killed"),
        }
    }
    Ok(output.stdout)
}

/// `git <args>`'s stdout as one trimmed line of text.
async fn git_line(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git(dir, args).await?;
    let text = String::from_utf8(out).context("git printed text that is not UTF-8")?;
    Ok(text.trim_end_matches(['\n', '\r']).to_owned())
}

/// The root of the git worktree `dir` is in, as git names it.
pub(crate) async fn toplevel(dir: &Path) -> Result<PathBuf> {
    let root = git_line(dir, &["rev-parse", "--show-toplevel"])
        .await
        .context("agmen join runs inside a git worktree")?;
    if root.is_empty() {
        bail!("agmen join runs inside a git worktree, not a bare repository");
    }
    Ok(PathBuf::from(root))
}

/// The worktree's `info/exclude` file.
pub(crate) async fn exclude_file(root: &Path) -> Result<PathBuf> {
    let path = git_line(root, &["rev-parse", "--git-path", "info/exclude"]).await?;
    Ok(root.join(path))
}

/// Whether `rel` is in the index; an untracked path is git's exit code 1.
pub(crate) async fn is_tracked(root: &Path, rel: &str) -> Result<bool> {
    let status = tokio::time::timeout(
        GIT_TIMEOUT,
        Command::new("git")
            .args(["ls-files", "--error-unmatch", "--", rel])
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .context("git ls-files did not answer within 10 s")?
    .context("git ls-files could not run")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(code) => bail!("git ls-files exited {code}"),
        None => bail!("git ls-files was killed"),
    }
}

/// Adds `line` to `info/exclude` unless it is already present.
pub(crate) fn add_exclude(path: &Path, line: &str) -> Result<()> {
    const MAX_EXCLUDE_BYTES: u64 = 1024 * 1024;
    let mut existing = String::new();
    match std::fs::File::open(path) {
        Ok(file) => {
            file.take(MAX_EXCLUDE_BYTES + 1)
                .read_to_string(&mut existing)
                .with_context(|| format!("reading {}", path.display()))?;
            if existing.len() as u64 > MAX_EXCLUDE_BYTES {
                bail!("{} exceeds the 1 MiB limit", path.display());
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    }
    if existing.lines().any(|existing| existing.trim() == line) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let lead = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    writeln!(file, "{lead}{line}").with_context(|| format!("writing {}", path.display()))
}

/// The full id of the commit `rev` names.
pub(crate) async fn commit(root: &Path, rev: &str) -> Result<GitSha> {
    let spec = format!("{rev}^{{commit}}");
    let sha = git_line(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ],
    )
    .await
    .with_context(|| format!("{rev} is not a commit in this repository"))?;
    sha.parse()
        .with_context(|| format!("git printed {sha:?} for {rev}, not a commit id"))
}

/// The current branch's short name, or `HEAD` when detached.
pub(crate) async fn branch(root: &Path) -> Result<String> {
    git_line(root, &["rev-parse", "--abbrev-ref", "HEAD"]).await
}

/// `git diff --name-status --no-renames <base>...<commit>` as DONE's
/// `changes`.
pub(crate) async fn changes(root: &Path, base: &GitSha, commit: &GitSha) -> Result<Vec<Change>> {
    let range = format!("{base}...{commit}");
    let out = git(
        root,
        &["diff", "--name-status", "--no-renames", "-z", &range, "--"],
    )
    .await?;
    parse_name_status(&out)
}

/// Parses `--name-status -z` output: a status field, then a path field,
/// each ending in NUL.
fn parse_name_status(out: &[u8]) -> Result<Vec<Change>> {
    let mut fields = out.split(|b| *b == 0).filter(|field| !field.is_empty());
    let mut changes = Vec::new();
    while let Some(status) = fields.next() {
        let Some(path) = fields.next() else {
            bail!("git diff printed a status without a path");
        };
        if changes.len() == MAX_CHANGES {
            bail!("the commit changes more than {MAX_CHANGES} paths, too many for one DONE");
        }
        changes.push(Change {
            status: String::from_utf8(status.to_vec()).context("a git status is not UTF-8")?,
            path: String::from_utf8(path.to_vec())
                .context("a changed path is not UTF-8, which a DONE cannot carry")?,
        });
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclude_is_added_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("info/exclude");
        add_exclude(&path, ".agmen/").unwrap();
        add_exclude(&path, ".agmen/").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), ".agmen/\n");
        std::fs::write(&path, "# git ls-files\n*.log").unwrap();
        add_exclude(&path, ".agmen/").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# git ls-files\n*.log\n.agmen/\n"
        );
    }

    #[tokio::test]
    async fn is_tracked_sees_committed_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]).await.unwrap();
        std::fs::write(root.join("tracked"), "yes").unwrap();
        std::fs::write(root.join("untracked"), "no").unwrap();
        git(root, &["add", "tracked"]).await.unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
        )
        .await
        .unwrap();
        assert!(is_tracked(root, "tracked").await.unwrap());
        assert!(!is_tracked(root, "untracked").await.unwrap());
        assert!(!is_tracked(root, "missing").await.unwrap());
        let outside = tempfile::tempdir().unwrap();
        assert!(is_tracked(outside.path(), "missing").await.is_err());
    }

    #[test]
    fn name_status_pairs() {
        let changes = parse_name_status(b"M\0README.md\0A\0src/a b.rs\0D\0old\0").unwrap();
        let pairs: Vec<(&str, &str)> = changes
            .iter()
            .map(|c| (c.status.as_str(), c.path.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("M", "README.md"), ("A", "src/a b.rs"), ("D", "old")]
        );
        assert!(parse_name_status(b"").unwrap().is_empty());
        assert!(parse_name_status(b"M\0").is_err());
    }
}
