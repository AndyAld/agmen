//! launchd XML property list rendering, without filesystem or manager access.

use anyhow::Result;

use super::{ServicePaths, Unit, append, executable};

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
    // Validation limits this loop to at most 64 KiB of input.
    for ch in executable.chars() {
        let mut utf8 = [0; 4];
        append(
            &mut contents,
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
    append(
        &mut contents,
        "</string></array>\n  <key>RunAtLoad</key><true/>\n",
    )?;
    // shortcut: no automatic restart after a crash; revisit when Andy chooses a bounded restart policy.
    append(
        &mut contents,
        "  <key>KeepAlive</key><false/>\n</dict>\n</plist>\n",
    )?;
    Ok(Unit {
        path: paths
            .home
            .join("Library/LaunchAgents/dev.agmen.agmend.plist"),
        contents,
    })
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
