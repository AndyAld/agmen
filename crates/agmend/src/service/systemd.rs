//! systemd user-unit rendering, without filesystem or manager access.

use anyhow::Result;

use super::{ServicePaths, Unit, append, executable};

pub(super) fn render(paths: &ServicePaths) -> Result<Unit> {
    let executable = executable(paths)?;
    let mut contents = String::new();
    append(
        &mut contents,
        concat!(
            "[Unit]\nDescription=agmen daemon\n\n",
            "[Service]\nType=simple\nExecStart=\"",
        ),
    )?;
    // Validation limits this loop to at most 64 KiB of input.
    for ch in executable.chars() {
        let mut utf8 = [0; 4];
        append(
            &mut contents,
            match ch {
                '"' => "\\\"",
                '\\' => "\\\\",
                '%' => "%%",
                '$' => "$$",
                _ => ch.encode_utf8(&mut utf8),
            },
        )?;
    }
    // shortcut: no automatic restart after a crash; revisit when Andy chooses a bounded restart policy.
    append(
        &mut contents,
        "\"\nRestart=no\n\n[Install]\nWantedBy=default.target\n",
    )?;
    Ok(Unit {
        path: paths.home.join(".config/systemd/user/agmend.service"),
        contents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::ServicePaths;
    use std::path::PathBuf;

    #[test]
    fn systemd_renders_user_unit() {
        let paths = ServicePaths {
            home: PathBuf::from("/test/home"),
            executable: PathBuf::from("/opt/agmen/agmend"),
            uid: 501,
        };
        let unit = render(&paths).unwrap();
        assert_eq!(
            unit.path,
            paths.home.join(".config/systemd/user/agmend.service")
        );
        assert_eq!(
            unit.contents,
            concat!(
                "[Unit]\nDescription=agmen daemon\n\n",
                "[Service]\nType=simple\nExecStart=\"/opt/agmen/agmend\"\nRestart=no\n\n",
                "[Install]\nWantedBy=default.target\n",
            )
        );
    }

    #[test]
    fn systemd_escapes_exec_path() {
        let paths = ServicePaths {
            home: PathBuf::from("/test/home"),
            executable: PathBuf::from("/opt/a b/\"c\"\\%n${HOME}/agmend"),
            uid: 501,
        };
        let unit = render(&paths).unwrap();
        assert_eq!(
            unit.contents
                .lines()
                .find(|line| line.starts_with("ExecStart=")),
            Some(r#"ExecStart="/opt/a b/\"c\"\\%%n$${HOME}/agmend""#)
        );
    }
}
