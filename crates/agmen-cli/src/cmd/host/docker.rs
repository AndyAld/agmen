//! Docker and Compose, each call bounded and passed as argv, never a shell.

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::{Child, Command};

use crate::output::{CommandFailed, clean, println_flush};

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Last lines of docker's stderr kept on a failed compose command.
const ERROR_LINE_CAP: usize = 20;
/// Those lines, joined, stay within this many bytes.
const ERROR_BYTE_CAP: usize = 4 * 1024;
/// A log line longer than this is cut and marked.
const LOG_LINE_MAX: usize = 4 * 1024;
const TRUNCATED: &str = " (truncated)";
/// Env values shorter than this are left in a docker error. A port, `url`,
/// or the host name would otherwise vanish from the line that explains it.
const REDACT_MIN: usize = 8;
const SHORT_LIMIT: Duration = Duration::from_secs(30);
const UP_LIMIT: Duration = Duration::from_secs(240);
const KILL_LIMIT: Duration = Duration::from_secs(5);

const NOT_INSTALLED: &str = "Docker is not installed. Install it from https://docs.docker.com/get-docker/ and run agmen host up again.";
const NOT_RUNNING: &str = "Docker is installed but its engine is not running: start Docker Desktop or the docker service, then run agmen host up again.";
const NO_COMPOSE: &str = "Docker Compose is not installed. Install it from https://docs.docker.com/compose/install/ and run agmen host up again.";

#[derive(Debug)]
struct TimedOut;

impl fmt::Display for TimedOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("docker timed out")
    }
}

impl std::error::Error for TimedOut {}

#[derive(Debug)]
enum DockerCheck {
    Ready,
    NotFound,
    Failed,
}

/// What [`captured`] does with stdout past [`MAX_OUTPUT_BYTES`].
#[derive(Debug, Clone, Copy)]
enum Overflow {
    /// Fail: the output is not the short answer the caller expects.
    Fail,
    /// Keep the last [`MAX_OUTPUT_BYTES`]: a log, where the newest lines
    /// matter, or output the caller drops.
    KeepEnd,
}

struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// One service line from `docker compose ps --format json`.
#[derive(Debug, Deserialize)]
pub(crate) struct ServiceState {
    #[serde(rename = "Service")]
    pub(crate) service: String,
    #[serde(rename = "State")]
    pub(crate) state: String,
    #[serde(rename = "Health", default)]
    pub(crate) health: String,
}

/// `docker compose` pointed at one host directory.
#[derive(Debug)]
pub(crate) struct Compose {
    dir: PathBuf,
}

impl Compose {
    /// Compose commands for the project in `dir`.
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// `up -d --wait --wait-timeout 180`, then `services`, bounded at 240 seconds.
    ///
    /// # Errors
    ///
    /// When docker cannot be started, times out, or exits non-zero. The
    /// message names the exit code. A non-zero exit also carries the last 20
    /// lines of stderr, at most 4 KiB, for the caller to print under it.
    /// Values from `.env` are not included.
    pub(crate) async fn up(&self, services: &[&str]) -> Result<()> {
        let mut args = vec!["up", "-d", "--wait", "--wait-timeout", "180"];
        args.extend(services);
        self.succeeded(&args, UP_LIMIT, "up").await
    }

    /// `stop`, bounded at 30 seconds.
    ///
    /// # Errors
    ///
    /// As [`Self::up`].
    pub(crate) async fn stop(&self) -> Result<()> {
        self.succeeded(&["stop"], SHORT_LIMIT, "stop").await
    }

    /// `restart <service>`, bounded at 30 seconds.
    ///
    /// # Errors
    ///
    /// As [`Self::up`].
    #[expect(dead_code, reason = "tunnel setup restarts the hub")]
    pub(crate) async fn restart(&self, service: &str) -> Result<()> {
        self.succeeded(&["restart", service], SHORT_LIMIT, "restart")
            .await
    }

