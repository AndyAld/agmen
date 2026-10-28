//! The git calls the CLI makes: the worktree root, the commit a claim
//! starts from, and DONE's commit, branch and changed paths.

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
