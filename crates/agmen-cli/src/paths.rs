//! Where agmen keeps its files on this machine (docs/plans/mvp.md):
//!
//! - who is signed in: `login.json` in the config folder, `~/Library/Application
//!   Support/agmen` on macOS, `$XDG_CONFIG_HOME/agmen` (else `~/.config/agmen`)
//!   on Linux, `%APPDATA%\agmen` on Windows;
//! - each identity's SQLite files: `<identity_id>/` in the state folder,
//!   `~/Library/Application Support/agmen` on macOS, `$XDG_STATE_HOME/agmen`
//!   (else `~/.local/state/agmen`) on Linux, `%LOCALAPPDATA%\agmen` on
//!   Windows.
//!
//! Tokens are not here: they go through agmen-client's token store.

use std::path::PathBuf;

use agmen_proto::IdentityId;
use anyhow::{Context as _, Result};

/// The config folder.
pub(crate) fn config_dir() -> Result<PathBuf> {
    base("XDG_CONFIG_HOME", ".config", "APPDATA")
}

/// The state folder of `identity`.
pub(crate) fn state_dir(identity: &IdentityId) -> Result<PathBuf> {
    Ok(base("XDG_STATE_HOME", ".local/state", "LOCALAPPDATA")?.join(identity.as_str()))
}

/// The login file.
pub(crate) fn login_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("login.json"))
}

/// `<base>/agmen` for this platform: on Linux the XDG variable `xdg` when it
/// is absolute, else `$HOME/<fallback>`; on macOS Application Support; on
/// Windows the folder in `windows_var`.
fn base(xdg: &str, fallback: &str, windows_var: &str) -> Result<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let home = || var("HOME").context("HOME is not set to an absolute path");
    let dir = if cfg!(target_os = "windows") {
        var(windows_var).with_context(|| format!("{windows_var} is not set"))?
    } else if cfg!(target_os = "macos") {
        home()?.join("Library").join("Application Support")
    } else {
        match var(xdg) {
            Some(dir) => dir,
            None => home()?.join(fallback),
        }
    };
    Ok(dir.join("agmen"))
}
