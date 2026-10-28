//! The optional CLI link offered by the desktop.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agmen_client::InstallReceipt;
use anyhow::{Context as _, Result, ensure};

use crate::bundle::Bundle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Offer {
    Available { hint: String },
    Existing { path: PathBuf, version: String },
    Blocked(String),
}

pub(crate) fn inspect(
    home: &Path,
    receipt: Option<&InstallReceipt>,
    run: &mut crate::service::Runner<'_>,
) -> Result<Offer> {
    let timeout = Duration::from_secs(5);
    let output = run(
        Path::new("/bin/zsh"),
        &["-lc".into(), "command -v agmen".into()],
        timeout,
    )?;
    if output.status.success() {
        let path = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        ensure!(
            path.is_absolute(),
            "The login shell returned an invalid agmen path"
        );
        let output = run(&path, &["--version".into()], timeout)?;
        ensure!(
            output.status.success(),
            "Reading the existing agmen version failed"
        );
        let version = String::from_utf8(output.stdout)?
            .trim()
            .chars()
            .take(512)
            .collect();
        return Ok(Offer::Existing { path, version });
    }
    ensure!(
        output.status.code() == Some(1) && output.stdout.is_empty(),
        "Checking the login shell PATH failed: {}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(512)
            .collect::<String>()
    );
    if receipt.is_some_and(|receipt| receipt.channel == "shell") {
        return Ok(Offer::Blocked("A shell installation owns agmen. Repair its PATH with the shell installer; the app will not write to ~/.local/bin.".into()));
    }
    let output = run(
        Path::new("/bin/zsh"),
        &["-lc".into(), "printf '%s' \"$PATH\"".into()],
        timeout,
    )?;
    ensure!(
        output.status.success(),
        "Reading the login shell PATH failed"
    );
    let local_bin = home.join(".local/bin");
    let on_path = std::env::split_paths(std::ffi::OsStr::new(std::str::from_utf8(&output.stdout)?))
        .any(|path| path == local_bin);
    let hint = if on_path {
        String::new()
    } else {
        path_hint(&std::env::var("SHELL").unwrap_or_default()).into()
    };
    Ok(Offer::Available { hint })
}

/// Links only after the caller has read the shared receipt and checked PATH.
pub(crate) fn install(
    bundle: &Bundle,
    home: &Path,
    receipt: Option<&InstallReceipt>,
    existing: Option<&Path>,
) -> Result<()> {
    ensure!(
        receipt.is_none_or(|receipt| receipt.channel != "shell"),
        "A shell installation owns the agmen command. Use that installer to repair PATH."
    );
    ensure!(
        existing.is_none(),
        "agmen is already on PATH; keeping that command."
    );
    // A link into a mounted disk image or translocated copy breaks once it goes away.
    ensure!(
        !crate::service::temporary(bundle),
        "Move agmen to Applications and relaunch it before installing the agmen command."
    );
    ensure!(
        home.is_absolute() && bundle.agmen.is_absolute(),
        "CLI paths must be absolute"
    );
    ensure!(
        bundle.agmen.is_file(),
        "The bundled agmen command is missing"
    );
    let bin = home.join(".local/bin");
    let destination = bin.join("agmen");
    match std::fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_symlink()
                    && std::fs::read_link(&destination)? == bundle.agmen,
                "{} belongs to another installation; keep it or choose a channel switch separately.",
                destination.display()
            );
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("reading existing agmen command"),
    }
    std::fs::create_dir_all(&bin).context("creating the CLI directory")?;
    std::os::unix::fs::symlink(&bundle.agmen, &destination).context("linking the agmen command")
}

