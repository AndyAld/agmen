//! Bounded, fixed-argument listener git reads.

use crate::git::{Git, GitError};

pub(super) const GIT_READ_CAP: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GitRead {
    Log {
        rev_range: Option<String>,
        n: Option<u32>,
    },
    Show {
        rev: String,
        stat: bool,
    },
    Diff {
        base: String,
        head: String,
    },
    Status,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum GitReadError {
    #[error("invalid git {what}")]
    BadRev { what: &'static str },
    #[error("git log count must be 1 to 200")]
    BadCount,
    #[error(transparent)]
    Git(#[from] GitError),
}

fn validate_rev(rev: &str, what: &'static str) -> Result<(), GitReadError> {
    if rev.is_empty()
        || rev.len() > 256
        || rev.starts_with('-')
        || rev.chars().any(char::is_control)
    {
        return Err(GitReadError::BadRev { what });
    }
    Ok(())
}

pub(super) fn argv(req: &GitRead) -> Result<Vec<String>, GitReadError> {
    let args = match req {
        GitRead::Log { rev_range, n } => {
            let n = n.unwrap_or(50);
            if !(1..=200).contains(&n) {
                return Err(GitReadError::BadCount);
            }
            let mut args = vec![
                "log".into(),
                "--oneline".into(),
                "-n".into(),
                n.to_string(),
                "--no-show-signature".into(),
                "--end-of-options".into(),
            ];
            if let Some(range) = rev_range {
                validate_rev(range, "range")?;
                args.push(range.clone());
            }
            args
        }
        GitRead::Show { rev, stat } => {
            validate_rev(rev, "rev")?;
            let mut args: Vec<String> = ["show", "--no-ext-diff", "--no-textconv", "--no-color"]
                .map(String::from)
                .into();
            if *stat {
                args.push("--stat".into());
            }
            args.extend([
                "--no-show-signature".into(),
                "--end-of-options".into(),
                rev.clone(),
            ]);
            args
        }
        GitRead::Diff { base, head } => {
            validate_rev(base, "base")?;
            validate_rev(head, "head")?;
            // The trailing `--` makes both operands revisions. Without it, two
            // paths outside the repository run as `git diff --no-index`, which
            // reads arbitrary files.
            [
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--end-of-options",
                base,
                head,
                "--",
            ]
            .map(String::from)
            .into()
        }
        GitRead::Status => ["status", "--porcelain=v1", "--branch"]
            .map(String::from)
            .into(),
    };
    Ok(args)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GitReadAnswer {
    pub(super) text: String,
    pub(super) truncated: bool,
}

pub(super) async fn run(git: &Git, req: &GitRead) -> Result<GitReadAnswer, GitReadError> {
    let args = argv(req)?;
    let output = git
        .run_str(
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            GIT_READ_CAP,
        )
        .await?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let expanded = text.len() > GIT_READ_CAP;
    // Lossy replacement can expand invalid UTF-8; keep the returned text bounded too.
    if expanded {
        text.truncate(text.floor_char_boundary(GIT_READ_CAP));
    }
    Ok(GitReadAnswer {
        text,
        truncated: output.truncated || expanded,
    })
}

#[cfg(test)]
#[path = "git_read_tests.rs"]
mod tests;