    /// `ps --all --format json`, one object per line, bounded at 30 seconds.
    ///
    /// # Errors
    ///
    /// When docker fails or a line is not one JSON object.
    pub(crate) async fn ps(&self) -> Result<Vec<ServiceState>> {
        let output = self
            .output(&["ps", "--all", "--format", "json"], "ps")
            .await?;
        let mut states = Vec::new();
        for line in output.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let state = serde_json::from_str::<ServiceState>(line).map_err(|_| {
                let end = line.floor_char_boundary(80.min(line.len()));
                anyhow::anyhow!(
                    "docker compose ps returned a line that is not JSON: {}",
                    &line[..end]
                )
            })?;
            states.push(state);
        }
        Ok(states)
    }

    /// The published address of `port` on `service`, bounded at 30 seconds.
    ///
    /// # Errors
    ///
    /// When docker fails or the address is not a socket address.
    #[expect(dead_code, reason = "tunnel setup reads the published ngrok port")]
    pub(crate) async fn port(&self, service: &str, port: u16) -> Result<SocketAddr> {
        let text = port.to_string();
        let output = self.output(&["port", service, &text], "port").await?;
        let line = output
            .lines()
            .find(|line| !line.trim().is_empty())
            .context("docker compose port returned nothing")?;
        let end = line.floor_char_boundary(80.min(line.len()));
        line[..end].trim().parse().with_context(|| {
            format!(
                "docker compose port returned an address that is not a socket address: {}",
                line[..end].trim()
            )
        })
    }

    /// `logs --no-color <service>`, bounded at 30 seconds. Past 1 MiB it keeps
    /// the end, since the newest lines are the ones callers read, and it
    /// replaces bytes that are not UTF-8.
    ///
    /// # Errors
    ///
    /// When docker fails or times out.
    pub(crate) async fn logs_text(&self, service: &str) -> Result<String> {
        let bytes = self
            .stdout(&["logs", "--no-color", service], "logs", Overflow::KeepEnd)
            .await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `logs`, each line cleaned and cut at 4 KiB. Not bounded: `--follow`
    /// stays open until docker exits, Ctrl-C, or the reader's pipe closes.
    ///
    /// # Errors
    ///
    /// When docker cannot be started or exits non-zero. A closed pipe or
    /// Ctrl-C while following stops the command without an error.
    pub(crate) async fn logs(&self, service: Option<&str>, follow: bool, tail: u32) -> Result<()> {
        let mut child = self.spawn_logs(service, follow, tail)?;
        let stdout = child.stdout.take().context("docker stdout was not piped")?;
        let stderr = child.stderr.take().context("docker stderr was not piped")?;
        let copying = async {
            tokio::try_join!(copy_cleaned(stdout, false), copy_cleaned(stderr, true))?;
            Ok(())
        };
        let outcome: Result<()> = if follow {
            tokio::select! {
                result = copying => result,
                () = interrupt() => {
                    stop_child(&mut child).await;
                    return Ok(());
                }
            }
        } else {
            copying.await
        };
        if let Err(err) = outcome {
            stop_child(&mut child).await;
            if err.is::<Closed>() {
                return Ok(());
            }
            return Err(err);
        }
        let status = child.wait().await.context("waiting for docker")?;
        if status.success() {
            Ok(())
        } else {
            Err(CommandFailed {
                summary: format!("docker compose logs exited {}", exit_code(status)),
                detail: String::new(),
            }
            .into())
        }
    }

    fn spawn_logs(&self, service: Option<&str>, follow: bool, tail: u32) -> Result<Child> {
        let tail_text = tail.to_string();
        let mut cmd = self.command()?;
        cmd.arg("logs");
        if follow {
            cmd.arg("--follow");
        }
        cmd.arg("--tail").arg(&tail_text);
        if let Some(service) = service {
            cmd.arg(service);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd.spawn().context("starting docker")
    }

    fn failed(&self, verb: &str, output: &Captured) -> CommandFailed {
        CommandFailed {
            summary: format!("docker compose {verb} exited {}", exit_code(output.status)),
            detail: stderr_detail(&self.dir, &output.stderr),
        }
    }

    fn command(&self) -> Result<Command> {
        let program = find_docker().context(NOT_INSTALLED)?;
        let mut cmd = docker_command(&program);
        cmd.arg("compose")
            .arg("--project-directory")
            .arg(&self.dir)
            .arg("-f")
            .arg(self.dir.join("compose.yaml"));
        Ok(cmd)
    }

    async fn output(&self, args: &[&str], verb: &str) -> Result<String> {
        let bytes = self.stdout(args, verb, Overflow::Fail).await?;
        String::from_utf8(bytes).context("docker output is not UTF-8")
    }

    async fn stdout(&self, args: &[&str], verb: &str, overflow: Overflow) -> Result<Vec<u8>> {
        match self.captured(args, SHORT_LIMIT, overflow).await {
            Ok(output) if output.status.success() => Ok(output.stdout),
            Ok(output) => Err(self.failed(verb, &output).into()),
            Err(err) => Err(map_timeout(verb, err)),
        }
    }

    async fn succeeded(&self, args: &[&str], limit: Duration, verb: &str) -> Result<()> {
        // `up` prints image pull progress, so never fail on its size. On failure
        // the stderr tail is attached; on success it is dropped.
        match self.captured(args, limit, Overflow::KeepEnd).await {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(self.failed(verb, &output).into()),
            Err(err) => Err(map_timeout(verb, err)),
        }
    }

    async fn captured(
        &self,
        args: &[&str],
        limit: Duration,
        overflow: Overflow,
    ) -> Result<Captured> {
        let mut cmd = self.command()?;
        cmd.args(args);
        captured(cmd, limit, overflow).await
    }
}

/// The first executable `docker` in an absolute `PATH` entry. A relative or
/// empty entry means the current directory, which can be a repository that
/// ships its own `docker`; never run that (`setup` filters `PATH` the same
/// way for the agent programs).
fn find_docker() -> Option<PathBuf> {
    // shortcut: Windows looks for docker.exe only; add a Windows host test before relying on it
    let file = if cfg!(windows) {
        "docker.exe"
    } else {
        "docker"
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(file))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    // A missing or inaccessible candidate cannot be used as a program.
    std::fs::metadata(path).is_ok_and(|metadata| {
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        true
    })
}

/// `program` without the shell's compose settings. `COMPOSE_PROJECT_NAME`
/// outranks the project name in compose.yaml, so it could point `up` and
/// `stop` at someone else's stack. A shell value also outranks the same name
/// in `.env`, so an `AGMEN_` variable could replace a generated secret or the
/// public URL. `.env` is the one home for both.
fn docker_command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    for name in [
        "COMPOSE_PROJECT_NAME",
        "COMPOSE_PROFILES",
        "COMPOSE_ENV_FILES",
    ] {
        cmd.env_remove(name);
    }
    for (name, _value) in std::env::vars_os() {
        let agmen = name
            .to_str()
            .and_then(|name| name.get(..6))
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("AGMEN_"));
        if agmen {
            cmd.env_remove(name);
        }
    }
    cmd
}

