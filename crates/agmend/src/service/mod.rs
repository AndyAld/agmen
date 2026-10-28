//! User-service rendering and management with injected paths and processes.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    StatusJson,
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

pub(super) fn execute(action: ServiceAction, program: Option<PathBuf>) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let paths = ServicePaths {
        home: std::env::home_dir().context("locating service home")?,
        executable: program.map_or_else(
            || std::env::current_exe().context("locating agmend executable"),
            Ok,
        )?,
        uid: rustix::process::getuid().as_raw(),
    };
    if action == ServiceAction::Install {
        validate_program(&paths.executable)?;
    }
    let answer = manage(action, &paths, &mut |argv| {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("service transition exceeded 120 s deadline")?;
        run_process_for(argv, remaining.min(Duration::from_secs(10)))
    })?;
    if action == ServiceAction::Install {
        wait_ready(
            &agmen_client::agmend_socket()?,
            env!("CARGO_PKG_VERSION"),
            deadline,
        )?;
    }
    Ok(answer)
}

fn validate_program(path: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    validate_path(path).context("invalid service program")?;
    let metadata = path.metadata().context("reading service program")?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        "service program must be an executable file"
    );
    Ok(path.to_owned())
}

fn configured_program(backend: Backend, unit: &Unit) -> Result<Option<PathBuf>> {
    use std::io::Read as _;
    let metadata = match std::fs::metadata(&unit.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading service configuration metadata"),
    };
    ensure!(
        metadata.is_file(),
        "service configuration must be a regular file"
    );
    ensure!(
        metadata.len() <= MAX_UNIT_BYTES as u64,
        "service configuration exceeds 64 KiB"
    );
    let mut file = match std::fs::File::open(&unit.path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading service configuration"),
    };
    let mut contents = String::new();
    (&mut file)
        .take(MAX_UNIT_BYTES as u64)
        .read_to_string(&mut contents)
        .context("reading service configuration")?;
    ensure!(
        file.metadata()?.len() == contents.len() as u64 && contents.len() as u64 == metadata.len(),
        "service configuration changed while reading"
    );
    let program = match backend {
        #[cfg(any(target_os = "macos", test))]
        Backend::Launchd => launchd::program(&contents)?,
        #[cfg(any(target_os = "linux", test))]
        Backend::Systemd => systemd::program(&contents)?,
    };
    validate_path(&program).context("invalid configured service program")?;
    Ok(Some(program))
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
    let program = if matches!(action, ServiceAction::Install | ServiceAction::StatusJson) {
        configured_program(backend, &unit)?
    } else {
        None
    };
    #[cfg(any(target_os = "linux", test))]
    let active = if backend == Backend::Systemd && action == ServiceAction::Install {
        systemd::active(run)?
    } else {
        false
    };
    if action == ServiceAction::Install {
        #[cfg(any(target_os = "macos", test))]
        if backend == Backend::Launchd {
            private_directory(&paths.home.join("Library/Logs/agmen"))?;
        }
        write_unit(&unit).context("installing service unit")?;
    }
    let operation = if action == ServiceAction::StatusJson {
        ServiceAction::Status
    } else {
        action
    };
    let answer = match backend {
        #[cfg(any(target_os = "macos", test))]
        Backend::Launchd => launchd::manage(operation, paths, &unit, run),
        #[cfg(any(target_os = "linux", test))]
        Backend::Systemd => systemd::manage(operation, &unit, run, active),
    }?;
    if action == ServiceAction::StatusJson {
        return Ok(format!(
            "{}\n",
            serde_json::json!({"state": answer.trim(), "program": program})
        ));
    }
    Ok(answer)
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
    finish_write(written, || std::fs::remove_file(&temporary))
}

fn finish_write(written: Result<()>, cleanup: impl FnOnce() -> std::io::Result<()>) -> Result<()> {
    if let Err(error) = written {
        return Err(match cleanup() {
            Ok(()) => error,
            Err(cleanup_error) => error.context(format!(
                "cleaning up temporary service unit failed: {cleanup_error}"
            )),
        });
    }
    Ok(())
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
        "{operation} failed (exit {:?}, {} stderr bytes); stderr: {}",
        result.code,
        result.stderr.len(),
        stderr_excerpt(&result.stderr)
    );
    Ok(())
}

fn stderr_excerpt(stderr: &[u8]) -> String {
    const MAX_EXCERPT_BYTES: usize = 512;
    let bounded = &stderr[..stderr.len().min(MAX_EXCERPT_BYTES)];
    let mut excerpt = String::new();
    // At most 512 input bytes, including lossy replacements.
    for ch in String::from_utf8_lossy(bounded).chars() {
        if ch.is_control() {
            continue;
        }
        if excerpt.len() + ch.len_utf8() > MAX_EXCERPT_BYTES {
            break;
        }
        excerpt.push(ch);
    }
    excerpt
}

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

