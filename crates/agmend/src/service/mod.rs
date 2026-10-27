//! Pure user-service unit rendering with injected paths.

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