pub(crate) fn path_hint(shell: &str) -> &'static str {
    if shell.ends_with("/fish") {
        "Run fish_add_path ~/.local/bin in fish."
    } else if shell.ends_with("/bash") {
        "Add export PATH=\"$HOME/.local/bin:$PATH\" to the first readable file among ~/.bash_profile, ~/.bash_login and ~/.profile, or create ~/.bash_profile if none exists. Then open a new login shell."
    } else {
        "Add export PATH=\"$HOME/.local/bin:$PATH\" to ~/.zprofile, then open a new login shell."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::Bundle;
    use std::path::Path;

    fn bundle(home: &Path) -> Bundle {
        let app = home.join("Applications/agmen.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let agmen = macos.join("agmen");
        std::fs::write(&agmen, "cli").unwrap();
        Bundle {
            app,
            agmen,
            agmend: macos.join("agmend"),
        }
    }

    #[test]
    fn link_points_into_the_bundle() {
        let home = tempfile::tempdir().unwrap();
        let bundle = bundle(home.path());
        install(&bundle, home.path(), None, None).unwrap();
        assert_eq!(
            std::fs::read_link(home.path().join(".local/bin/agmen")).unwrap(),
            bundle.agmen
        );
        install(&bundle, home.path(), None, None).unwrap();
    }

    #[test]
    fn link_refuses_a_foreign_file() {
        let home = tempfile::tempdir().unwrap();
        let bundle = bundle(home.path());
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let dest = bin.join("agmen");
        std::fs::write(&dest, "mine").unwrap();
        assert!(install(&bundle, home.path(), None, None).is_err());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "mine");
        std::fs::remove_file(&dest).unwrap();
        std::os::unix::fs::symlink("/foreign/agmen", &dest).unwrap();
        assert!(install(&bundle, home.path(), None, None).is_err());
        assert_eq!(
            std::fs::read_link(&dest).unwrap(),
            Path::new("/foreign/agmen")
        );
    }

    #[test]
    fn temporary_bundle_refuses_cli_link() {
        let home = tempfile::tempdir().unwrap();
        let app = home.path().join("AppTranslocation/ID/d/agmen.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("agmen"), "cli").unwrap();
        let bundle = Bundle {
            agmen: macos.join("agmen"),
            agmend: macos.join("agmend"),
            app,
        };
        let error = install(&bundle, home.path(), None, None).unwrap_err();
        assert!(error.to_string().contains("Move agmen to Applications"));
        assert!(!home.path().join(".local").exists());
    }

    #[test]
    fn existing_cli_is_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let bundle = bundle(home.path());
        assert!(
            install(
                &bundle,
                home.path(),
                None,
                Some(Path::new("/opt/homebrew/bin/agmen"))
            )
            .is_err()
        );
        assert!(!home.path().join(".local").exists());
    }

    #[test]
    fn shell_receipt_blocks_local_bin_writes() {
        let home = tempfile::tempdir().unwrap();
        let bundle = bundle(home.path());
        let receipt = agmen_client::InstallReceipt {
            channel: "shell".into(),
            version: "1".into(),
            owned_paths: vec![],
        };
        assert!(install(&bundle, home.path(), Some(&receipt), None).is_err());
        assert!(!home.path().join(".local").exists());
    }

    #[test]
    fn path_hint_when_local_bin_missing() {
        assert!(path_hint("/bin/zsh").contains("~/.zprofile"));
    }

    #[test]
    fn path_hints_cover_zsh_bash_fish() {
        assert!(path_hint("/bin/zsh").contains("export PATH"));
        assert!(path_hint("/bin/bash").contains("~/.bash_profile"));
        assert!(path_hint("/bin/bash").contains("~/.bash_login"));
        assert!(path_hint("/bin/bash").contains("~/.profile"));
        assert!(path_hint("/opt/homebrew/bin/fish").contains("fish_add_path ~/.local/bin"));
    }

    #[test]
    fn existing_cli_reports_path_and_version() {
        use std::os::unix::process::ExitStatusExt as _;
        let home = tempfile::tempdir().unwrap();
        let mut calls = Vec::new();
        let offer = inspect(home.path(), None, &mut |program, args, timeout| {
            calls.push((program.to_owned(), args.to_vec(), timeout));
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: if calls.len() == 1 {
                    b"/opt/homebrew/bin/agmen\n".to_vec()
                } else {
                    b"agmen 0.1.0\n".to_vec()
                },
                stderr: vec![],
            })
        })
        .unwrap();
        assert!(
            matches!(offer, Offer::Existing { path, version } if path == Path::new("/opt/homebrew/bin/agmen") && version == "agmen 0.1.0")
        );
        assert_eq!(calls[0].0, Path::new("/bin/zsh"));
        assert_eq!(
            calls[0].1,
            ["-lc", "command -v agmen"].map(std::ffi::OsString::from)
        );
        assert!(
            calls
                .iter()
                .all(|call| call.2 == std::time::Duration::from_secs(5))
        );
    }

    #[test]
    fn missing_cli_with_shell_receipt_offers_repair_only() {
        use std::os::unix::process::ExitStatusExt as _;
        let home = tempfile::tempdir().unwrap();
        let receipt = agmen_client::InstallReceipt {
            channel: "shell".into(),
            version: "1".into(),
            owned_paths: vec![],
        };
        let offer = inspect(home.path(), Some(&receipt), &mut |_, _, _| {
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(256),
                stdout: vec![],
                stderr: vec![],
            })
        })
        .unwrap();
        assert!(matches!(offer, Offer::Blocked(text) if text.contains("shell")));
        assert!(!home.path().join(".local").exists());
    }
}
