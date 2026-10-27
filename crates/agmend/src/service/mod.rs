//! User-service rendering and management with injected paths and processes.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};

// shortcut: Unix-only service; revisit when S6 Windows support is approved.
#[cfg(any(target_os = "macos", test))]
mod launchd;
#[cfg(any(target_os = "linux", test))]
mod systemd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServiceAction {
    Install,
    Uninstall,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServicePaths {
    home: PathBuf,
    executable: PathBuf,
    uid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Unit {
    path: PathBuf,
    contents: String,
}

const MAX_UNIT_BYTES: usize = 64 * 1024;

fn executable(paths: &ServicePaths) -> Result<&str> {
    validate_path(&paths.home).context("invalid service home path")?;
    validate_path(&paths.executable).context("invalid service executable path")
}

fn validate_path(path: &Path) -> Result<&str> {
    ensure!(path.is_absolute(), "path must be absolute");
    ensure!(
        path.as_os_str().len() <= MAX_UNIT_BYTES,
        "path exceeds 64 KiB"
    );
    let text = path.to_str().context("path must be UTF-8")?;
    ensure!(
        !text
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '\u{fffe}' | '\u{ffff}')),
        "path contains a control character or an invalid XML character"
    );
    Ok(text)
}

fn append(contents: &mut String, text: &str) -> Result<()> {
    ensure!(
        text.len() <= MAX_UNIT_BYTES - contents.len(),
        "rendered service unit exceeds 64 KiB"
    );
    contents.push_str(text);
    Ok(())
}

#[derive(Debug)]
struct ProcessResult {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

type Runner<'a> = dyn FnMut(&[std::ffi::OsString]) -> Result<ProcessResult> + 'a;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    #[cfg(any(target_os = "macos", test))]
    Launchd,
    #[cfg(any(target_os = "linux", test))]
    Systemd,
}

pub(super) fn execute(action: ServiceAction) -> Result<String> {
    let paths = ServicePaths {
        home: std::env::home_dir().context("locating service home")?,
        executable: std::env::current_exe().context("locating agmend executable")?,
        uid: rustix::process::getuid().as_raw(),
    };
    manage(action, &paths, &mut run_process)
}

fn manage(action: ServiceAction, paths: &ServicePaths, run: &mut Runner<'_>) -> Result<String> {
    // shortcut: Unix-only service; revisit when S6 Windows support is approved.
    #[cfg(target_os = "macos")]
    let backend = Backend::Launchd;
    #[cfg(target_os = "linux")]
    let backend = Backend::Systemd;
    manage_backend(backend, action, paths, run)
}

fn manage_backend(
    backend: Backend,
    action: ServiceAction,
    paths: &ServicePaths,
    run: &mut Runner<'_>,
) -> Result<String> {
    let unit = match backend {
        #[cfg(any(target_os = "macos", test))]
        Backend::Launchd => launchd::render(paths)?,
        #[cfg(any(target_os = "linux", test))]
        Backend::Systemd => systemd::render(paths)?,
    };
    if action == ServiceAction::Install {
        #[cfg(any(target_os = "macos", test))]
        if backend == Backend::Launchd {
            private_directory(&paths.home.join("Library/Logs/agmen"))?;
        }
        write_unit(&unit).context("installing service unit")?;
    }
    match backend {
        #[cfg(any(target_os = "macos", test))]
        Backend::Launchd => launchd::manage(action, paths, &unit, run),
        #[cfg(any(target_os = "linux", test))]
        Backend::Systemd => systemd::manage(action, &unit, run),
    }
}

#[cfg(any(target_os = "macos", test))]
fn private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .context("creating private service directory")?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .context("setting private service directory permissions")
}

