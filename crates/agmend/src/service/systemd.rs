//! systemd user-unit rendering and management through an injected runner.

use anyhow::{Context as _, Result};

use super::{ServicePaths, Unit, append, executable};

pub(super) fn manage(
    action: super::ServiceAction,
    unit: &Unit,
    run: &mut super::Runner<'_>,
) -> Result<String> {
    use super::ServiceAction;
    let mut invoke = |args: &[&str]| {
        let argv: Vec<_> = ["systemctl", "--user"]
            .into_iter()
            .chain(args.iter().copied())
            .map(std::ffi::OsString::from)
            .collect();
        run(&argv).with_context(|| format!("systemctl --user {}", args.join(" ")))
    };
    match action {
        ServiceAction::Status => {
            let result = invoke(&[
                "show",
                "agmend.service",
                "--property=LoadState",
                "--property=ActiveState",
            ])?;
            super::checked(&result, "systemctl --user show")?;
            let output = std::str::from_utf8(&result.stdout).context("decoding systemctl show")?;
            let load = output
                .lines()
                .find_map(|line| line.strip_prefix("LoadState="))
                .context("systemctl show omitted LoadState")?;
            let state = if load == "not-found" {
                "not installed\n"
            } else {
                let active = output
                    .lines()
                    .find_map(|line| line.strip_prefix("ActiveState="))
                    .context("systemctl show omitted ActiveState")?;
                if active == "active" {
                    "running\n"
                } else {
                    "stopped\n"
                }
            };
            Ok(state.to_owned())
        }
        ServiceAction::Install => {
            for args in [
                &["daemon-reload"][..],
                &["enable", "agmend.service"],
                &["start", "agmend.service"],
            ] {
                super::checked(
                    &invoke(args)?,
                    &format!("systemctl --user {}", args.join(" ")),
                )?;
            }
            Ok("installed\n".to_owned())
        }
        ServiceAction::Uninstall => {
            if !unit.path.try_exists().context("checking service unit")? {
                // Finish a prior removal whose reload failed, then check for a
                // service still running after its unit file disappeared.
                super::checked(
                    &invoke(&["daemon-reload"])?,
                    "systemctl --user daemon-reload",
                )?;
                let result = invoke(&[
                    "show",
                    "agmend.service",
                    "--property=LoadState",
                    "--property=ActiveState",
                ])?;
                super::checked(&result, "systemctl --user show")?;
                let output =
                    std::str::from_utf8(&result.stdout).context("decoding systemctl show")?;
                let active = output
                    .lines()
                    .find_map(|line| line.strip_prefix("ActiveState="))
                    .context("systemctl show omitted ActiveState")?;
                if active != "inactive" {
                    super::checked(
                        &invoke(&["stop", "agmend.service"])?,
                        "systemctl --user stop",
                    )?;
                }
                return Ok("uninstalled\n".to_owned());
            }
            for action in ["disable", "stop"] {
                super::checked(
                    &invoke(&[action, "agmend.service"])?,
                    &format!("systemctl --user {action}"),
                )?;
            }
            super::remove_unit(unit)?;
            super::checked(
                &invoke(&["daemon-reload"])?,
                "systemctl --user daemon-reload",
            )?;
            Ok("uninstalled\n".to_owned())
        }
    }
}

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