/// Confirms `docker` and `docker compose` answer, within 30 seconds each.
///
/// # Errors
///
/// A fixed message for a missing docker, a stopped engine, or a missing
/// Compose plugin. Other failures propagate. Docker's own output is not included.
pub(crate) async fn check_docker() -> Result<()> {
    match docker_ok(&["version", "--format", "{{.Server.Version}}"]).await? {
        DockerCheck::NotFound => bail!("{NOT_INSTALLED}"),
        DockerCheck::Failed => bail!("{NOT_RUNNING}"),
        DockerCheck::Ready => {}
    }
    match docker_ok(&["compose", "version"]).await? {
        DockerCheck::Ready => Ok(()),
        DockerCheck::NotFound | DockerCheck::Failed => bail!("{NO_COMPOSE}"),
    }
}

async fn docker_ok(args: &[&str]) -> Result<DockerCheck> {
    let Some(program) = find_docker() else {
        return Ok(DockerCheck::NotFound);
    };
    let mut cmd = docker_command(&program);
    cmd.args(args);
    match captured(cmd, SHORT_LIMIT, Overflow::Fail).await {
        // The file went away between the lookup and the spawn.
        Err(err) if is_not_found(&err) => Ok(DockerCheck::NotFound),
        Err(err) if err.is::<TimedOut>() => Ok(DockerCheck::Failed),
        Err(err) => Err(err),
        Ok(output) if output.status.success() && !output.stdout.trim_ascii().is_empty() => {
            Ok(DockerCheck::Ready)
        }
        Ok(_) => Ok(DockerCheck::Failed),
    }
}

async fn captured(mut cmd: Command, limit: Duration, overflow: Overflow) -> Result<Captured> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("starting docker")?;
    let stdout = child.stdout.take().context("docker stdout was not piped")?;
    let stderr = child.stderr.take().context("docker stderr was not piped")?;
    let result = tokio::time::timeout(limit, async {
        let (status, stdout, stderr) = tokio::try_join!(
            async { child.wait().await.context("waiting for docker") },
            async {
                match overflow {
                    Overflow::Fail => read_pipe(stdout).await,
                    Overflow::KeepEnd => read_end(stdout).await,
                }
            },
            // A long `up` writes pull progress here: never fail on its size.
            // Only a non-zero exit keeps a tail of this, and never a value from `.env`.
            read_end(stderr),
        )?;
        Ok(Captured {
            status,
            stdout,
            stderr,
        })
    })
    .await;
    match result {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) => {
            stop_child(&mut child).await;
            Err(err)
        }
        Err(_elapsed) => {
            stop_child(&mut child).await;
            Err(TimedOut.into())
        }
    }
}

