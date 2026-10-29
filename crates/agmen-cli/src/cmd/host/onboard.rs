//! Claim a self-hosted hub and join its first project.

use std::io::IsTerminal as _;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use agmen_proto::RepoUrl;
use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;
use url::Url;

use super::docker::Compose;
use crate::client::{self, Human, Membership, blocking, refusal};
use crate::cmd::{account, join};
use crate::git;
use crate::output::clean;

const GIT_LIMIT: Duration = Duration::from_secs(10);
const ORIGIN_CAP: u64 = 4 * 1024;
const INVITE_LIMIT: Duration = Duration::from_secs(30);

/// The newest claim field must contain exactly 64 lowercase hex digits.
pub(crate) fn claim_token_from_logs(logs: &str) -> Option<String> {
    let (_, tail) = logs.rsplit_once("claim_token=")?;
    let token = tail.split(char::is_whitespace).next()?;
    (token.len() == 64
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then(|| token.to_owned())
}

/// Make a running hub usable, reusing sign-in, membership and local identity.
pub(crate) async fn onboard(
    hub: &Url,
    compose: &Compose,
    project: Option<&str>,
    handle: Option<&str>,
) -> Result<()> {
    let human = match client::human_at(hub).await? {
        Some(human) => human,
        None if std::io::stdin().is_terminal() => account::device_login(hub).await?,
        None => {
            println!(
                "the hub runs at {hub}; sign in from a terminal with agmen login --hub {hub}, then run agmen host up again to claim it"
            );
            return Ok(());
        }
    };
    let mut me = human.api.me().await?;
    if !me
        .groups
        .iter()
        .any(|g| matches!(g.role.as_str(), "owner" | "admin"))
    {
        claim(&human, hub, compose).await?;
        me = human.api.me().await?;
    }
    let group = me
        .groups
        .iter()
        .find(|g| matches!(g.role.as_str(), "owner" | "admin"))
        .context("the hub did not return your group after claiming it")?;
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let root = match git::toplevel(&cwd).await {
        Ok(root) => root,
        Err(err)
            if err.chain().any(|cause| {
                cause.is::<std::io::Error>()
                    || cause.is::<tokio::time::error::Elapsed>()
                    || cause.is::<std::string::FromUtf8Error>()
            }) =>
        {
            return Err(err);
        }
        Err(_) => {
            println!(
                "run agmen host up again inside a clone of your repository to create the first project"
            );
            return Ok(());
        }
    };
    let to_find = root.clone();
    let existing = blocking(move || {
        agmen_client::find_identity(&to_find).context("reading this worktree's identity")
    })
    .await?;
    if let Some(joined) = &existing
        && (!client::same_hub(&joined.identity.hub_url, hub)
            || joined.identity.tenant.as_str() != group.tenant
            || !me
                .projects
                .iter()
                .any(|p| p.project == joined.identity.project.as_str()))
    {
        bail!("this worktree is joined to another hub or group; run agmen leave first");
    }
    let membership = me.projects.iter().find(|p| {
        p.tenant == group.tenant
            && existing
                .as_ref()
                .is_none_or(|joined| p.project == joined.identity.project.as_str())
    });
    let project = match membership {
        Some(project) => project.clone(),
        None => create_project(&human, &root, &group.tenant, project).await?,
    };
    let identity = match existing {
        Some(joined) => joined.identity,
        None => {
            let args = join::JoinArgs::with_handle(handle.map(str::to_owned));
            let identity = join::join_here(&human, &root, &project.project, &args).await?;
            println!(
                "joined project {} ({}) as {} ({})",
                clean(&project.name),
                clean(&project.project),
                clean(&identity.handle),
                identity.identity_id
            );
            identity
        }
    };
    join::run_setup(&root, &identity).await;
    invite(&project.project).await
}

async fn claim(human: &Human, hub: &Url, compose: &Compose) -> Result<()> {
    for attempt in 0..2 {
        let logs = compose
            .logs_text("hub")
            .await
            .map_err(|_| anyhow::anyhow!("reading the hub's claim token failed"))?;
        if let Some(token) = claim_token_from_logs(&logs) {
            // Never retain a hub refusal's description or error chain: it can echo the token.
            match human
                .api
                .post::<_, serde_json::Value>(&["claim"], &serde_json::json!({"token":token}))
                .await
            {
                Ok(_) => return Ok(()),
                Err(err)
                    if attempt == 0
                        && refusal(&err).is_some_and(|r| {
                            (r.status == 410 && r.code == "claim_expired")
                                || (r.status == 403 && r.code == "bad_claim_token")
                        }) => {}
                Err(err) => return Err(claim_error(&err)),
            }
        } else if attempt == 1 {
            bail!("the hub did not log a claim token after restarting; check agmen host logs hub");
        }
        if attempt == 0 {
            compose.restart("hub").await.map_err(|_| {
                anyhow::anyhow!("restarting the hub to renew its claim token failed")
            })?;
            let mut health = hub.clone();
            health
                .path_segments_mut()
                .map_err(|_| anyhow::anyhow!("the hub URL cannot name a health endpoint"))?
                .pop_if_empty()
                .push("healthz");
            super::wait_healthy_at(health.as_str()).await.map_err(|_| {
                anyhow::anyhow!(
                    "the hub did not become healthy after restarting to renew its claim token"
                )
            })?;
        }
    }
    bail!("claiming the hub failed after one restart; run agmen host up again")
}

fn claim_error(err: &anyhow::Error) -> anyhow::Error {
    let message = match refusal(err).map(|r| (r.status, r.code.as_str())) {
        Some((409, "already_claimed")) => "this hub already has an owner; ask them for an invite",
        Some((410, "claim_expired")) => {
            "the hub's claim token expired after one restart; run agmen host up again"
        }
        Some((403, "bad_claim_token")) => {
            "the hub refused its claim token after one restart; run agmen host up again"
        }
        _ => "claiming the hub failed; run agmen host up again",
    };
    anyhow::anyhow!(message)
}

#[derive(Serialize)]
struct CreateProject<'a> {
    name: &'a str,
    tenant: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_url: Option<RepoUrl>,
}

