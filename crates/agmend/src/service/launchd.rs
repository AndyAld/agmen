//! launchd property list rendering and management through an injected runner.

use anyhow::{Context as _, Result};

use super::{
    ProcessResult, Runner, ServiceAction, ServicePaths, Unit, append, checked, executable,
    remove_unit,
};

fn invoke(
    run: &mut Runner<'_>,
    action: &str,
    target: &str,
    path: Option<&std::path::Path>,
) -> Result<ProcessResult> {
    let mut argv = vec!["launchctl".into(), action.into(), target.into()];
    if let Some(path) = path {
        argv.push(path.as_os_str().to_owned());
    }
    run(&argv).with_context(|| format!("launchctl {action}"))
}

pub(super) fn manage(
    action: ServiceAction,
    paths: &ServicePaths,
    unit: &Unit,
    run: &mut Runner<'_>,
) -> Result<String> {
    let target = format!("gui/{}/dev.agmen.agmend", paths.uid);
    let found = invoke(run, "print", &target, None)?;
    if found.code != Some(113) {
        checked(&found, "launchctl print")?;
    }
    if action == ServiceAction::Status {
        return Ok(if found.code == Some(113) {
            if unit.path.try_exists().context("checking service unit")? {
                "stopped\n"
            } else {
                "not installed\n"
            }
        } else if std::str::from_utf8(&found.stdout)
            .context("decoding launchctl print")?
            .lines()
            .any(|line| line.trim() == "state = running")
        {
            "running\n"
        } else {
            "stopped\n"
        }
        .to_owned());
    }
    if found.code == Some(0) {
        checked(&invoke(run, "bootout", &target, None)?, "launchctl bootout")?;
    }
    match action {
        ServiceAction::Install => {
            checked(
                &invoke(
                    run,
                    "bootstrap",
                    &format!("gui/{}", paths.uid),
                    Some(&unit.path),
                )?,
                "launchctl bootstrap",
            )?;
            Ok("installed\n".to_owned())
        }
        ServiceAction::Uninstall => {
            remove_unit(unit)?;
            Ok("uninstalled\n".to_owned())
        }
        ServiceAction::Status => anyhow::bail!("status already handled"),
    }
}

pub(super) fn render(paths: &ServicePaths) -> Result<Unit> {
    let executable = executable(paths)?;
    let mut contents = String::new();
    append(
        &mut contents,
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
            "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n<dict>\n",
            "  <key>Label</key><string>dev.agmen.agmend</string>\n",
            "  <key>ProgramArguments</key>\n",
            "  <array><string>",
        ),
    )?;
    escape(&mut contents, executable)?;
    append(
        &mut contents,
        "</string></array>\n  <key>RunAtLoad</key><true/>\n",
    )?;
    // shortcut: no automatic restart after a crash; revisit when Andy chooses a bounded restart policy.
    append(&mut contents, "  <key>KeepAlive</key><false/>\n")?;
    let log = paths.home.join("Library/Logs/agmen/agmend.log");
    let log = super::validate_path(&log)?;
    for key in ["StandardOutPath", "StandardErrorPath"] {
        append(&mut contents, &format!("  <key>{key}</key><string>"))?;
        escape(&mut contents, log)?;
        append(&mut contents, "</string>\n")?;
    }
    append(&mut contents, "</dict>\n</plist>\n")?;
    Ok(Unit {
        path: paths
            .home
            .join("Library/LaunchAgents/dev.agmen.agmend.plist"),
        contents,
    })
}

fn escape(contents: &mut String, text: &str) -> Result<()> {
    // Validation limits this loop to at most 64 KiB of input.
    for ch in text.chars() {
        let mut utf8 = [0; 4];
        append(
            contents,
            match ch {
                '&' => "&amp;",
                '"' => "&quot;",
                '<' => "&lt;",
                '>' => "&gt;",
                '\'' => "&apos;",
                _ => ch.encode_utf8(&mut utf8),
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::ServicePaths;
    use std::path::PathBuf;

    #[test]
    fn launchd_renders_absolute_binary() {
        let paths = ServicePaths {
            home: PathBuf::from("/test/home"),
            executable: PathBuf::from("/opt/agmen/agmend"),
            uid: 501,
        };
        let unit = render(&paths).unwrap();
        assert_eq!(
            unit.path,
            paths
                .home
                .join("Library/LaunchAgents/dev.agmen.agmend.plist")
        );
        assert_eq!(
            unit.contents,
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
                "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
                "<plist version=\"1.0\">\n<dict>\n",
                "  <key>Label</key><string>dev.agmen.agmend</string>\n",
                "  <key>ProgramArguments</key>\n",
                "  <array><string>/opt/agmen/agmend</string></array>\n",
                "  <key>RunAtLoad</key><true/>\n",
                "  <key>KeepAlive</key><false/>\n",
                "  <key>StandardOutPath</key><string>/test/home/Library/Logs/agmen/agmend.log</string>\n",
                "  <key>StandardErrorPath</key><string>/test/home/Library/Logs/agmen/agmend.log</string>\n",
                "</dict>\n</plist>\n",
            )
        );
    }

    #[test]
    fn launchd_escapes_xml() {
        let paths = ServicePaths {
            home: PathBuf::from("/test/home"),
            executable: PathBuf::from("/opt/a & \"b\" <c> 'd'/agmend"),
            uid: 501,
        };
        let unit = render(&paths).unwrap();
        assert!(unit.contents.contains(
            "<array><string>/opt/a &amp; &quot;b&quot; &lt;c&gt; &apos;d&apos;/agmend</string></array>"
        ));
    }
}
