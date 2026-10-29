//! Where agmen keeps its files on this machine: agmen-client's paths
//! (`config_dir`, `state_dir`, `login_file`), as anyhow results.
//!
//! Tokens are not here: they go through agmen-client's token store.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};

/// The state folder that holds one folder per identity.
pub(crate) fn state_base() -> Result<PathBuf> {
    Ok(agmen_client::state_dir()?)
}

/// The login file.
pub(crate) fn login_file() -> Result<PathBuf> {
    Ok(agmen_client::login_file()?)
}

/// Whether `name` is a host directory name: `[a-z0-9][a-z0-9-]{0,31}`.
pub(crate) fn is_host_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    let mut len = 1usize;
    for ch in chars {
        if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-') {
            return false;
        }
        len += 1;
        if len > 32 {
            return false;
        }
    }
    true
}

/// `$HOME/.agmen/host/<name>` without creating it.
///
/// # Errors
///
/// When the home directory variable is missing or not absolute, or `name`
/// is not a host name.
pub(crate) fn host_dir_path(name: &str) -> Result<PathBuf> {
    if !is_host_name(name) {
        bail!(
            "{name:?} is not a host name: use 1 to 32 lowercase letters, digits and hyphens, starting with a letter or digit"
        );
    }
    Ok(home_dir()?.join(".agmen").join("host").join(name))
}

/// `$HOME/.agmen/host/<name>`, created mode 0700.
///
/// # Errors
///
/// As [`host_dir_path`], and when the directory cannot be created.
pub(crate) fn host_dir(name: &str) -> Result<PathBuf> {
    let dir = host_dir_path(name)?;
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("setting permissions on {}", dir.display()))?;
    }
    Ok(dir)
}

fn home_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        // shortcut: %USERPROFILE% with no directory mode; add a Windows host_dir test before relying on it
        absolute_var("USERPROFILE")
    }
    #[cfg(not(windows))]
    {
        absolute_var("HOME")
    }
}

fn absolute_var(name: &str) -> Result<PathBuf> {
    let value = std::env::var(name).with_context(|| format!("{name} is not set"))?;
    let path = PathBuf::from(&value);
    if !path.is_absolute() {
        bail!("{name} is not an absolute path");
    }
    Ok(path)
}