async fn read_pipe(pipe: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.take(MAX_OUTPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .context("reading docker output")?;
    if bytes.len() > MAX_OUTPUT_BYTES {
        bail!("docker output exceeds the 1 MiB limit");
    }
    Ok(bytes)
}

/// Reads `pipe` to its end and returns the last [`MAX_OUTPUT_BYTES`] of it,
/// holding at most twice that.
async fn read_end(mut pipe: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut kept: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        let read = pipe
            .read(&mut chunk)
            .await
            .context("reading docker output")?;
        if read == 0 {
            break;
        }
        kept.extend_from_slice(&chunk[..read]);
        if kept.len() > 2 * MAX_OUTPUT_BYTES {
            kept.drain(..kept.len() - MAX_OUTPUT_BYTES);
        }
    }
    if kept.len() > MAX_OUTPUT_BYTES {
        kept.drain(..kept.len() - MAX_OUTPUT_BYTES);
    }
    Ok(kept)
}

async fn stop_child(child: &mut Child) {
    // kill_on_drop is the fallback if killing or reaping does not finish.
    match tokio::time::timeout(KILL_LIMIT, child.kill()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::debug!(%err, "killing docker failed"),
        Err(err) => tracing::debug!(%err, "killing docker timed out"),
    }
}

fn map_timeout(verb: &str, err: anyhow::Error) -> anyhow::Error {
    if err.is::<TimedOut>() {
        anyhow::anyhow!("docker compose {verb} timed out")
    } else {
        err
    }
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

fn exit_code(status: ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| "signal".to_owned(), |code| code.to_string())
}

/// Stdout or stderr has no reader left.
#[derive(Debug)]
struct Closed;

impl fmt::Display for Closed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the log pipe is closed")
    }
}

impl std::error::Error for Closed {}

async fn interrupt() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

async fn copy_cleaned(mut pipe: impl AsyncRead + Unpin, to_stderr: bool) -> Result<()> {
    let mut buf = [0u8; 8 * 1024];
    let mut line = Vec::new();
    let mut over = false;
    loop {
        let read = pipe.read(&mut buf).await.context("reading docker logs")?;
        if read == 0 {
            break;
        }
        for &byte in &buf[..read] {
            if byte == b'\n' {
                emit_log_line(&line, over, to_stderr)?;
                line.clear();
                over = false;
                continue;
            }
            if line.len() >= LOG_LINE_MAX {
                over = true;
                continue;
            }
            line.push(byte);
        }
    }
    if !line.is_empty() || over {
        emit_log_line(&line, over, to_stderr)?;
    }
    Ok(())
}

fn emit_log_line(raw: &[u8], over: bool, to_stderr: bool) -> Result<()> {
    let shown = format_log_line(raw, over);
    write_log_line(&shown, to_stderr).map_err(|err| match err.kind() {
        std::io::ErrorKind::BrokenPipe => anyhow::Error::new(Closed),
        _ => anyhow::Error::new(err).context("writing logs"),
    })
}

fn format_log_line(raw: &[u8], over: bool) -> String {
    let text = String::from_utf8_lossy(raw);
    let cleaned = clean(&text);
    if !over {
        return cleaned.into_owned();
    }
    let budget = LOG_LINE_MAX.saturating_sub(TRUNCATED.len());
    let end = cleaned.floor_char_boundary(budget.min(cleaned.len()));
    format!("{}{TRUNCATED}", &cleaned[..end])
}

fn write_log_line(line: &str, to_stderr: bool) -> std::io::Result<()> {
    if to_stderr {
        use std::io::Write as _;
        let mut out = std::io::stderr().lock();
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()
    } else {
        println_flush(line)
    }
}

/// The last [`ERROR_LINE_CAP`] lines of `bytes`, each cleaned, cut to their
/// last [`ERROR_BYTE_CAP`] bytes. A line that is itself a line of `.env`, and
/// any value from that file of at least [`REDACT_MIN`] bytes, is left out.
///
/// The match runs on the cleaned text and the byte cap comes last, so what is
/// checked is what is printed: a control character inside a value cannot hide
/// it from the match and then vanish, and the cap cannot cut one in half.
fn stderr_detail(dir: &Path, bytes: &[u8]) -> String {
    // shortcut: exact match only. A value that docker quotes, re-encodes, wraps over
    // two lines or splits with an escape sequence (its printable part stays), and
    // a value under REDACT_MIN bytes, still print. Revisit if docker is seen
    // echoing env values.
    let (exact, values) = env_redaction(dir);
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(ERROR_LINE_CAP);
    let mut kept: Vec<String> = Vec::new();
    for line in &lines[start..] {
        let cleaned = clean(line);
        if exact.iter().any(|item| *item == *cleaned) {
            continue;
        }
        kept.push(redact_line(&cleaned, &values));
    }
    last_bytes(&kept.join("\n"), ERROR_BYTE_CAP).to_owned()
}

