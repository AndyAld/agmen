//! Bounded subprocess I/O for future live_model_ S7 tests. Nothing launches on import.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

/// Maximum input batch or logged call size.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
/// Maximum size of one stream-json output event or the entire call log.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum number of events collected before a result must arrive.
const MAX_EVENTS: usize = 1024;
/// Maximum number of batches sent to one process.
const MAX_BATCHES: usize = 40;
/// Maximum timeout accepted for any operation.
const MAX_TIMEOUT: Duration = Duration::from_secs(180);

/// Failures at the listener harness boundary, retaining I/O and parse sources.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A harness limit or process status prevented the operation.
    #[error("{0}")]
    Invalid(&'static str),
    /// An operation failed with its context and source chain.
    #[error(transparent)]
    Operation(#[from] anyhow::Error),
    /// Encoding a batch or MCP configuration failed.
    #[error("encoding listener JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// The two runtime modes S7 will compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeMode {
    /// A long-lived stream-json process.
    Stream,
    /// One process per batch, starting or resuming the named session.
    Batch {
        /// The session used across batches.
        session_id: String,
        /// Resume instead of creating the session.
        resume: bool,
    },
}

/// Explicit paths and limits for one listener process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerOptions {
    /// Claude executable for live work, or an offline fixture executable.
    pub program: PathBuf,
    /// Model passed to Claude.
    pub model: String,
    /// Strict MCP config containing the fake agmen server only.
    pub mcp_config: PathBuf,
    /// Rendered listener role file.
    pub prompt: PathBuf,
    /// JSONL call log, passed to the fake through S7_LOG.
    pub log: PathBuf,
    /// Stream or per-batch operation.
    pub mode: RuntimeMode,
    /// I/O and exit deadline, at most 180 seconds.
    pub timeout: Duration,
}

impl ListenerOptions {
    /// The argv specified by the refreshed S7 plan, capped at $2 per process.
    #[must_use]
    pub fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
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
        .into_iter()
        .map(OsString::from)
        .collect();
        args.push(self.mcp_config.clone().into_os_string());
        args.push("--append-system-prompt-file".into());
        args.push(self.prompt.clone().into_os_string());
        args.extend(["--max-budget-usd".into(), "2.00".into()]);
        match &self.mode {
            RuntimeMode::Stream => args.extend(["--input-format".into(), "stream-json".into()]),
            RuntimeMode::Batch { session_id, resume } => {
                args.push(if *resume { "--resume" } else { "--session-id" }.into());
                args.push(session_id.into());
            }
        }
        args
    }

    /// Write an isolated MCP config for this process, with no credentials.
    ///
    /// # Errors
    /// Returns an error if a path cannot be encoded or the config cannot be written.
    pub async fn write_mcp_config(&self, server: &Path) -> Result<(), Error> {
        let config = json!({"mcpServers":{"agmen":{
            "command": server.to_str().context("fake MCP path is not UTF-8")?,
            "args":[], "env":{"S7_LOG":self.log.to_str().context("log path is not UTF-8")?}
        }}});
        tokio::fs::write(&self.mcp_config, serde_json::to_vec(&config)?)
            .await
            .context("writing isolated fake MCP config")?;
        Ok(())
    }
}

/// A complete result and the assistant tool calls preceding it.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    /// The unmodified result event, including model errors and session id.
    pub result: Value,
    /// Assistant content blocks whose type is tool_use.
    pub tool_calls: Vec<Value>,
    /// Cost reported by Claude, without assuming it is incremental.
    pub total_cost_usd: Option<f64>,
    /// Time from the oldest outstanding batch to its result.
    pub latency: Duration,
}

/// A recorded fake MCP call, one per JSON line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Tool name, without the MCP namespace prefix.
    pub tool: String,
    /// Parsed tool arguments.
    pub args: Value,
}

/// A listener child. Dropping it requests termination; close reaps it.
#[derive(Debug)]
pub struct Harness {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    mode: RuntimeMode,
    deadline: Duration,
    sent: usize,
    pending: std::collections::VecDeque<Instant>,
}

impl Harness {
    /// Spawn the configured listener. This is an explicit, potentially paid operation.
    ///
    /// # Errors
    /// Returns an error for an invalid deadline or a failed spawn.
    pub async fn spawn(options: ListenerOptions) -> Result<Self, Error> {
        Self::spawn_command(options, |_| {}).await
    }

