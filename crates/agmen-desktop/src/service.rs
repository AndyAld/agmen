//! Consented setup of the bundled per-user background service.

use std::ffi::OsString;
use std::io::{Read as _, Seek as _, Write as _};
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use agmen_client::InstallReceipt;
use agmen_local::{API, HelloAnswer, ReadyAnswer};
use anyhow::{Context as _, Result, ensure};

use crate::backend::{AgmendBackend, BackendError};
use crate::bundle::Bundle;

const TRANSITION: Duration = Duration::from_secs(120);
const MAX_OUTPUT: u64 = 64 * 1024;

pub(crate) type Runner<'a> = dyn FnMut(&Path, &[OsString], Duration) -> Result<Output> + 'a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Check {
    Ready,
    Missing,
    Stopped,
    Skewed {
        running: Option<String>,
        ours: String,
        exe: Option<PathBuf>,
    },
    Elsewhere {
        exe: PathBuf,
    },
    Moved {
        configured: PathBuf,
    },
    TemporaryBundle,
    NoBundle,
}

#[derive(Debug)]
pub(crate) struct Observation {
    pub(crate) check: Check,
    pub(crate) running: bool,
}

impl Check {
    pub(crate) fn action(&self) -> Option<&'static str> {
        match self {
            Self::Missing | Self::Stopped | Self::Moved { .. } => Some("Set up agmend"),
            Self::Skewed { .. } => Some("Switch to this app's agmend"),
            Self::TemporaryBundle => Some("Move to Applications"),
            _ => None,
        }
    }

    pub(crate) fn can_connect(&self) -> bool {
        matches!(self, Self::Ready | Self::Elsewhere { .. } | Self::NoBundle)
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Status {
    state: String,
    program: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}\nCommand: {command}\nLog: {log}")]
pub(crate) struct ServiceError {
    message: String,
    command: String,
    log: String,
}

fn failure(bundle: &Bundle, message: impl ToString) -> ServiceError {
    ServiceError {
        message: message.to_string().chars().take(512).collect(),
        command: command(bundle),
        log: log_path().display().to_string(),
    }
}

pub(crate) fn command(bundle: &Bundle) -> String {
    let path = bundle.agmend.to_string_lossy();
    let escaped = path
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`");
    format!("\"{escaped}\" service install --program \"{escaped}\"")
}

pub(crate) fn log_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("~"))
        .join("Library/Logs/agmen/agmend.log")
}

pub(crate) fn temporary(bundle: &Bundle) -> bool {
    bundle.app.starts_with("/Volumes")
        || bundle
            .app
            .components()
            .any(|part| part.as_os_str() == "AppTranslocation")
}

fn classify(
    bundle: Option<&Bundle>,
    hello: Option<&HelloAnswer>,
    ready: Option<&ReadyAnswer>,
    status: Option<&Status>,
    receipt: Option<&InstallReceipt>,
) -> Check {
    let Some(bundle) = bundle else {
        return Check::NoBundle;
    };
    if let Some(hello) = hello {
        if hello.api != API
            || hello.version.as_deref() != Some(env!("CARGO_PKG_VERSION"))
            || !ready.is_some_and(|ready| ready.ready)
        {
            if temporary(bundle) {
                return Check::TemporaryBundle;
            }
            return Check::Skewed {
                running: hello.version.clone(),
                ours: env!("CARGO_PKG_VERSION").into(),
                exe: hello.exe.clone(),
            };
        }
        return match &hello.exe {
            Some(exe) if exe != &bundle.agmend => Check::Elsewhere { exe: exe.clone() },
            _ => Check::Ready,
        };
    }
    if temporary(bundle) {
        return Check::TemporaryBundle;
    }
    let Some(status) = status else {
        return Check::Missing;
    };
    let Some(program) = &status.program else {
        return Check::Missing;
    };
    if program != &bundle.agmend
        && receipt.is_some_and(|receipt| {
            receipt.version == env!("CARGO_PKG_VERSION") && receipt.owned_paths.contains(program)
        })
    {
        return Check::Elsewhere {
            exe: program.clone(),
        };
    }
    if !program.exists() {
        return Check::Moved {
            configured: program.clone(),
        };
    }
    if program != &bundle.agmend {
        return Check::Skewed {
            running: receipt
                .filter(|receipt| receipt.owned_paths.contains(program))
                .map(|receipt| receipt.version.clone()),
            ours: env!("CARGO_PKG_VERSION").into(),
            exe: Some(program.clone()),
        };
    }
    Check::Stopped
}

/// Reads process identity first, then configured service paths if stopped.
pub(crate) fn check(
    bundle: Option<&Bundle>,
    socket: &Path,
    runner: &mut Runner<'_>,
) -> Result<Observation, ServiceError> {
    let Some(bundle) = bundle else {
        return Ok(Observation {
            check: Check::NoBundle,
            running: false,
        });
    };
    let receipt = agmen_client::read_install_receipt().map_err(|error| failure(bundle, error))?;
    check_with_receipt(bundle, socket, receipt.as_ref(), runner)
}

fn check_with_receipt(
    bundle: &Bundle,
    socket: &Path,
    receipt: Option<&InstallReceipt>,
    runner: &mut Runner<'_>,
) -> Result<Observation, ServiceError> {
    match AgmendBackend::probe(socket) {
        Ok((hello, ready)) => {
            return Ok(Observation {
                check: classify(Some(bundle), Some(&hello), ready.as_ref(), None, receipt),
                running: true,
            });
        }
        Err(BackendError::NotConnected) => {}
        Err(error) => return Err(failure(bundle, error)),
    }
    if temporary(bundle) {
        return Ok(Observation {
            check: Check::TemporaryBundle,
            running: false,
        });
    }
    let status = configured_status(bundle, runner, Duration::from_secs(10))?;
    Ok(Observation {
        check: classify(Some(bundle), None, None, Some(&status), receipt),
        running: false,
    })
}

fn configured_status(
    bundle: &Bundle,
    runner: &mut Runner<'_>,
    timeout: Duration,
) -> Result<Status, ServiceError> {
    let output = runner(
        &bundle.agmend,
        &["service".into(), "status".into(), "--json".into()],
        timeout,
    )
    .map_err(|error| failure(bundle, error))?;
    if !output.status.success() {
        return Err(failure(bundle, String::from_utf8_lossy(&output.stderr)));
    }
    let status: Status =
        serde_json::from_slice(&output.stdout).map_err(|error| failure(bundle, error))?;
    if !matches!(
        status.state.as_str(),
        "not installed" | "running" | "stopped"
    ) || status
        .program
        .as_ref()
        .is_some_and(|path| !path.is_absolute())
        || (status.state == "not installed") != status.program.is_none()
    {
        return Err(failure(
            bundle,
            "Invalid or ambiguous service status; no service changes were made",
        ));
    }
    Ok(status)
}

/// The caller's setup or switch button grants consent; recheck ownership here.
pub(crate) fn install(bundle: &Bundle, runner: &mut Runner<'_>) -> Result<(), ServiceError> {
    if temporary(bundle) {
        return Err(failure(
            bundle,
            "Move to Applications, then relaunch agmen before setting up agmend",
        ));
    }
    let started = Instant::now();
    let receipt = agmen_client::read_install_receipt().map_err(|error| failure(bundle, error))?;
    let socket = agmen_client::agmend_socket().map_err(|error| failure(bundle, error))?;
    let observed = check(Some(bundle), &socket, runner)?;
    let remaining = TRANSITION
        .checked_sub(started.elapsed())
        .ok_or_else(|| failure(bundle, "Service setup exceeded 120 s"))?;
    install_with_timeout(bundle, &observed.check, receipt.as_ref(), runner, remaining)
}

#[cfg(test)]
fn install_checked(
    bundle: &Bundle,
    check: &Check,
    receipt: Option<&InstallReceipt>,
    runner: &mut Runner<'_>,
) -> Result<(), ServiceError> {
    install_with_timeout(bundle, check, receipt, runner, TRANSITION)
}

fn install_with_timeout(
    bundle: &Bundle,
    check: &Check,
    receipt: Option<&InstallReceipt>,
    runner: &mut Runner<'_>,
    timeout: Duration,
) -> Result<(), ServiceError> {
    let started = Instant::now();
    if temporary(bundle) {
        return Err(failure(
            bundle,
            "Move to Applications, then relaunch agmen before setting up agmend",
        ));
    }
    if matches!(
        check,
        Check::Elsewhere { .. } | Check::Ready | Check::NoBundle
    ) {
        return Err(failure(
            bundle,
            "The compatible service is already owned by its current installation; keeping it",
        ));
    }
    if let Check::Skewed { exe: Some(exe), .. } = check
        && receipt.is_some_and(|receipt| {
            receipt.version == env!("CARGO_PKG_VERSION") && receipt.owned_paths.contains(exe)
        })
    {
        return Err(failure(
            bundle,
            "Keeping the same-version service owned by another installation",
        ));
    }
    if let Check::Skewed {
        running: Some(version),
        exe: Some(exe),
        ..
    } = check
        && version == env!("CARGO_PKG_VERSION")
        && exe != &bundle.agmend
    {
        return Err(failure(
            bundle,
            "Keeping the same-version service owned by another installation; retry when its stores are ready",
        ));
    }
    // The process that answered hello may differ from the configured next start.
    let status = configured_status(bundle, runner, timeout.min(Duration::from_secs(10)))?;
    if status.program.as_ref().is_some_and(|program| {
        program != &bundle.agmend
            && receipt.is_some_and(|receipt| {
                receipt.version == env!("CARGO_PKG_VERSION")
                    && receipt.owned_paths.contains(program)
            })
    }) {
        return Err(failure(
            bundle,
            "Keeping the configured same-version service owned by another installation",
        ));
    }
    let remaining = timeout
        .checked_sub(started.elapsed())
        .ok_or_else(|| failure(bundle, "Service setup exceeded 120 s"))?;
    let output = runner(
        &bundle.agmend,
        &[
            "service".into(),
            "install".into(),
            "--program".into(),
            bundle.agmend.as_os_str().into(),
        ],
        remaining,
    )
    .map_err(|error| failure(bundle, error))?;
    if !output.status.success() {
        return Err(failure(bundle, String::from_utf8_lossy(&output.stderr)));
    }
    Ok(())
}

/// Runs a helper with bounded disk output, polling and reaping on timeout.
pub(crate) fn run(program: &Path, args: &[OsString], timeout: Duration) -> Result<Output> {
    run_with_input(program, args, timeout, None, &[], &mut |_| {})
}

/// The same bounded runner, with protected stdin and live stdout for device login.
pub(crate) fn run_with_input(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
    cwd: Option<&Path>,
    input: &[u8],
    progress: &mut dyn FnMut(&[u8]),
) -> Result<Output> {
    ensure!(input.len() <= 8192, "helper stdin exceeds 8 KiB");
    let scratch = tempfile::tempdir().context("creating helper output directory")?;
    let mut stdout = tempfile::tempfile_in(scratch.path()).context("creating helper stdout")?;
    let mut stderr = tempfile::tempfile_in(scratch.path()).context("creating helper stderr")?;
    let mut stdin = tempfile::tempfile_in(scratch.path()).context("creating helper stdin")?;
    stdin.write_all(input)?;
    stdin.rewind()?;
    let mut command = Command::new(program);
    command.args(args).stdin(stdin);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()
        .context("starting bundled helper")?;
    let deadline = Instant::now() + timeout.min(TRANSITION);
    let mut reported = 0;
    let result = (|| -> Result<_> {
        for _ in 0..6001 {
            ensure!(
                stdout.metadata()?.len() <= MAX_OUTPUT && stderr.metadata()?.len() <= MAX_OUTPUT,
                "helper output exceeds 64 KiB"
            );
            let length = stdout.metadata()?.len().min(MAX_OUTPUT) as usize;
            if length != reported {
                let mut bytes = vec![0; length];
                let count = stdout.read_at(&mut bytes, 0)?;
                progress(&bytes[..count]);
                reported = count;
            }
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            ensure!(
                Instant::now() < deadline,
                "helper exceeded {} s deadline",
                timeout.as_secs()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        anyhow::bail!("helper exceeded polling bound")
    })();
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            child.kill().context("stopping failed helper")?;
            child.wait().context("reaping failed helper")?;
            return Err(error);
        }
    };
    stdout.rewind()?;
    stderr.rewind()?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.take(MAX_OUTPUT + 1).read_to_end(&mut out)?;
    stderr.take(MAX_OUTPUT + 1).read_to_end(&mut err)?;
    ensure!(
        out.len() <= MAX_OUTPUT as usize && err.len() <= MAX_OUTPUT as usize,
        "helper output exceeds 64 KiB"
    );
    progress(&out);
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
