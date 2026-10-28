//! Claude Code's `plugin list --json` returns an array of objects. The
//! installed plugin name is in `id`. Observed keys: `id`, `version`,
//! `scope`, `enabled`, `installPath`; some objects also have `installedAt`,
//! `lastUpdated`, `projectPath`, `projectEnabled`.

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;

use super::{AgentEnv, Outcome, Plugin, json::merge_mcp_servers, write_worktree_file};

const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize)]
struct InstalledPlugin {
    id: String,
}

pub(super) async fn run(root: &Path, env: &AgentEnv, plugin: Plugin) -> Result<Outcome> {
    if plugin == Plugin::Decline {
        return write_worktree_file(root, ".mcp.json", merge_mcp_servers).await;
    }
    let failure = if let Some(program) = env.find_program("claude") {
        if let Ok(output) =
            command(&program, root, &["plugin", "list", "--json"], LIST_TIMEOUT).await
            && output.status.success()
            && let Ok(plugins) = serde_json::from_slice::<Vec<InstalledPlugin>>(&output.stdout)
            && plugins.iter().any(|plugin| plugin.id == "agmen@agmen")
        {
            return Ok(Outcome::Unchanged);
        }
        let args = [
            "plugin",
            "install",
            "agmen",
            "--marketplace",
            "agmen-dev/agmen",
            "--scope",
            "user",
            "--yes",
        ];
        match command(&program, root, &args, INSTALL_TIMEOUT).await {
            Ok(output) if output.status.success() => {
                return Ok(Outcome::Configured("installed the agmen plugin".into()));
            }
            Ok(output) => {
                let line: String = String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
                if line.is_empty() {
                    format!("{}", output.status)
                } else {
                    line
                }
            }
            Err(err) => err.to_string().chars().take(200).collect(),
        }
    } else {
        "claude is not on PATH".into()
    };
    let outcome = write_worktree_file(root, ".mcp.json", merge_mcp_servers).await?;
    Ok(match outcome {
        Outcome::Configured(_) => Outcome::Configured(format!(
            "plugin install failed ({failure}); wrote .mcp.json instead"
        )),
        Outcome::Unchanged => Outcome::Configured(format!(
            "plugin install failed ({failure}); .mcp.json already set up"
        )),
        Outcome::Skipped(why) => {
            Outcome::Skipped(format!("plugin install failed ({failure}); {why}"))
        }
        Outcome::Failed(why) => {
            Outcome::Failed(format!("plugin install failed ({failure}); {why}"))
        }
    })
}

/// Bound both pipes as well as wall time; kill and reap on any failure.
async fn command(program: &Path, root: &Path, args: &[&str], limit: Duration) -> Result<Output> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting claude")?;
    let stdout = child.stdout.take().context("claude stdout was not piped")?;
    let stderr = child.stderr.take().context("claude stderr was not piped")?;
    let result = tokio::time::timeout(limit, async {
        let (status, stdout, stderr) = tokio::try_join!(
            async { child.wait().await.context("waiting for claude") },
            read_output(stdout),
            read_output(stderr),
        )?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
    .await;
    let failure = match result {
        Ok(Ok(output)) => return Ok(output),
        Ok(Err(err)) => err,
        Err(_) => anyhow::anyhow!("claude did not answer within {} s", limit.as_secs()),
    };
    // kill_on_drop is the fallback if killing or reaping does not finish.
    match tokio::time::timeout(Duration::from_secs(5), child.kill()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::debug!(%err, "killing claude failed"),
        Err(err) => tracing::debug!(%err, "reaping claude timed out"),
    }
    Err(failure)
}

async fn read_output(pipe: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.take(MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .context("reading claude output")?;
    if bytes.len() as u64 > MAX_OUTPUT_BYTES {
        bail!("claude output exceeds the 1 MiB limit");
    }
    Ok(bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plugin_command_deadline_kills_and_reaps_the_child() {
        let dir = tempfile::tempdir().unwrap();
        // exec keeps the sleeper in the process we own and kill.
        let err = command(
            Path::new("/bin/sh"),
            dir.path(),
            &["-c", "exec /bin/sleep 10"],
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("did not answer within"));
    }

    #[tokio::test]
    async fn plugin_output_has_a_size_limit() {
        let err = read_output(&vec![b'x'; MAX_OUTPUT_BYTES as usize + 1][..])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exceeds the 1 MiB limit"));
    }
}
