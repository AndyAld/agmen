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

// shortcut: Unix sockets only; add Windows named pipe checks when Windows IPC lands.
#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
/// Why an agmend socket cannot be trusted for a local connection.
pub enum AgmendSocketError {
    /// Nothing is listening at this path because the socket is absent.
    #[error("agmend socket is missing")]
    Missing,
    /// The path names something other than a Unix socket.
    #[error("{} is not a socket, so it is not agmend's", path.display())]
    NotSocket {
        /// The socket path checked.
        path: PathBuf,
    },
    /// Another user owns the socket.
    #[error("{} is owned by uid {owner}, not by you (uid {uid}); refusing to talk to it", path.display())]
    WrongOwner {
        /// The socket path checked.
        path: PathBuf,
        /// The socket owner's uid.
        owner: u32,
        /// The required owner's uid.
        uid: u32,
    },
    /// The socket permissions differ from 0600.
    #[error("{} has mode {mode:04o}, so other users can reach it; agmend's socket is 0600", path.display())]
    OpenMode {
        /// The socket path checked.
        path: PathBuf,
        /// The permission bits found.
        mode: u32,
    },
    /// The socket's metadata could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The socket path checked.
        path: PathBuf,
        /// The filesystem error.
        source: std::io::Error,
    },
}

/// Checks that agmend's socket belongs to the effective user and has mode 0600.
///
/// # Errors
/// Returns [`AgmendSocketError`] if the path is missing, unreadable or untrusted.
#[cfg(unix)]
pub fn check_agmend_socket(socket: &std::path::Path) -> Result<(), AgmendSocketError> {
    check_agmend_socket_for(socket, rustix::process::geteuid().as_raw())
}

/// Checks that `socket` is a Unix socket owned by `uid` with mode 0600.
///
/// # Errors
/// Returns [`AgmendSocketError`] if the path is missing, unreadable or untrusted.
#[cfg(unix)]
pub fn check_agmend_socket_for(
    socket: &std::path::Path,
    uid: u32,
) -> Result<(), AgmendSocketError> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    let meta = std::fs::metadata(socket).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            AgmendSocketError::Missing
        } else {
            AgmendSocketError::Read {
                path: socket.to_owned(),
                source,
            }
        }
    })?;
    if !meta.file_type().is_socket() {
        return Err(AgmendSocketError::NotSocket {
            path: socket.to_owned(),
        });
    }
    if meta.uid() != uid {
        return Err(AgmendSocketError::WrongOwner {
            path: socket.to_owned(),
            owner: meta.uid(),
            uid,
        });
    }
    let mode = meta.mode() & 0o777;
    if mode != 0o600 {
        return Err(AgmendSocketError::OpenMode {
            path: socket.to_owned(),
            mode,
        });
    }
    Ok(())
}

/// A folder could not be found because the variable that names it is unset
/// or not an absolute path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PathError {
    /// `HOME` is unset, empty or relative.
    #[error("HOME is not set to an absolute path")]
    Home,
    /// The Windows folder variable is unset, empty or relative.
    #[error("{0} is not set")]
    Windows(&'static str),
}

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

/// The file name of agmend's socket in the state folder.
pub const AGMEND_SOCKET: &str = "agmend.sock";

/// agmend's socket, which agmend binds and the desktop app connects to:
/// `$XDG_STATE_HOME/agmen/agmend.sock`, on macOS
/// `~/Library/Application Support/agmen/agmend.sock`.
///
/// # Errors
///
/// As [`state_dir`].
pub fn agmend_socket() -> Result<PathBuf, PathError> {
    Ok(state_dir()?.join(AGMEND_SOCKET))
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
    let home = || var("HOME").ok_or(PathError::Home);
    let dir = if cfg!(target_os = "windows") {
        var(windows_var).ok_or(PathError::Windows(windows_var))?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn socket_check_refuses_other_owners_and_open_modes() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let uid = std::fs::metadata(&socket).unwrap().uid();
        assert!(check_agmend_socket(&socket).is_ok());
        assert!(check_agmend_socket_for(&socket, uid).is_ok());
        assert!(matches!(
            check_agmend_socket_for(&socket, uid + 1),
            Err(AgmendSocketError::WrongOwner { .. })
        ));
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(matches!(
            check_agmend_socket_for(&socket, uid),
            Err(AgmendSocketError::OpenMode { mode: 0o666, .. })
        ));
        let plain = dir.path().join("plain");
        std::fs::write(&plain, "").unwrap();
        assert!(matches!(
            check_agmend_socket_for(&plain, uid),
            Err(AgmendSocketError::NotSocket { .. })
        ));
        assert!(matches!(
            check_agmend_socket_for(&dir.path().join("missing"), uid),
            Err(AgmendSocketError::Missing)
        ));
    }

    #[test]
    fn agmend_socket_is_in_the_state_folder() {
        // agmend binds here and the desktop app connects here (Task 7).
        assert_eq!(
            agmend_socket().unwrap(),
            state_dir().unwrap().join("agmend.sock")
        );
        assert_eq!(AGMEND_SOCKET, "agmend.sock");
    }
}
