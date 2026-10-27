//! Where agmen keeps its files on this machine (docs/plans/mvp.md), for the
//! CLI and agmend alike:
//!
//! - who is signed in: `login.json` in the config folder, `~/Library/Application
//!   Support/agmen` on macOS, `$XDG_CONFIG_HOME/agmen` (else `~/.config/agmen`)
//!   on Linux, `%APPDATA%\agmen` on Windows;
//! - each identity's SQLite files, and agmend's socket and lock file, in the
//!   state folder: `~/Library/Application Support/agmen` on macOS,
//!   `$XDG_STATE_HOME/agmen` (else `~/.local/state/agmen`) on Linux,
//!   `%LOCALAPPDATA%\agmen` on Windows.
//!
//! Tokens are not here: they go through the token store.

use std::path::PathBuf;

/// A folder could not be found because the variable that names it is unset
/// or not an absolute path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0} is not set to an absolute path")]
pub struct PathError(&'static str);

/// The config folder, which holds `login.json`.
///
/// # Errors
///
/// [`PathError`] when `HOME` (or, on Windows, `APPDATA`) is not set.
pub fn config_dir() -> Result<PathBuf, PathError> {
    base("XDG_CONFIG_HOME", ".config", "APPDATA")
}

/// The state folder: one folder per identity, and agmend's socket.
///
/// # Errors
///
/// [`PathError`] when `HOME` (or, on Windows, `LOCALAPPDATA`) is not set.
pub fn state_dir() -> Result<PathBuf, PathError> {
    base("XDG_STATE_HOME", ".local/state", "LOCALAPPDATA")
}

/// The login file `agmen login` writes.
///
/// # Errors
///
/// As [`config_dir`].
pub fn login_file() -> Result<PathBuf, PathError> {
    Ok(config_dir()?.join("login.json"))
}

/// `<base>/agmen` for this platform: on Linux the XDG variable `xdg` when it
/// is absolute, else `$HOME/<fallback>`; on macOS Application Support; on
/// Windows the folder in `windows_var`.
fn base(xdg: &str, fallback: &str, windows_var: &'static str) -> Result<PathBuf, PathError> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let home = || var("HOME").ok_or(PathError("HOME"));
    let dir = if cfg!(target_os = "windows") {
        var(windows_var).ok_or(PathError(windows_var))?
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
