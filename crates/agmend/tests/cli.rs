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
