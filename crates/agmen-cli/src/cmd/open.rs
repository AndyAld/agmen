//! Desktop launch decisions are separate from spawning so tests never open a GUI.

use anyhow::{Context as _, Result, ensure};
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "macos", test))]
// A short-lived second process delivers argv through the instance forwarder.
const REGISTERED_ARGS: &[&str] = &["-n", "-b", "ai.getagmen.desktop", "--args"];

pub(super) fn folder(path: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    let path = path.unwrap_or(cwd);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    ensure!(path.exists(), "folder does not exist: {}", path.display());
    ensure!(path.is_dir(), "not a folder: {}", path.display());
    path.canonicalize().context("resolving the folder to open")
}

#[cfg(any(target_os = "macos", test))]
fn bundled_desktop(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    if macos.file_name()? != "MacOS"
        || contents.file_name()? != "Contents"
        || app.extension()? != "app"
    {
        return None;
    }
    let desktop = macos.join("agmen-desktop");
    desktop.is_file().then_some(desktop)
}

pub(super) async fn run(path: Option<PathBuf>) -> Result<()> {
    let cwd = std::env::current_dir().context("reading the current folder")?;
    let path = folder(path.as_deref(), &cwd)?;
    launch(&path).await
}

#[cfg(not(target_os = "macos"))]
async fn launch(_path: &Path) -> Result<()> {
    anyhow::bail!("the desktop app is macOS-only for now")
}

#[cfg(target_os = "macos")]
async fn launch(path: &Path) -> Result<()> {
    use std::process::Stdio;
    use std::time::Duration;
    let exe = std::env::current_exe().context("locating agmen")?;
    if let Some(desktop) = bundled_desktop(&exe) {
        tokio::process::Command::new(desktop)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .context("launching agmen.app")?;
        return Ok(());
    }
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("/usr/bin/open")
            .args(REGISTERED_ARGS)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .context("looking up agmen.app timed out")?
    .context("looking up agmen.app")?;
    ensure!(
        status.success(),
        "agmen.app is not installed. Download it from https://getagmen.ai/download/mac"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_app_launch_reaches_instance_forwarder() {
        assert_eq!(
            REGISTERED_ARGS,
            ["-n", "-b", "ai.getagmen.desktop", "--args"]
        );
    }

    #[test]
    fn decisions_resolve_cwd_relative_absolute_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(
            folder(None, dir.path()).unwrap(),
            dir.path().canonicalize().unwrap()
        );
        assert_eq!(
            folder(Some(Path::new("sub")), dir.path()).unwrap(),
            dir.path().join("sub").canonicalize().unwrap()
        );
        assert_eq!(
            folder(Some(dir.path()), dir.path()).unwrap(),
            dir.path().canonicalize().unwrap()
        );
        assert!(
            folder(Some(Path::new("missing")), dir.path())
                .unwrap_err()
                .to_string()
                .contains("does not exist")
        );
    }

    #[test]
    fn bundle_lookup_requires_bundle_layout_and_desktop() {
        let dir = tempfile::tempdir().unwrap();
        let macos = dir.path().join("agmen.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        assert_eq!(bundled_desktop(&macos.join("agmen")), None);
        std::fs::write(macos.join("agmen-desktop"), "").unwrap();
        assert_eq!(
            bundled_desktop(&macos.join("agmen")),
            Some(macos.join("agmen-desktop"))
        );
        assert_eq!(bundled_desktop(&dir.path().join("agmen")), None);
    }
}
