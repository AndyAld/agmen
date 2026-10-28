//! agmend's command line: help and rejection of invalid argument lists.

#![cfg(test)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runs agmend with `args`. One that is still running after 10 s took the
/// arguments as a daemon start, so it is killed and the test fails.
fn agmend(args: &[&str]) -> std::process::Output {
    let state = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agmend"))
        .args(args)
        // A run that got past the argument check binds its socket and
        // lock here, not in the person's own state folder (HOME on macOS,
        // XDG_STATE_HOME on Linux).
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!(
                "agmend {args:?} ran as a daemon: {:?}",
                child.wait_with_output()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn unknown_argument_exits_non_zero() {
    let out = agmend(&["service", "frobnicate"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("unknown service action"), "{stderr}");
    assert!(stderr.contains("usage: agmend"), "{stderr}");
}

#[test]
fn help_prints_usage() {
    let out = agmend(&["--help"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.starts_with("usage: agmend"), "{stdout}");
}

#[test]
fn service_install_preserves_explicit_stable_path() {
    // A missing explicit target must reach validation, never the service manager.
    let out = agmend(&[
        "service",
        "install",
        "--program",
        "/missing/agmen/active/agmend",
    ]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("service program")
    );
}

/// Starts the daemon in a temporary state folder, sends it `signal`, and
/// returns its exit status and stderr. The daemon logs when it listens and
/// when it stops; a run that does neither within 10 s is killed.
fn stop_with(signal: &str) -> (std::process::ExitStatus, String) {
    use std::io::{BufRead as _, BufReader, Read as _};

    // The socket lives under HOME, and macOS limits a socket path to 103
    // bytes, which the default temp folder's name already exceeds.
    let state = tempfile::Builder::new()
        .prefix("agd")
        .tempdir_in("/tmp")
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agmend"))
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let (listening, ready) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut seen = String::new();
        let mut line = String::new();
        let mut announced = false;
        // At most 1000 lines, so a noisy child cannot keep this thread alive.
        for _ in 0..1000 {
            line.clear();
            if stderr.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            seen.push_str(&line);
            if !announced && line.contains("agmend is listening") {
                announced = true;
                let _sent = listening.send(());
            }
        }
        let mut rest = String::new();
        let _read = stderr.read_to_string(&mut rest);
        seen.push_str(&rest);
        seen
    });
    if ready.recv_timeout(Duration::from_secs(10)).is_err() {
        child.kill().unwrap();
        child.wait().unwrap();
        panic!("agmend never listened: {:?}", reader.join());
    }
    let sent = Command::new("kill")
        .args([signal, &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("agmend ignored {signal}: {:?}", reader.join());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    (status, reader.join().unwrap())
}

#[test]
fn sigterm_stops_the_daemon_after_its_shutdown_path() {
    let (status, log) = stop_with("-TERM");
    assert!(status.success(), "{status:?}\n{log}");
    assert!(log.contains("agmend stopped"), "{log}");
}

#[test]
fn sigint_stops_the_daemon_after_its_shutdown_path() {
    let (status, log) = stop_with("-INT");
    assert!(status.success(), "{status:?}\n{log}");
    assert!(log.contains("agmend stopped"), "{log}");
}