    /// Spawn with a command prefix, allowing offline fixtures to replace Claude.
    /// The prefix is added before the normal listener arguments.
    ///
    /// # Errors
    /// Returns an error for an invalid deadline or a failed spawn.
    pub async fn spawn_command(
        options: ListenerOptions,
        prefix: impl FnOnce(&mut Command),
    ) -> Result<Self, Error> {
        if options.timeout.is_zero() || options.timeout > MAX_TIMEOUT {
            return Err(Error::Invalid(
                "listener timeout must be greater than zero and at most 180 seconds",
            ));
        }
        let mut command = Command::new(&options.program);
        prefix(&mut command);
        let mut child = command
            .args(options.args())
            .env("S7_LOG", &options.log)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawning listener")?;
        let stdin = child.stdin.take().context("listener stdin missing")?;
        let stdout = child.stdout.take().context("listener stdout missing")?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            mode: options.mode,
            deadline: options.timeout,
            sent: 0,
            pending: std::collections::VecDeque::with_capacity(MAX_BATCHES),
        })
    }

    /// Send a batch; stream mode permits delivery while another turn is running.
    /// Batch mode closes stdin after its single prompt.
    ///
    /// # Errors
    /// Returns an error for oversized input, too many batches, closed stdin or a write timeout.
    pub async fn send_batch(&mut self, batch: &Value) -> Result<(), Error> {
        if self.sent >= MAX_BATCHES {
            return Err(Error::Invalid("listener batch limit reached"));
        }
        let content = serde_json::to_string(batch).context("encoding event batch")?;
        if content.len() > MAX_INPUT_BYTES {
            return Err(Error::Invalid("event batch exceeds 64 KiB"));
        }
        let mut bytes = match self.mode {
            RuntimeMode::Stream => serde_json::to_vec(&json!({"type":"user",
                "message":{"role":"user","content":content}}))?,
            RuntimeMode::Batch { .. } => content.into_bytes(),
        };
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(Error::Invalid("encoded input exceeds 64 KiB"));
        }
        bytes.push(b'\n');
        let stdin = self.stdin.as_mut().context("listener stdin is closed")?;
        timeout(self.deadline, stdin.write_all(&bytes))
            .await
            .context("listener input timed out")?
            .context("writing listener input")?;
        self.sent += 1;
        self.pending.push_back(Instant::now());
        if matches!(self.mode, RuntimeMode::Batch { .. }) {
            self.stdin.take();
        }
        Ok(())
    }

    /// Read events through the next result, retaining tool calls and reported cost.
    ///
    /// # Errors
    /// Returns an error on EOF, invalid JSON, size/event limits or the turn deadline.
    pub async fn next_turn(&mut self) -> Result<Turn, Error> {
        let started = *self
            .pending
            .front()
            .context("no listener batch is pending")?;
        let remaining = self
            .deadline
            .checked_sub(started.elapsed())
            .context("listener turn timed out")?;
        let turn = timeout(remaining, async {
            let mut tool_calls = Vec::new();
            let mut total_bytes = 0;
            for _ in 0..MAX_EVENTS {
                let mut bytes = Vec::new();
                // Take caps read_until's allocation before any JSON parsing.
                let n = (&mut self.stdout)
                    .take((MAX_OUTPUT_BYTES + 1) as u64)
                    .read_until(b'\n', &mut bytes)
                    .await
                    .context("reading listener output")?;
                total_bytes += n;
                if n == 0 {
                    bail!("listener closed stdout before a result");
                }
                if total_bytes > MAX_OUTPUT_BYTES {
                    bail!("listener turn exceeds 1 MiB");
                }
                let event: Value =
                    serde_json::from_slice(&bytes).context("parsing listener event")?;
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
                    return Ok(Turn {
                        total_cost_usd: event["total_cost_usd"].as_f64(),
                        result: event,
                        tool_calls,
                        latency: started.elapsed(),
                    });
                }
            }
            bail!("listener result missing after 1024 events")
        })
        .await
        .context("listener turn timed out")??;
        self.pending.pop_front();
        Ok(turn)
    }

    /// Close stdin and reap the child, killing it if its exit deadline expires.
    ///
    /// # Errors
    /// Returns an error on a failed wait, timeout or nonzero process exit.
    pub async fn close(mut self) -> Result<ExitStatus, Error> {
        self.stdin.take();
        let status = match timeout(self.deadline, self.child.wait()).await {
            Ok(status) => status.context("waiting for listener exit")?,
            Err(_) => {
                timeout(self.deadline, self.child.kill())
                    .await
                    .context("killing listener timed out")?
                    .context("killing listener")?;
                return Err(Error::Invalid(
                    "listener exit timed out; child killed and reaped",
                ));
            }
        };
        if !status.success() {
            return Err(anyhow::anyhow!("listener exited with {status}").into());
        }
        Ok(status)
    }
}

/// Read a bounded JSONL tool log. A missing file means no tools were called.
///
/// # Errors
/// Returns an error on I/O failure, malformed JSON or a log larger than 1 MiB.
pub async fn read_calls(path: &Path) -> Result<Vec<ToolCall>, Error> {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context("opening fake MCP call log")
                .into());
        }
    };
    let mut bytes = Vec::new();
    file.take((MAX_OUTPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .context("reading fake MCP call log")?;
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(Error::Invalid("fake MCP call log exceeds 1 MiB"));
    }
    let text = std::str::from_utf8(&bytes).context("fake MCP call log is not UTF-8")?;
    if text.lines().count() > MAX_EVENTS {
        return Err(Error::Invalid("fake MCP call log exceeds 1024 calls"));
    }
    text.lines()
        .map(|line| serde_json::from_str(line).context("parsing fake MCP call log line"))
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(Into::into)
}