fn wait_ready(socket: &Path, version: &str, deadline: Instant) -> Result<()> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .context("service transition exceeded 120 s deadline")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting readiness runtime")?;
    runtime.block_on(async {
        let operation = async {
            let mut last = String::from("daemon has not answered");
            // At most 1200 attempts within the shared 120 s transition deadline.
            for _ in 0..1200 {
                match tokio::time::timeout(Duration::from_secs(2), ready_handshake(socket, version))
                    .await
                {
                    Ok(Ok(())) => return Ok(()),
                    Ok(Err(error)) => last = format!("{error:#}"),
                    Err(_) => last = "hello/ready handshake timed out".to_owned(),
                }
                let pause = Duration::from_millis(100);
                if Instant::now()
                    .checked_add(pause)
                    .is_none_or(|next| next >= deadline)
                {
                    anyhow::bail!("service transition exceeded 120 s deadline: {last}");
                }
                tokio::time::sleep(pause).await;
            }
            anyhow::bail!("service never became ready: {last}")
        };
        tokio::time::timeout(remaining, operation)
            .await
            .context("service transition exceeded 120 s deadline while waiting for hello/ready")?
    })
}

async fn ready_handshake(socket: &Path, version: &str) -> Result<()> {
    use agmen_local::{API, HelloAnswer, Outcome, ReadyAnswer, Request, Response, frame};
    let mut stream = tokio::net::UnixStream::connect(socket)
        .await
        .context("connecting to installed agmend")?;
    ensure!(
        stream.peer_cred()?.uid() == rustix::process::geteuid().as_raw(),
        "installed agmend belongs to another user"
    );
    frame::write_async(
        &mut stream,
        &Request::new(1, "hello", serde_json::json!({"api":API})),
    )
    .await?;
    let hello: Response = frame::read_async(&mut stream).await?;
    ensure!(hello.id == 1, "hello response id mismatch");
    let Outcome::Ok { result } = hello.outcome else {
        anyhow::bail!("installed agmend refused hello");
    };
    let hello: HelloAnswer = serde_json::from_value(result)?;
    ensure!(
        hello.api == API && hello.version.as_deref() == Some(version),
        "installed agmend version {:?} does not match {version}",
        hello.version
    );
    frame::write_async(
        &mut stream,
        &Request::new(2, "ready", serde_json::json!({})),
    )
    .await?;
    let ready: Response = frame::read_async(&mut stream).await?;
    ensure!(ready.id == 2, "ready response id mismatch");
    let Outcome::Ok { result } = ready.outcome else {
        anyhow::bail!("installed agmend refused ready");
    };
    let ready: ReadyAnswer = serde_json::from_value(result)?;
    ensure!(
        ready.ready
            && ready.schemas.contains_key("agmend.db")
            && ready.schemas.values().all(|schema| *schema >= 0),
        "installed agmend stores are not ready"
    );
    Ok(())
}

#[cfg(test)]
fn run_process(argv: &[std::ffi::OsString]) -> Result<ProcessResult> {
    run_process_for(argv, Duration::from_secs(10))
}