fn last_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn redact_line(line: &str, values: &[String]) -> String {
    let mut ordered: Vec<&str> = values.iter().map(String::as_str).collect();
    ordered.sort_by_key(|value| std::cmp::Reverse(value.len()));
    let mut out = line.to_owned();
    for value in ordered {
        if out.contains(value) {
            out = out.replace(value, "");
        }
    }
    out
}

fn env_redaction(dir: &Path) -> (Vec<String>, Vec<String>) {
    let Ok(bytes) = std::fs::read(dir.join(".env")) else {
        return (Vec::new(), Vec::new());
    };
    // Same ceiling as `HostEnv::load`. A larger file is not one we wrote.
    if bytes.len() > 64 * 1024 {
        return (Vec::new(), Vec::new());
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return (Vec::new(), Vec::new());
    };
    let mut exact = Vec::new();
    let mut values = Vec::new();
    for raw in text.lines() {
        // Cleaned like the stderr text it is matched against in `stderr_detail`.
        let line = clean(raw);
        exact.push(line.to_string());
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.starts_with('#') || value.len() < REDACT_MIN {
            continue;
        }
        values.push(value.to_owned());
    }
    (exact, values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_end_keeps_only_the_last_mebibyte() {
        // More than twice the cap, so the trim inside the loop runs as well as the last one.
        let data: Vec<u8> = (0..5 * MAX_OUTPUT_BYTES + 123)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        let kept = read_end(data.as_slice()).await.unwrap();
        assert_eq!(kept.len(), MAX_OUTPUT_BYTES);
        assert!(kept.as_slice() == &data[data.len() - MAX_OUTPUT_BYTES..]);

        let short = read_end(&b"abc"[..]).await.unwrap();
        assert_eq!(short, b"abc");
        assert!(read_end(&b""[..]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn read_pipe_refuses_more_than_a_mebibyte() {
        let data = vec![b'x'; MAX_OUTPUT_BYTES + 1];
        assert!(read_pipe(data.as_slice()).await.is_err());
        let at_cap = vec![b'x'; MAX_OUTPUT_BYTES];
        assert_eq!(
            read_pipe(at_cap.as_slice()).await.unwrap().len(),
            MAX_OUTPUT_BYTES
        );
    }

    /// 64 hex characters, as `host up` generates.
    const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn host_dir_with_secret() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            format!("AGMEN_POSTGRES_PASSWORD={SECRET}\nAGMEN_HOST_PORT=7080\n"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn stderr_detail_hides_a_secret_split_by_a_character_cleaning_drops() {
        let dir = host_dir_with_secret();
        let (head, tail) = SECRET.split_at(32);
        for gap in ["\u{7}", "\r", "\0", "\u{202e}"] {
            let raw = format!("bad value {head}{gap}{tail} for key\n");
            let shown = stderr_detail(dir.path(), raw.as_bytes());
            assert!(!shown.contains(SECRET), "{gap:?}: {shown}");
            assert!(shown.starts_with("bad value "), "{gap:?}: {shown}");
        }
    }

    #[test]
    fn stderr_detail_hides_a_secret_the_byte_cap_would_cut_in_half() {
        let dir = host_dir_with_secret();
        // The last 4 KiB of this line starts 30 characters into the secret.
        let raw = format!("{SECRET}{}\n", "x".repeat(ERROR_BYTE_CAP - 34));
        let shown = stderr_detail(dir.path(), raw.as_bytes());
        assert!(shown.bytes().all(|byte| byte == b'x'), "{shown}");
    }

    #[test]
    fn stderr_detail_matches_env_text_that_holds_a_control_character() {
        // A hand-edited `.env` can hold a tab. The stderr line is cleaned before
        // the match, so the file's text is cleaned too.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "AGMEN_NOTE=a\tb\nAGMEN_TOKEN=abcd\tefgh\n",
        )
        .unwrap();
        let raw = "first\nAGMEN_NOTE=a\tb\nbad token abcd\tefgh here\nlast\n";
        let shown = stderr_detail(dir.path(), raw.as_bytes());
        assert_eq!(shown, "first\nbad token  here\nlast");
    }
}
