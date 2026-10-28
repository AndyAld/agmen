//! Consented setup of the bundled per-user background service.

use std::ffi::OsString;
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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
            Self::Missing | Self::Stopped | Self::Moved { .. } => Some("Start agmend"),
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
    let scratch = tempfile::tempdir().context("creating helper output directory")?;
    let mut stdout = tempfile::tempfile_in(scratch.path()).context("creating helper stdout")?;
    let mut stderr = tempfile::tempfile_in(scratch.path()).context("creating helper stderr")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn()
        .context("starting bundled helper")?;
    let deadline = Instant::now() + timeout.min(TRANSITION);
    let result = (|| -> Result<_> {
        for _ in 0..6001 {
            ensure!(
                stdout.metadata()?.len() <= MAX_OUTPUT && stderr.metadata()?.len() <= MAX_OUTPUT,
                "helper output exceeds 64 KiB"
            );
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
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitChoice {
    Ask,
    Keep,
    Stop,
}

pub(crate) fn quit_choice(owned: bool, remembered: Option<bool>) -> QuitChoice {
    match (owned, remembered) {
        (false, _) | (true, Some(true)) => QuitChoice::Keep,
        (true, Some(false)) => QuitChoice::Stop,
        (true, None) => QuitChoice::Ask,
    }
}

pub(crate) fn finish_quit(
    choice: QuitChoice,
    shutdown: &mut dyn FnMut(Duration) -> Result<()>,
) -> Result<()> {
    if choice == QuitChoice::Stop {
        shutdown(Duration::from_secs(15))?;
    }
    Ok(())
}

fn start_checked(
    bundle: &Bundle,
    observed: &Observation,
    spawn: &mut dyn FnMut(&Bundle) -> Result<()>,
) -> Result<()> {
    if !observed.running
        && !temporary(bundle)
        && matches!(
            observed.check,
            Check::Missing | Check::Stopped | Check::Moved { .. }
        )
    {
        spawn(bundle)?;
    }
    Ok(())
}

fn login_checked(
    bundle: &Bundle,
    enabled: bool,
    check: &Check,
    receipt: Option<&InstallReceipt>,
    runner: &mut Runner<'_>,
) -> Result<(), ServiceError> {
    if temporary(bundle) {
        return Err(failure(bundle, "Move to Applications, then relaunch agmen"));
    }
    if !enabled && matches!(check, Check::Elsewhere { .. } | Check::Skewed { .. }) {
        return Err(failure(
            bundle,
            "Keeping the daemon owned by another installation",
        ));
    }
    if enabled && *check != Check::Ready {
        return install_with_timeout(bundle, check, receipt, runner, TRANSITION);
    }
    let status = configured_status(bundle, runner, Duration::from_secs(10))?;
    if status
        .program
        .as_ref()
        .is_some_and(|path| path != &bundle.agmend)
    {
        return Err(failure(
            bundle,
            "Keeping the login service owned by another installation",
        ));
    }
    if !enabled && status.program.is_none() {
        return Ok(());
    }
    let mut args: Vec<OsString> = vec![
        "service".into(),
        if enabled { "install" } else { "uninstall" }.into(),
    ];
    if enabled {
        args.extend(["--program".into(), bundle.agmend.as_os_str().into()]);
    }
    let output =
        runner(&bundle.agmend, &args, TRANSITION).map_err(|error| failure(bundle, error))?;
    if !output.status.success() {
        return Err(failure(bundle, String::from_utf8_lossy(&output.stderr)));
    }
    Ok(())
}

/// The Settings toggle is the consent to install or remove this bundle's login item.
pub(crate) fn login(
    bundle: &Bundle,
    enabled: bool,
    runner: &mut Runner<'_>,
) -> Result<(), ServiceError> {
    let socket = agmen_client::agmend_socket().map_err(|error| failure(bundle, error))?;
    let receipt = agmen_client::read_install_receipt().map_err(|error| failure(bundle, error))?;
    let observed = check_with_receipt(bundle, &socket, receipt.as_ref(), runner)?;
    login_checked(bundle, enabled, &observed.check, receipt.as_ref(), runner)
}

/// Starts the bundle helper without registering a login service. The daemon's
/// socket lock arbitrates simultaneous launches, including another channel.
pub(crate) fn start(
    bundle: &Bundle,
    socket: &Path,
    runner: &mut Runner<'_>,
) -> Result<Observation, ServiceError> {
    let observed = check(Some(bundle), socket, runner)?;
    let mut started = false;
    start_checked(bundle, &observed, &mut |bundle| {
        spawn_detached(bundle, &log_path())?;
        started = true;
        Ok(())
    })
    .map_err(|error| failure(bundle, error))?;
    if !started {
        return Ok(observed);
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    for _ in 0..150 {
        match AgmendBackend::probe(socket) {
            Ok((hello, ready)) => {
                return Ok(Observation {
                    check: classify(Some(bundle), Some(&hello), ready.as_ref(), None, None),
                    running: true,
                });
            }
            Err(BackendError::NotConnected) => {}
            Err(error) => return Err(failure(bundle, error)),
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(failure(
        bundle,
        "agmend did not start within 15 s; check the log",
    ))
}

fn spawn_detached(bundle: &Bundle, log: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).context("creating agmend log folder")?;
    }
    let output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .context("opening agmend log")?;
    let mut child = Command::new(&bundle.agmend)
        .process_group(0)
        .current_dir(&bundle.app)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .spawn()
        .context("starting bundled agmend")?;
    // Reap without retaining the app as the daemon's lifetime owner.
    std::thread::Builder::new()
        .name("agmend-reap".into())
        .spawn(move || {
            if let Err(error) = child.wait() {
                eprintln!("agmen-desktop: reaping agmend: {error}");
            }
        })
        .context("starting agmend reaper")?;
    Ok(())
}

/// Requires a compatible process from this bundle and no foreign login item.
pub(crate) fn owns_running(
    bundle: &Bundle,
    socket: &Path,
    runner: &mut Runner<'_>,
) -> Result<bool> {
    let (hello, ready) = match AgmendBackend::probe(socket) {
        Ok(identity) => identity,
        Err(BackendError::NotConnected) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if hello.exe.as_ref() != Some(&bundle.agmend)
        || classify(Some(bundle), Some(&hello), ready.as_ref(), None, None) != Check::Ready
    {
        return Ok(false);
    }
    let status = configured_status(bundle, runner, Duration::from_secs(10))?;
    Ok(status
        .program
        .as_ref()
        .is_none_or(|path| path == &bundle.agmend))
}

pub(crate) fn shutdown(bundle: &Bundle, socket: &Path, bound: Duration) -> Result<()> {
    // Recheck on the connection that receives shutdown, so a replacement daemon
    // from another channel is never stopped after a stale UI decision.
    AgmendBackend::shutdown_owned(socket, &bundle.agmend, bound).map_err(anyhow::Error::new)
}