fn run_process_for(argv: &[std::ffi::OsString], timeout: Duration) -> Result<ProcessResult> {
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
        let result = match tokio::time::timeout(timeout, operation).await {
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

    #[test]
    fn service_write_failure_preserves_original_when_cleanup_fails() {
        for operation in ["writing service unit", "replacing service unit"] {
            let original = std::io::Error::other("original disk failure");
            let written = Err(original).context(operation);
            let error = finish_write(written, || {
                Err(std::io::Error::other("cleanup permission denied"))
            })
            .unwrap_err();
            assert_eq!(error.root_cause().to_string(), "original disk failure");
            let message = format!("{error:#}");
            assert!(message.contains(operation), "{message}");
            assert!(
                message.contains("cleaning up temporary service unit"),
                "{message}"
            );
            assert!(message.contains("cleanup permission denied"), "{message}");
        }
    }

    #[test]
    fn service_errors_include_bounded_lossy_stderr() {
        for stderr in [
            b"permission denied\n\t\0\x1b".to_vec(),
            [b"bad byte: ".as_slice(), &[0xff], b"\n"].concat(),
            vec![b'x'; 1024],
            vec![0xff; 1024],
            "\u{85}denied\u{9f}".as_bytes().to_vec(),
            [vec![b'x'; 511], "\u{20ac}".as_bytes().to_vec()].concat(),
        ] {
            let response = ProcessResult {
                code: Some(1),
                stdout: Vec::new(),
                stderr,
            };
            let message = checked(&response, "systemctl --user start")
                .unwrap_err()
                .to_string();
            let (_, excerpt) = message
                .split_once("stderr: ")
                .expect("missing stderr excerpt");
            assert!(excerpt.len() <= 512, "excerpt length: {}", excerpt.len());
            assert!(!excerpt.chars().any(char::is_control));
            assert!(!excerpt.is_empty());
            assert!(message.contains("systemctl --user start"));
            assert!(message.contains("Some(1)"));
            if response.stderr.starts_with(b"permission") {
                assert_eq!(excerpt, "permission denied");
            }
            if response.stderr.starts_with(b"bad byte") {
                assert_eq!(excerpt, "bad byte: \u{fffd}");
            }
        }
        assert!(checked(&result(0, ""), "systemctl").is_ok());
    }

    #[test]
    fn service_launchctl_print_error_includes_stderr() {
        let home = tempfile::tempdir().unwrap();
        let response = ProcessResult {
            code: Some(1),
            stdout: Vec::new(),
            stderr: b"access denied\n".to_vec(),
        };
        let (answer, calls) = exercise(
            Backend::Launchd,
            ServiceAction::Status,
            &paths(&home),
            vec![response],
        );
        let message = answer.unwrap_err().to_string();
        assert!(message.contains("launchctl print"), "{message}");
        assert!(message.contains("access denied"), "{message}");
        assert!(!message.chars().any(char::is_control));
        assert_eq!(calls.len(), 1);
    }

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
                Backend::Systemd => vec![
                    result(0, "LoadState=not-found\nActiveState=inactive\n"),
                    result(0, ""),
                    result(0, ""),
                    result(0, ""),
                ],
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
                        words(&[
                            "systemctl",
                            "--user",
                            "show",
                            "agmend.service",
                            "--property=LoadState",
                            "--property=ActiveState"
                        ]),
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
    fn running_systemd_install_restarts() {
        let home = tempfile::tempdir().unwrap();
        let mut calls = Vec::new();
        let answer = manage_backend(
            Backend::Systemd,
            ServiceAction::Install,
            &paths(&home),
            &mut |argv| {
                calls.push(argv.to_vec());
                Ok(result(0, "LoadState=loaded\nActiveState=active\n"))
            },
        );
        assert!(answer.is_ok(), "{answer:?}");
        assert_eq!(
            calls.last().unwrap(),
            &words(&["systemctl", "--user", "restart", "agmend.service"])
        );
        assert!(
            calls
                .iter()
                .any(|call| call.iter().any(|word| word == "show"))
        );
    }

    #[test]
    fn status_reports_missing_program_path() {
        let home = tempfile::tempdir().unwrap();
        for backend in [Backend::Launchd, Backend::Systemd] {
            let paths = ServicePaths {
                executable: "/missing/Old & Moved.app/agmend".into(),
                ..paths(&home)
            };
            let unit = match backend {
                Backend::Launchd => launchd::render(&paths),
                Backend::Systemd => systemd::render(&paths),
            }
            .unwrap();
            write_unit(&unit).unwrap();
            assert_eq!(
                configured_program(backend, &unit).unwrap(),
                Some(paths.executable.clone())
            );
            let mut run = |_: &[OsString]| {
                Ok(result(
                    0,
                    "state = waiting\nLoadState=loaded\nActiveState=inactive\n",
                ))
            };
            let answer =
                manage_backend(backend, ServiceAction::StatusJson, &paths, &mut run).unwrap();
            let answer: serde_json::Value = serde_json::from_str(&answer).unwrap();
            assert_eq!(
                answer,
                serde_json::json!({"state":"stopped", "program": paths.executable})
            );
            fs::remove_file(&unit.path).unwrap();
            assert_eq!(configured_program(backend, &unit).unwrap(), None);
        }
    }

    #[test]
    fn status_rejects_malformed_program() {
        let home = tempfile::tempdir().unwrap();
        for (backend, contents) in [
            (Backend::Launchd, "<plist><dict><key>ProgramArguments</key><array><string>relative</string></array></dict></plist>".to_owned()),
            (Backend::Launchd, "<plist><dict><key>ProgramArguments</key><array><string>/ok</string></array><key>ProgramArguments</key><array><string>/bad</string></array></dict></plist>".to_owned()),
            (Backend::Launchd, "<plist><dict><key>ProgramArguments</key><array><string>/bad</string></dict></plist>".to_owned()),
            (Backend::Launchd, "<plist><dict><key>ProgramArguments<!--split-->Other</key><array><string>/bad</string></array></dict></plist>".to_owned()),
            (Backend::Launchd, "<plist><dict>garbage<key>ProgramArguments</key><array><string>/bad</string></array></dict></plist>".to_owned()),
            (Backend::Systemd, "[Service]\nExecStart=/ok\nExecStart=/other\n".to_owned()),
            (Backend::Systemd, "[Service]\nExecStart=\"/unclosed\n".to_owned()),
            (Backend::Systemd, "[Service]\nExecStart=/bad/%h\n".to_owned()),
            (Backend::Systemd, "[Service]\nExecStart=relative\n".to_owned()),
            (Backend::Systemd, "[Service]\nExecStart=\"/ok\"suffix\n".to_owned()),
            (Backend::Systemd, "[Service]\nExecStart=/bad/\\x25h\n".to_owned()),
            (Backend::Systemd, "x".repeat(65537)),
        ] {
            let paths = paths(&home);
            let mut unit = match backend { Backend::Launchd => launchd::render(&paths), Backend::Systemd => systemd::render(&paths) }.unwrap();
            unit.contents = contents;
            fs::create_dir_all(unit.path.parent().unwrap()).unwrap();
            fs::write(&unit.path, &unit.contents).unwrap();
            assert!(configured_program(backend, &unit).is_err(), "{}", unit.contents);
            let mut calls = 0;
            assert!(manage_backend(backend, ServiceAction::Install, &paths, &mut |_| { calls += 1; Ok(result(0, "")) }).is_err());
            assert_eq!(calls, 0);
            assert_eq!(fs::read_to_string(&unit.path).unwrap(), unit.contents);
            fs::remove_file(unit.path).unwrap();
        }
    }

    #[test]
    fn status_decodes_program_escaping() {
        let home = tempfile::tempdir().unwrap();
        let paths = ServicePaths {
            executable: "/opt/a & \"b\"/\\%n$HOME/é/agmend".into(),
            ..paths(&home)
        };
        for backend in [Backend::Launchd, Backend::Systemd] {
            let unit = match backend {
                Backend::Launchd => launchd::render(&paths),
                Backend::Systemd => systemd::render(&paths),
            }
            .unwrap();
            write_unit(&unit).unwrap();
            assert_eq!(
                configured_program(backend, &unit).unwrap(),
                Some(paths.executable.clone())
            );
        }
        for (contents, expected) in [
            (
                r#"[Service]
ExecStart="/opt/a\sb/\xC3\xA9/\u0061/\141/%%/$$/agmend" --flag
"#,
                "/opt/a b/é/a/a/%/$/agmend",
            ),
            (
                "[Service]\nExecStart='/opt/a b/agmend'\n",
                "/opt/a b/agmend",
            ),
        ] {
            let mut unit = systemd::render(&paths).unwrap();
            unit.contents = contents.to_owned();
            write_unit(&unit).unwrap();
            assert_eq!(
                configured_program(Backend::Systemd, &unit).unwrap(),
                Some(expected.into())
            );
        }
    }

    #[test]
    fn status_json_reports_absent_program() {
        let home = tempfile::tempdir().unwrap();
        for (backend, response) in [
            (Backend::Launchd, result(113, "")),
            (
                Backend::Systemd,
                result(0, "LoadState=not-found\nActiveState=inactive\n"),
            ),
        ] {
            let answer = exercise(
                backend,
                ServiceAction::StatusJson,
                &paths(&home),
                vec![response],
            )
            .0
            .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&answer).unwrap(),
                serde_json::json!({"state":"not installed","program":null})
            );
        }
    }

    #[test]
    fn explicit_stable_program_spelling_is_preserved() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("Cellar/0.1/agmend");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "executable fixture").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        let stable = home.path().join("active");
        std::os::unix::fs::symlink(target.parent().unwrap(), &stable).unwrap();
        let program = stable.join("agmend");
        assert_eq!(validate_program(&program).unwrap(), program);
        let paths = ServicePaths {
            executable: program.clone(),
            ..paths(&home)
        };
        for backend in [Backend::Launchd, Backend::Systemd] {
            manage_backend(backend, ServiceAction::Install, &paths, &mut |_| {
                Ok(result(0, "LoadState=loaded\nActiveState=inactive\n"))
            })
            .unwrap();
            let unit = match backend {
                Backend::Launchd => launchd::render(&paths),
                Backend::Systemd => systemd::render(&paths),
            }
            .unwrap();
            assert_eq!(
                configured_program(backend, &unit).unwrap(),
                Some(program.clone())
            );
        }
        assert!(validate_program(home.path()).is_err());
        assert!(validate_program(Path::new("relative")).is_err());
        assert!(validate_program(&home.path().join("absent")).is_err());
    }

    #[tokio::test]
    async fn service_health_requires_matching_version_and_ready() {
        use agmen_local::{Request, Response, frame};
        use serde_json::json;
        for (hello, ready) in [
            (json!({"api":1,"version":"old"}), None),
            (json!({"api":1}), None),
            (json!({"api":2,"version":"new"}), None),
            (
                json!({"api":1,"version":"new"}),
                Some(json!({"ready":false,"schemas":{"agmend.db":2}})),
            ),
            (
                json!({"api":1,"version":"new"}),
                Some(json!({"ready":true,"schemas":{}})),
            ),
            (
                json!({"api":1,"version":"new"}),
                Some(json!({"ready":true})),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("s");
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let request: Request = frame::read(&mut stream).unwrap();
                frame::write(&mut stream, &Response::ok(request.id, hello)).unwrap();
                if let Some(ready) = ready {
                    let request: Request = frame::read(&mut stream).unwrap();
                    assert_eq!(request.method, "ready");
                    frame::write(&mut stream, &Response::ok(request.id, ready)).unwrap();
                }
            });
            assert!(
                tokio::time::timeout(Duration::from_secs(1), ready_handshake(&socket, "new"))
                    .await
                    .unwrap()
                    .is_err()
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn readiness_uses_remaining_transition_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("absent");
        let start = Instant::now();
        let error = wait_ready(&socket, "new", start + Duration::from_millis(80)).unwrap_err();
        assert!(format!("{error:#}").contains("deadline"));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(wait_ready(&socket, "new", start).is_err());
    }

    #[test]
    fn upgrade_running_daemon_reports_new_version() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let home = tempfile::tempdir().unwrap();
        let socket_dir = tempfile::tempdir().unwrap();
        let socket = socket_dir.path().join("s");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let upgraded = Arc::new(AtomicBool::new(false));
        let replacement = upgraded.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..100 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let hello: agmen_local::Request =
                            agmen_local::frame::read(&mut stream).unwrap();
                        assert!(replacement.load(Ordering::Acquire));
                        agmen_local::frame::write(
                            &mut stream,
                            &agmen_local::Response::ok(
                                hello.id,
                                serde_json::json!({"api":1,"version":"replacement"}),
                            ),
                        )
                        .unwrap();
                        let ready: agmen_local::Request =
                            agmen_local::frame::read(&mut stream).unwrap();
                        assert_eq!(ready.method, "ready");
                        agmen_local::frame::write(
                            &mut stream,
                            &agmen_local::Response::ok(
                                ready.id,
                                serde_json::json!({"ready":true,"schemas":{"agmend.db":2}}),
                            ),
                        )
                        .unwrap();
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
            panic!("replacement never connected");
        });
        manage_backend(
            Backend::Systemd,
            ServiceAction::Install,
            &paths(&home),
            &mut |argv| {
                if argv.iter().any(|word| word == "restart") {
                    upgraded.store(true, Ordering::Release);
                }
                Ok(result(0, "LoadState=loaded\nActiveState=active\n"))
            },
        )
        .unwrap();
        wait_ready(
            &socket,
            "replacement",
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn running_launchd_install_boots_out_then_bootstraps() {
        let home = tempfile::tempdir().unwrap();
        let (answer, calls) = exercise(
            Backend::Launchd,
            ServiceAction::Install,
            &paths(&home),
            vec![result(0, "state = running"), result(0, ""), result(0, "")],
        );
        assert!(answer.is_ok());
        assert_eq!(
            calls[1],
            words(&["launchctl", "bootout", "gui/501/dev.agmen.agmend"])
        );
        assert_eq!(calls[2][1], "bootstrap");
    }

    #[test]
    fn missing_user_manager_explains_fallback() {
        let home = tempfile::tempdir().unwrap();
        let error = manage_backend(
            Backend::Systemd,
            ServiceAction::Install,
            &paths(&home),
            &mut |_| {
                Ok(ProcessResult {
                    code: Some(1),
                    stdout: vec![],
                    stderr: b"Failed to connect to bus: No medium found".to_vec(),
                })
            },
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("user manager"), "{message}");
        assert!(
            message.contains("foreground") && message.contains("agmend"),
            "{message}"
        );
        assert!(!systemd::render(&paths(&home)).unwrap().path.exists());
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
                vec![
                    result(0, "LoadState=loaded\nActiveState=inactive\n"),
                    result(0, ""),
                    result(0, ""),
                    result(1, ""),
                ]
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