fn write_unit(unit: &Unit) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let parent = unit.path.parent().context("unit has no parent directory")?;
    std::fs::create_dir_all(parent).context("creating service unit directory")?;
    let temporary = parent.join(format!(
        ".agmend-{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .context("creating private temporary service unit")?;
    let written = (|| -> Result<()> {
        file.write_all(unit.contents.as_bytes())
            .context("writing service unit")?;
        file.sync_all().context("syncing service unit")?;
        std::fs::rename(&temporary, &unit.path).context("replacing service unit")
    })();
    if written.is_err() {
        std::fs::remove_file(&temporary).context("cleaning up temporary service unit")?;
    }
    written
}

fn remove_unit(unit: &Unit) -> Result<()> {
    match std::fs::remove_file(&unit.path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("removing service unit"),
    }
}

fn checked(result: &ProcessResult, operation: &str) -> Result<()> {
    ensure!(
        result.code == Some(0),
        "{operation} failed (exit {:?}, {} stderr bytes)",
        result.code,
        result.stderr.len()
    );
    Ok(())
}

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

fn run_process(argv: &[std::ffi::OsString]) -> Result<ProcessResult> {
    let (program, args) = argv
        .split_first()
        .context("empty service manager command")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting service process runtime")?;
    runtime.block_on(async {
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("starting service manager process")?;
        let stdout = child
            .stdout
            .take()
            .context("missing service process stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("missing service process stderr")?;
        let operation = async {
            let wait = async {
                // 501 polls cover the 10 s deadline at 20 ms intervals.
                for _ in 0..501 {
                    if let Some(status) = child.try_wait().context("polling service process")? {
                        return Ok(status);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                anyhow::bail!("service process exceeded 10 s deadline")
            };
            let (stdout, stderr, status) =
                tokio::try_join!(read_output(stdout), read_output(stderr), wait)?;
            Ok(ProcessResult {
                code: status.code(),
                stdout,
                stderr,
            })
        };
        let result = match tokio::time::timeout(std::time::Duration::from_secs(10), operation).await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("service process exceeded 10 s deadline")),
        };
        if result.is_err() {
            child
                .kill()
                .await
                .context("killing and reaping failed service process")?;
        }
        result
    })
}

async fn read_output(pipe: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>> {
    use tokio::io::AsyncReadExt as _;
    let mut output = Vec::with_capacity(MAX_OUTPUT_BYTES + 1);
    pipe.take((MAX_OUTPUT_BYTES + 1) as u64)
        .read_to_end(&mut output)
        .await
        .context("reading service process output")?;
    ensure!(
        output.len() <= MAX_OUTPUT_BYTES,
        "service process output exceeds 64 KiB"
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn service_render_refuses_invalid_paths() {
        for executable in [
            "bin/agmend".to_owned(),
            "/bin/ag\0mend".to_owned(),
            "/bin/ag\nmend".to_owned(),
            "/bin/ag\rmend".to_owned(),
            format!("/{}", "a".repeat(64 * 1024)),
        ] {
            let paths = ServicePaths {
                home: PathBuf::from("/test/home"),
                executable: PathBuf::from(executable),
                uid: 501,
            };
            assert!(launchd::render(&paths).is_err());
            assert!(systemd::render(&paths).is_err());
        }
        let paths = ServicePaths {
            home: PathBuf::from("/test/home"),
            executable: PathBuf::from(format!("/{}", "%".repeat(32 * 1024))),
            uid: 501,
        };
        assert!(systemd::render(&paths).is_err());
        let paths = ServicePaths {
            executable: PathBuf::from(format!("/{}", "&".repeat(16 * 1024))),
            ..paths
        };
        assert!(launchd::render(&paths).is_err());
    }

    #[test]
    fn service_render_checks_exact_content_cap() {
        for render in [launchd::render, systemd::render] {
            let mut paths = ServicePaths {
                home: PathBuf::from("/test/home"),
                executable: PathBuf::from("/"),
                uid: 501,
            };
            let overhead = render(&paths).unwrap().contents.len();
            paths.executable = PathBuf::from(format!("/{}", "a".repeat(64 * 1024 - overhead)));
            assert_eq!(render(&paths).unwrap().contents.len(), 64 * 1024);
            paths.executable.as_mut_os_string().push("a");
            assert!(render(&paths).is_err());
        }
    }

    #[test]
    fn service_render_refuses_non_utf8_and_control_characters() {
        use std::os::unix::ffi::OsStringExt as _;
        for executable in [
            PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff])),
            PathBuf::from("/opt/a\tb/agmend"),
            PathBuf::from("/opt/\u{ffff}/agmend"),
        ] {
            let paths = ServicePaths {
                home: PathBuf::from("/test/home"),
                executable,
                uid: 501,
            };
            assert!(launchd::render(&paths).is_err());
            assert!(systemd::render(&paths).is_err());
        }
    }
}

#[cfg(test)]
mod management_tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, Instant};

    fn paths(home: &tempfile::TempDir) -> ServicePaths {
        ServicePaths {
            home: home.path().to_owned(),
            executable: "/opt/agmen/agmend".into(),
            uid: 501,
        }
    }
    fn result(code: i32, stdout: &str) -> ProcessResult {
        ProcessResult {
            code: Some(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }
    fn words(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }
    fn exercise(
        backend: Backend,
        action: ServiceAction,
        paths: &ServicePaths,
        responses: Vec<ProcessResult>,
    ) -> (Result<String>, Vec<Vec<OsString>>) {
        let mut responses = responses.into_iter();
        let mut calls = Vec::new();
        let answer = manage_backend(backend, action, paths, &mut |argv| {
            calls.push(argv.to_vec());
            Ok(responses.next().expect("unexpected manager call"))
        });
        assert!(responses.next().is_none(), "unused manager response");
        (answer, calls)
    }
    #[test]
    fn service_install_uses_injected_paths_and_runner() {
        for backend in [Backend::Launchd, Backend::Systemd] {
            let home = tempfile::tempdir().unwrap();
            let paths = paths(&home);
            let responses = match backend {
                Backend::Launchd => vec![result(113, ""), result(0, "")],
                Backend::Systemd => vec![result(0, ""), result(0, ""), result(0, "")],
            };
            let (answer, calls) = exercise(backend, ServiceAction::Install, &paths, responses);
            assert!(answer.is_ok(), "{answer:?}");
            let unit = match backend {
                Backend::Launchd => launchd::render(&paths),
                Backend::Systemd => systemd::render(&paths),
            }
            .unwrap();
            assert_eq!(fs::read_to_string(&unit.path).unwrap(), unit.contents);
            assert_eq!(
                fs::metadata(&unit.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            match backend {
                Backend::Launchd => {
                    assert_eq!(
                        calls,
                        vec![
                            words(&["launchctl", "print", "gui/501/dev.agmen.agmend"]),
                            vec![
                                "launchctl".into(),
                                "bootstrap".into(),
                                "gui/501".into(),
                                unit.path.into_os_string()
                            ]
                        ]
                    );
                    assert_eq!(
                        fs::metadata(home.path().join("Library/Logs/agmen"))
                            .unwrap()
                            .permissions()
                            .mode()
                            & 0o777,
                        0o700
                    );
                }
                Backend::Systemd => assert_eq!(
                    calls,
                    vec![
                        words(&["systemctl", "--user", "daemon-reload"]),
                        words(&["systemctl", "--user", "enable", "agmend.service"]),
                        words(&["systemctl", "--user", "start", "agmend.service"])
                    ]
                ),
            }
        }
    }
    #[test]
    fn service_reinstall_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths(&home);
        for _ in 0..2 {
            let (answer, calls) = exercise(
                Backend::Launchd,
                ServiceAction::Install,
                &paths,
                vec![result(0, "state = running"), result(0, ""), result(0, "")],
            );
            assert!(answer.is_ok());
            assert_eq!(calls.len(), 3);
            assert_eq!(
                calls[1],
                words(&["launchctl", "bootout", "gui/501/dev.agmen.agmend"])
            );
        }
        assert_eq!(
            fs::read_dir(home.path().join("Library/LaunchAgents"))
                .unwrap()
                .count(),
            1
        );
    }
    #[test]
    fn service_uninstall_is_idempotent() {
        for backend in [Backend::Launchd, Backend::Systemd] {
            let home = tempfile::tempdir().unwrap();
            let paths = paths(&home);
            let unit = match backend {
                Backend::Launchd => launchd::render(&paths),
                Backend::Systemd => systemd::render(&paths),
            }
            .unwrap();
            fs::create_dir_all(unit.path.parent().unwrap()).unwrap();
            fs::write(&unit.path, "managed").unwrap();
            let other = unit.path.with_file_name("unrelated");
            fs::write(&other, "keep").unwrap();
            for attempt in 0..2 {
                let responses = match backend {
                    Backend::Launchd => {
                        if attempt == 0 {
                            vec![result(0, ""), result(0, "")]
                        } else {
                            vec![result(113, "")]
                        }
                    }
                    Backend::Systemd => {
                        if attempt == 0 {
                            vec![result(0, ""), result(0, ""), result(0, "")]
                        } else {
                            vec![
                                result(0, ""),
                                result(0, "LoadState=not-found\nActiveState=inactive\n"),
                            ]
                        }
                    }
                };
                let (answer, calls) =
                    exercise(backend, ServiceAction::Uninstall, &paths, responses);
                assert!(answer.is_ok(), "{answer:?}");
                assert!(calls.len() <= 3);
                assert!(!unit.path.exists());
                assert_eq!(fs::read_to_string(&other).unwrap(), "keep");
                if backend == Backend::Systemd && attempt == 0 {
                    assert_eq!(
                        calls,
                        vec![
                            words(&["systemctl", "--user", "disable", "agmend.service"]),
                            words(&["systemctl", "--user", "stop", "agmend.service"]),
                            words(&["systemctl", "--user", "daemon-reload"])
                        ]
                    );
                }
            }
        }
    }
    #[test]
    fn service_uninstall_recovers_after_reload_failure() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths(&home);
        let unit = systemd::render(&paths).unwrap();
        fs::create_dir_all(unit.path.parent().unwrap()).unwrap();
        fs::write(&unit.path, "managed").unwrap();
        let (answer, calls) = exercise(
            Backend::Systemd,
            ServiceAction::Uninstall,
            &paths,
            vec![result(0, ""), result(0, ""), result(1, "bus failure")],
        );
        assert!(answer.is_err());
        assert_eq!(calls.len(), 3);
        assert!(!unit.path.exists());
        let (answer, calls) = exercise(
            Backend::Systemd,
            ServiceAction::Uninstall,
            &paths,
            vec![
                result(0, ""),
                result(0, "LoadState=not-found\nActiveState=inactive\n"),
            ],
        );
        assert!(answer.is_ok());
        assert_eq!(calls[0], words(&["systemctl", "--user", "daemon-reload"]));
    }

    #[test]
    fn service_uninstall_stops_loaded_unit_without_file() {
        let home = tempfile::tempdir().unwrap();
        let (answer, calls) = exercise(
            Backend::Systemd,
            ServiceAction::Uninstall,
            &paths(&home),
            vec![
                result(0, ""),
                result(0, "LoadState=not-found\nActiveState=active\n"),
                result(0, ""),
            ],
        );
        assert!(answer.is_ok());
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[2],
            words(&["systemctl", "--user", "stop", "agmend.service"])
        );
    }

    #[test]
    fn service_status_reports_three_states() {
        let home = tempfile::tempdir().unwrap();
        let paths = paths(&home);
        for (backend, response, expected) in [
            (Backend::Launchd, result(113, ""), "not installed\n"),
            (
                Backend::Launchd,
                result(0, "state = waiting\n"),
                "stopped\n",
            ),
            (
                Backend::Launchd,
                result(0, "\tstate = running\n"),
                "running\n",
            ),
            (
                Backend::Systemd,
                result(0, "LoadState=not-found\nActiveState=inactive\n"),
                "not installed\n",
            ),
            (
                Backend::Systemd,
                result(0, "LoadState=loaded\nActiveState=failed\n"),
                "stopped\n",
            ),
            (
                Backend::Systemd,
                result(0, "LoadState=loaded\nActiveState=active\n"),
                "running\n",
            ),
        ] {
            let (answer, calls) = exercise(backend, ServiceAction::Status, &paths, vec![response]);
            assert_eq!(answer.unwrap(), expected);
            assert_eq!(calls.len(), 1);
            if backend == Backend::Systemd {
                assert_eq!(
                    calls[0],
                    words(&[
                        "systemctl",
                        "--user",
                        "show",
                        "agmend.service",
                        "--property=LoadState",
                        "--property=ActiveState"
                    ])
                );
            }
        }
        let unit = launchd::render(&paths).unwrap();
        fs::create_dir_all(unit.path.parent().unwrap()).unwrap();
        fs::write(&unit.path, "installed").unwrap();
        assert_eq!(
            exercise(
                Backend::Launchd,
                ServiceAction::Status,
                &paths,
                vec![result(113, "")]
            )
            .0
            .unwrap(),
            "stopped\n"
        );
    }
    #[test]
    fn service_failure_keeps_recoverable_unit() {
        for backend in [Backend::Launchd, Backend::Systemd] {
            let home = tempfile::tempdir().unwrap();
            let paths = paths(&home);
            let responses = if backend == Backend::Launchd {
                vec![result(113, ""), result(1, "permission denied")]
            } else {
                vec![result(0, ""), result(0, ""), result(1, "")]
            };
            let error = exercise(backend, ServiceAction::Install, &paths, responses)
                .0
                .unwrap_err();
            assert!(
                format!("{error:#}").contains(if backend == Backend::Launchd {
                    "bootstrap"
                } else {
                    "start"
                })
            );
            let unit = if backend == Backend::Launchd {
                launchd::render(&paths)
            } else {
                systemd::render(&paths)
            }
            .unwrap();
            assert!(unit.path.exists());
            let responses = vec![result(0, ""), result(1, "")];
            assert!(
                exercise(backend, ServiceAction::Uninstall, &paths, responses)
                    .0
                    .is_err()
            );
            assert!(unit.path.exists());
        }
    }
    #[test]
    fn service_status_manager_error_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        for backend in [Backend::Launchd, Backend::Systemd] {
            assert!(
                exercise(
                    backend,
                    ServiceAction::Status,
                    &paths(&home),
                    vec![result(1, "permission/bus failure")]
                )
                .0
                .is_err()
            );
        }
    }
    #[test]
    fn service_runner_timeout_reaps_child() {
        let home = tempfile::tempdir().unwrap();
        let pidfile = home.path().join("pid");
        let script = format!("echo $$ > '{}'; exec /bin/sleep 30", pidfile.display());
        let start = Instant::now();
        let error = run_process(&words(&["/bin/sh", "-c", &script])).unwrap_err();
        assert!(format!("{error:#}").contains("10 s"));
        assert!(start.elapsed() >= Duration::from_secs(10));
        assert!(start.elapsed() < Duration::from_secs(13));
        let pid = fs::read_to_string(pidfile)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert_eq!(
            rustix::process::waitpid(
                Some(rustix::process::Pid::from_raw(pid).unwrap()),
                rustix::process::WaitOptions::NOHANG
            )
            .unwrap_err(),
            rustix::io::Errno::CHILD
        );
    }
    #[test]
    fn service_runner_caps_output() {
        let home = tempfile::tempdir().unwrap();
        for redirect in ["", " >&2"] {
            let script = format!(
                "cd '{}'; /usr/bin/head -c 65537 /dev/zero{redirect}",
                home.path().display()
            );
            assert!(
                format!(
                    "{:#}",
                    run_process(&words(&["/bin/sh", "-c", &script])).unwrap_err()
                )
                .contains("64 KiB")
            );
        }
        let out = run_process(&words(&[
            "/bin/sh",
            "-c",
            "/usr/bin/head -c 65536 /dev/zero; /usr/bin/head -c 65536 /dev/zero >&2",
        ]))
        .unwrap();
        assert_eq!(out.code, Some(0));
        assert_eq!(out.stdout.len(), 65536);
        assert_eq!(out.stderr.len(), 65536);
    }
}