async fn create_project(
    human: &Human,
    root: &Path,
    tenant: &str,
    name: Option<&str>,
) -> Result<Membership> {
    let name = name
        .or_else(|| root.file_name().and_then(|n| n.to_str()))
        .context("the worktree folder has no project name; pass --project <name>")?;
    let repo_url = match origin(root).await {
        Ok(url) => Some(url),
        Err(err) => {
            println!(
                "the project has no repository URL: {}",
                clean(&err.to_string())
            );
            None
        }
    };
    let body = CreateProject {
        name,
        tenant,
        repo_url,
    };
    human.api.post(&["projects"], &body).await.map_err(|err| {
        match refusal(&err).map(|r| r.code.as_str()) {
            Some("admin_required") => anyhow::anyhow!("only a group owner or admin can create a project"),
            Some("tenant_required") => anyhow::anyhow!("the hub needs a group for this project"),
            Some("invalid_name") => anyhow::anyhow!("the project name must have 1 to 64 characters with no surrounding whitespace or control characters"),
            Some("project_exists") => anyhow::anyhow!("a project with that name already exists in this group"),
            Some("bad_repo_url") => anyhow::anyhow!("the hub refused the repository URL"),
            _ => err.context("creating the first project failed"),
        }
    })
}

/// Keep git's stdout bounded and discard stderr: either can contain credentials.
async fn origin(root: &Path) -> Result<RepoUrl> {
    let mut child = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("git remote could not run")?;
    let stdout = child
        .stdout
        .take()
        .context("git remote stdout was not piped")?;
    let (status, bytes) = tokio::time::timeout(GIT_LIMIT, async {
        tokio::try_join!(
            async { child.wait().await.context("waiting for git remote") },
            read_origin(stdout)
        )
    })
    .await
    .context("git remote did not answer within 10 s")??;
    if !status.success() {
        bail!("git origin is unavailable");
    }
    let text = std::str::from_utf8(&bytes).context("git origin is not UTF-8")?;
    text.trim_end_matches(['\n', '\r'])
        .parse()
        .map_err(Into::into)
}

async fn read_origin(stdout: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stdout
        .take(ORIGIN_CAP + 1)
        .read_to_end(&mut bytes)
        .await
        .context("reading git origin")?;
    if bytes.len() as u64 > ORIGIN_CAP {
        bail!("git origin exceeds the 4 KiB limit");
    }
    Ok(bytes)
}

async fn invite(project: &str) -> Result<()> {
    let exe = std::env::current_exe().context("finding agmen to print an invite")?;
    let status = tokio::time::timeout(
        INVITE_LIMIT,
        Command::new(exe)
            .args(["invite", "--project", project])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .context("printing the invite exceeded 30 s")?
    .context("running agmen invite")?;
    if !status.success() {
        bail!(
            "agmen invite failed; run agmen invite --project {}",
            clean(project)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Refusal;

    #[test]
    fn claim_token_from_logs_takes_the_last() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let logs = format!(
            "hub-1 | warn claim_token={a} expires_at=now\nhub-1 | warn claim_token={b} expires_at=later\n"
        );
        assert_eq!(claim_token_from_logs(&logs), Some(b.clone()));
        for invalid in [
            "a".repeat(63),
            "A".repeat(64),
            "a".repeat(65),
            format!("{b}z"),
            format!(" {b}"),
            format!("\n{b}"),
        ] {
            assert_eq!(
                claim_token_from_logs(&format!("claim_token={invalid}")),
                None
            );
        }
        assert_eq!(claim_token_from_logs("no field"), None);
        assert_eq!(
            claim_token_from_logs(&format!("claim_token={a}\nclaim_token=bad")),
            None
        );
        assert_eq!(claim_token_from_logs(&format!("claim_token={a}")), Some(a));
    }

    #[test]
    fn claim_token_never_in_error_text() {
        let token = String::from("abcdef01").repeat(8);
        for (status, code) in [
            (403, "bad_claim_token"),
            (410, "claim_expired"),
            (409, "already_claimed"),
            (500, "unexpected"),
            (401, "unauthorized"),
        ] {
            let err = Refusal {
                status,
                code: code.into(),
                description: format!("bad token {token}"),
            }
            .into();
            let shown = claim_error(&err);
            assert!(!format!("{shown:#}").contains(&token));
            assert!(!format!("{shown:?}").contains(&token));
        }
        let err = anyhow::anyhow!("failed with {token}").context(format!("transport {token}"));
        assert!(!format!("{:#}", claim_error(&err)).contains(&token));
    }
}
