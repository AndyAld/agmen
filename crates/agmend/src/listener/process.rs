//! Long-lived stream-json listener process and bounded turns.

// shortcut: L21 wires this into the listener runtime
#![cfg_attr(not(test), expect(dead_code, reason = "wired by L21"))]

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process_group};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

use super::batch::MAX_BATCH_BYTES;
use crate::budget::{Budget, InvalidCost, Money};

const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_EVENTS: usize = 1024;
const STDERR_BYTES: usize = 8 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(super) struct ListenerArgs {
    pub(super) program: PathBuf,
    pub(super) model: String,
    pub(super) mcp_config: PathBuf,
    pub(super) prompt_file: PathBuf,
    pub(super) max_budget_usd: Money,
}

impl ListenerArgs {
    pub(super) fn argv(&self) -> Vec<OsString> {
        let mut args: Vec<_> = [
            "-p",
            "--model",
            &self.model,
            "--output-format",
            "stream-json",
            "--verbose",
            "--tools",
            "Read,Grep,Glob",
            "--strict-mcp-config",
            "--mcp-config",
        ]
        .map(OsString::from)
        .into();
        args.push(self.mcp_config.as_os_str().to_owned());
        args.push("--append-system-prompt-file".into());
        args.push(self.prompt_file.as_os_str().to_owned());
        args.extend([
            "--max-budget-usd".into(),
            self.max_budget_usd.usd().into(),
            "--input-format".into(),
            "stream-json".into(),
        ]);
        args
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum CostError {
    #[error("invalid incremental model usage: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid incremental model cost: {0}")]
    Amount(#[from] InvalidCost),
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ProcessError {
    #[error("spawning listener: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("listener turn timeout must be 1 to 180 seconds")]
    InvalidTimeout,
    #[error("listener input exceeds 64 KiB")]
    OversizeInput,
    #[error("listener {operation}: {source}; stderr: {stderr}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
        stderr: String,
    },
    #[error("listener {operation} timed out; stderr: {stderr}")]
    Timeout {
        operation: &'static str,
        #[source]
        source: tokio::time::error::Elapsed,
        stderr: String,
    },
    #[error("listener output line exceeds 1 MiB; stderr: {stderr}")]
    OversizeLine { stderr: String },
    #[error("listener result missing after 1024 events; stderr: {stderr}")]
    TooManyEvents { stderr: String },
    #[error("invalid listener JSON: {source}; stderr: {stderr}")]
    InvalidJson {
        #[source]
        source: serde_json::Error,
        stderr: String,
    },
    #[error("invalid listener cost: {source}; stderr: {stderr}")]
    InvalidCost {
        #[source]
        source: CostError,
        stderr: String,
    },
    #[error("listener closed stdout before a result; stderr: {stderr}")]
    EarlyExit { stderr: String },
}

impl ProcessError {
    fn io(operation: &'static str, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            source,
            stderr: String::new(),
        }
    }

    fn with_stderr(mut self, tail: &VecDeque<u8>) -> Self {
        let bytes: Vec<_> = tail.iter().copied().collect();
        let text = String::from_utf8_lossy(&bytes);
        // Invalid UTF-8 can expand on conversion; error text stays under 8 KiB.
        let text = &text[text.ceil_char_boundary(text.len().saturating_sub(STDERR_BYTES))..];
        match &mut self {
            Self::Io { stderr, .. }
            | Self::Timeout { stderr, .. }
            | Self::OversizeLine { stderr }
            | Self::TooManyEvents { stderr }
            | Self::InvalidJson { stderr, .. }
            | Self::InvalidCost { stderr, .. }
            | Self::EarlyExit { stderr } => *stderr = text.to_owned(),
            Self::Spawn(_) | Self::InvalidTimeout | Self::OversizeInput => {}
        }
        self
    }
}

#[derive(Debug)]
pub(super) struct Turn {
    pub(super) result: Value,
    pub(super) tool_calls: Vec<Value>,
    pub(super) cost: Money,
    pub(super) is_error: bool,
}

#[derive(Debug)]
pub(super) struct ListenerProcess {
    child: Child,
    group: Option<Pid>,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: ChildStderr,
    stderr_tail: VecDeque<u8>,
    turn_timeout: Duration,
}

impl ListenerProcess {
    pub(super) async fn spawn(
        args: &ListenerArgs,
        cwd: &Path,
        env: &[(OsString, OsString)],
        turn_timeout: Duration,
    ) -> Result<Self, ProcessError> {
        if !(Duration::from_secs(1)..=Duration::from_secs(180)).contains(&turn_timeout) {
            return Err(ProcessError::InvalidTimeout);
        }
        let mut child = Command::new(&args.program)
            .args(args.argv())
            .current_dir(cwd)
            .env_clear()
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(ProcessError::Spawn)?;
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(Pid::from_raw)
            .ok_or_else(|| {
                ProcessError::Spawn(std::io::Error::other("listener process ID missing"))
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProcessError::Spawn(std::io::Error::other("listener stdin missing")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProcessError::Spawn(std::io::Error::other("listener stdout missing")))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ProcessError::Spawn(std::io::Error::other("listener stderr missing")))?;
        Ok(Self {
            child,
            group: Some(group),
            stdin,
            stdout: BufReader::new(stdout),
            stderr,
            stderr_tail: VecDeque::with_capacity(STDERR_BYTES),
            turn_timeout,
        })
    }

    /// On a write failure the group is killed, since a partial batch cannot be retried safely.
    /// After cancelling this future, stop or drop the process before reusing it.
    pub(super) async fn send(&mut self, message: &[u8]) -> Result<(), ProcessError> {
        if message.len() > MAX_BATCH_BYTES {
            return Err(ProcessError::OversizeInput);
        }
        let work = async {
            self.stdin
                .write_all(message)
                .await
                .map_err(|source| ProcessError::io("writing input", source))
        };
        let result = with_stderr(
            &mut self.stderr,
            &mut self.stderr_tail,
            WRITE_TIMEOUT,
            "writing input",
            work,
        )
        .await;
        if result.is_err() {
            self.kill_group();
        }
        result
    }

    /// An unsuccessful turn terminates the group rather than reusing a partial stream.
    /// After cancelling this future, stop or drop the process before reusing it.
    pub(super) async fn next_turn(&mut self) -> Result<Turn, ProcessError> {
        let work = async {
            let mut tool_calls = Vec::new();
            for _ in 0..MAX_EVENTS {
                let mut bytes = Vec::new();
                let n = (&mut self.stdout)
                    .take((MAX_LINE_BYTES + 1) as u64)
                    .read_until(b'\n', &mut bytes)
                    .await
                    .map_err(|source| ProcessError::io("reading output", source))?;
                if n == 0 {
                    return Err(ProcessError::EarlyExit {
                        stderr: String::new(),
                    });
                }
                if n > MAX_LINE_BYTES {
                    return Err(ProcessError::OversizeLine {
                        stderr: String::new(),
                    });
                }
                let event: Value =
                    serde_json::from_slice(&bytes).map_err(|source| ProcessError::InvalidJson {
                        source,
                        stderr: String::new(),
                    })?;
                if event["type"] == "assistant"
                    && let Some(content) = event["message"]["content"].as_array()
                {
                    tool_calls.extend(
                        content
                            .iter()
                            .filter(|block| block["type"] == "tool_use")
                            .cloned(),
                    );
                }
                if event["type"] == "result" {
                    let cost = turn_cost(&bytes).map_err(|source| ProcessError::InvalidCost {
                        source,
                        stderr: String::new(),
                    })?;
                    return Ok(Turn {
                        is_error: event["is_error"].as_bool().unwrap_or(false),
                        result: event,
                        tool_calls,
                        cost,
                    });
                }
            }
            Err(ProcessError::TooManyEvents {
                stderr: String::new(),
            })
        };
        let result = with_stderr(
            &mut self.stderr,
            &mut self.stderr_tail,
            self.turn_timeout,
            "reading turn",
            work,
        )
        .await;
        if result.is_err() {
            self.kill_group();
        }
        result
    }

    pub(super) async fn stop(mut self) {
        self.kill_group();
        match timeout(STOP_TIMEOUT, self.child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "waiting for listener exit failed"),
            Err(_) => tracing::warn!("waiting for listener exit exceeded 5 seconds"),
        }
    }

    fn kill_group(&mut self) {
        if let Some(group) = self.group.take()
            && let Err(error) = kill_process_group(group, Signal::KILL)
            && error != rustix::io::Errno::SRCH
        {
            tracing::warn!(%error, "could not kill listener process group");
        }
    }
}

impl Drop for ListenerProcess {
    fn drop(&mut self) {
        self.kill_group();
    }
}

/// Both the pipe drain loop and work are bounded by this operation's deadline.
async fn with_stderr<T>(
    stderr: &mut ChildStderr,
    tail: &mut VecDeque<u8>,
    limit: Duration,
    operation: &'static str,
    work: impl Future<Output = Result<T, ProcessError>>,
) -> Result<T, ProcessError> {
    let result = timeout(limit, async {
        let drain = async {
            let mut buffer = [0; STDERR_BYTES];
            loop {
                let n = stderr.read(&mut buffer).await.map_err(|source| ProcessError::io("reading stderr", source))?;
                if n == 0 { return Ok::<_, ProcessError>(()); }
                let discard = (tail.len() + n).saturating_sub(STDERR_BYTES);
                tail.drain(..discard);
                tail.extend(&buffer[..n]);
            }
        };
        tokio::pin!(work, drain);
        tokio::select! {
            result = &mut work => {
                if matches!(result, Err(ProcessError::EarlyExit { .. })) {
                    // Capture the exit diagnostic, but descendants may still hold this pipe.
                    // A drain timeout is harmless: the original stdout EOF is the error.
                    if let Ok(drained) = timeout(Duration::from_millis(100), &mut drain).await { drained?; }
                }
                result
            }
            drained = &mut drain => { drained?; work.await }
        }
    }).await.unwrap_or_else(|source| Err(ProcessError::Timeout { operation, source, stderr: String::new() }));
    result.map_err(|error| error.with_stderr(tail))
}

fn turn_cost(bytes: &[u8]) -> Result<Money, CostError> {
    #[derive(Deserialize)]
    struct Usage {
        model_usage_since_last_result: Option<BTreeMap<String, ModelUsage>>,
    }
    #[derive(Deserialize)]
    struct ModelUsage {
        #[serde(rename = "costUSD")]
        cost: Box<RawValue>,
    }
    let usage: Usage = serde_json::from_slice(bytes)?;
    let mut sum = Budget::new(Money::from_usd("0")?);
    if let Some(models) = usage.model_usage_since_last_result {
        // The number of models and token sizes are bounded by the output line cap.
        for model in models.into_values() {
            sum.add_cost_usd(model.cost.get(), |_| {})?;
        }
    } else {
        tracing::warn!("listener result missing incremental model usage; charging zero");
    }
    Ok(sum.spent())
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
