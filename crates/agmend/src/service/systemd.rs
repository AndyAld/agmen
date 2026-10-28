//! systemd user-unit rendering and management through an injected runner.

use anyhow::{Context as _, Result};

use super::{ServicePaths, Unit, append, executable};

pub(super) fn active(run: &mut super::Runner<'_>) -> Result<bool> {
    let argv: Vec<_> = [
        "systemctl",
        "--user",
        "show",
        "agmend.service",
        "--property=LoadState",
        "--property=ActiveState",
    ]
    .into_iter()
    .map(std::ffi::OsString::from)
    .collect();
    let fallback = "systemd user manager is unavailable; run agmend in the foreground instead";
    let result = run(&argv).context(fallback)?;
    super::checked(&result, "systemctl --user show").context(fallback)?;
    let output = std::str::from_utf8(&result.stdout).context("decoding systemctl show")?;
    let active = output
        .lines()
        .find_map(|line| line.strip_prefix("ActiveState="))
        .context("systemctl show omitted ActiveState")?;
    Ok(matches!(
        active,
        "active" | "activating" | "reloading" | "deactivating"
    ))
}

pub(super) fn manage(
    action: super::ServiceAction,
    unit: &Unit,
    run: &mut super::Runner<'_>,
    active: bool,
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
                &[if active { "restart" } else { "start" }, "agmend.service"],
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
        ServiceAction::StatusJson => anyhow::bail!("JSON status already handled"),
    }
}

pub(super) fn program(contents: &str) -> Result<std::path::PathBuf> {
    use anyhow::ensure;
    let mut section = "";
    let mut command = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        ensure!(
            !line.ends_with('\\'),
            "continued systemd configuration is ambiguous"
        );
        if line.starts_with('[') {
            section = line
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .context("malformed systemd section")?;
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .context("malformed systemd assignment")?;
        if section == "Service" && key.trim() == "ExecStart" {
            ensure!(
                command.is_none(),
                "multiple ExecStart assignments are ambiguous"
            );
            command = Some(value.trim());
        }
    }
    let command = command.context("service unit lacks ExecStart")?;
    let words = exec_words(command)?;
    let first = words.first().context("empty ExecStart")?;
    Ok(first.into())
}

/// Decode the unit's quoting and C escapes; refuse executable expansions.
fn exec_words(command: &str) -> Result<Vec<String>> {
    use anyhow::ensure;
    let mut words = Vec::new();
    let mut word = Vec::new();
    let mut quote = None;
    let mut started = false;
    let mut closed = false;
    let mut chars = command.chars();
    // The caller bounds configuration input to 64 KiB.
    while let Some(ch) = chars.next() {
        ensure!(
            !closed || ch.is_whitespace(),
            "systemd closing quote must end a word"
        );
        if ch == '\\' {
            word.extend(decode_escape(&mut chars)?);
            started = true;
        } else if matches!(ch, '\'' | '"') {
            match quote {
                None => {
                    ensure!(!started, "systemd quote must begin a word");
                    quote = Some(ch);
                    started = true;
                }
                Some(open) if open == ch => {
                    quote = None;
                    closed = true;
                }
                Some(_) => {
                    let mut utf8 = [0; 4];
                    word.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
                }
            }
        } else if ch.is_whitespace() && quote.is_none() {
            if started {
                words.push(
                    String::from_utf8(std::mem::take(&mut word))
                        .context("ExecStart is not UTF-8")?,
                );
                started = false;
                closed = false;
            }
        } else {
            let mut utf8 = [0; 4];
            word.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
            started = true;
        }
    }
    ensure!(quote.is_none(), "unclosed systemd quote");
    if started {
        words.push(String::from_utf8(word).context("ExecStart is not UTF-8")?);
    }
    if let Some(first) = words.first_mut() {
        let mut program = String::new();
        let mut chars = first.chars();
        while let Some(ch) = chars.next() {
            if matches!(ch, '%' | '$') {
                ensure!(
                    chars.next() == Some(ch),
                    "ExecStart executable contains an unresolved expansion"
                );
            }
            program.push(ch);
        }
        *first = program;
    }
    Ok(words)
}

fn decode_escape(chars: &mut std::str::Chars<'_>) -> Result<Vec<u8>> {
    let escaped = chars.next().context("truncated systemd escape")?;
    let scalar = match escaped {
        '\\' | '"' | '\'' => escaped,
        's' => ' ',
        't' => '\t',
        'n' => '\n',
        'r' => '\r',
        'a' => '\u{7}',
        'b' => '\u{8}',
        'f' => '\u{c}',
        'v' => '\u{b}',
        'x' | 'u' | 'U' | '0'..='7' => {
            let (radix, digits, mut code) = match escaped {
                'x' => (16, 2, 0_u32),
                'u' => (16, 4, 0),
                'U' => (16, 8, 0),
                digit => (8, 2, u32::from(digit) - u32::from('0')),
            };
            for _ in 0..digits {
                let digit = chars
                    .next()
                    .and_then(|ch| ch.to_digit(radix))
                    .context("invalid systemd numeric escape")?;
                code = code
                    .checked_mul(radix)
                    .and_then(|value| value.checked_add(digit))
                    .context("systemd escape overflow")?;
            }
            if escaped != 'u' && escaped != 'U' {
                return Ok(vec![
                    u8::try_from(code).context("systemd byte escape exceeds 255")?,
                ]);
            }
            char::from_u32(code).context("invalid systemd code point")?
        }
        _ => anyhow::bail!("unknown systemd escape"),
    };
    let mut utf8 = [0; 4];
    Ok(scalar.encode_utf8(&mut utf8).as_bytes().to_vec())
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
