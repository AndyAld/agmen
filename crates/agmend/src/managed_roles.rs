//! Child project roles are resolved from reviewed Git objects, never agent files.

use std::path::Path;
use std::time::Duration;

use agmen_local::ApiError;

use super::{Managed, Record, error};
use crate::console::bad_request;
use crate::git::{Git, GitError};

impl Managed {
    /// The main checkout's branch is the base, as for agent_prune.
    pub(super) async fn child_role_commit(
        &self,
        repo: &Path,
    ) -> Result<(String, String), ApiError> {
        let git = Git::new(repo, tokio::time::Instant::now() + Duration::from_secs(5));
        let no_branch = || {
            bad_request(
                "child roles are read from origin/<branch> of the main checkout, and it has no current branch; check out a branch there",
            )
        };
        let branch = git
            .run_str(&["symbolic-ref", "--quiet", "--short", "HEAD"], 4096)
            .await
            .map_err(|failure| match failure {
                GitError::Failed { .. } => no_branch(),
                other => error(other),
            })?;
        if branch.truncated {
            return Err(bad_request("project base branch exceeds 4096 bytes"));
        }
        let branch = std::str::from_utf8(&branch.stdout).map_err(error)?.trim();
        if branch.is_empty() {
            return Err(no_branch());
        }
        let revision = format!("origin/{branch}");
        let reference = format!("refs/remotes/{revision}^{{commit}}");
        let output = git
            .run_str(
                &["rev-parse", "--verify", "--end-of-options", &reference],
                128,
            )
            .await
            .map_err(|failure| {
                bad_request(format!(
                    "project roles need reviewed commit {revision}: {failure}"
                ))
            })?;
        let oid = std::str::from_utf8(&output.stdout).map_err(error)?.trim();
        if output.truncated
            || !matches!(oid.len(), 40 | 64)
            || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(error("git returned an invalid project role commit"));
        }
        Ok((revision, oid.to_owned()))
    }

    pub(super) async fn record_role(
        &self,
        record: &Record,
        name: &str,
    ) -> Result<agmen_roles::Resolved, ApiError> {
        match &record.role_commit {
            Some(commit) => self.load_role_at(&record.repo, name, commit).await,
            None => self.resolve_role(&record.repo, name).await,
        }
    }

    /// Reuse Layers' inheritance, layer precedence, shadow checks and hashing.
    /// The snapshot has the same bounds as agmen-roles' project directory reader.
    pub(super) async fn load_role_at(
        &self,
        repo: &Path,
        name: &str,
        commit: &str,
    ) -> Result<agmen_roles::Resolved, ApiError> {
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(bad_request("stored project role commit is invalid"));
        }
        let git = Git::new(repo, tokio::time::Instant::now() + Duration::from_secs(5));
        let output = git
            .run_str(
                &["ls-tree", "-r", "-z", commit, "--", ".agmen/roles/"],
                1024 * 1024,
            )
            .await
            .map_err(error)?;
        if output.truncated {
            return Err(bad_request("project role listing exceeds 1 MiB"));
        }
        let snapshot = tokio::task::spawn_blocking(tempfile::tempdir)
            .await
            .map_err(error)?
            .map_err(error)?;
        let dir = snapshot.path().join(".agmen/roles");
        tokio::fs::create_dir_all(&dir).await.map_err(error)?;
        let mut files = 0;
        for (index, entry) in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
            .enumerate()
        {
            if index >= 4096 {
                return Err(bad_request("project role directory exceeds 4096 entries"));
            }
            let entry = std::str::from_utf8(entry).map_err(error)?;
            let (metadata, path) = entry
                .split_once('\t')
                .ok_or_else(|| error("invalid git role entry"))?;
            let Some(file) = path.strip_prefix(".agmen/roles/") else {
                continue;
            };
            if file.contains('/') || !file.ends_with(".xml") {
                continue;
            }
            if files >= 64 {
                break;
            }
            files += 1;
            let mut fields = metadata.split(' ');
            let (mode, kind, oid) = (fields.next(), fields.next(), fields.next());
            if !matches!(mode, Some("100644" | "100755")) || kind != Some("blob") {
                continue;
            }
            let oid = oid.ok_or_else(|| error("git role blob has no object ID"))?;
            let blob = git
                .run_str(&["cat-file", "blob", oid], super::MAX_ROLE_PROMPT_BYTES)
                .await
                .map_err(error)?;
            if blob.truncated || std::str::from_utf8(&blob.stdout).is_err() {
                continue;
            }
            tokio::fs::write(dir.join(format!("{index:04}.xml")), blob.stdout)
                .await
                .map_err(error)?;
        }
        self.load_role(snapshot.path(), name).await
    }
}
