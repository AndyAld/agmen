//! The local API socket and the lock that keeps one agmend per user.
//!
//! The lock file sits beside the socket (`agmend.lock` next to
//! `agmend.sock`). agmend takes an exclusive `flock` on it before it
//! touches the socket and holds it while it runs, so a second agmend fails
//! with [`AgmendError::AlreadyRunning`] and leaves the first one's socket
//! alone; a socket left by a crashed agmend is replaced. The folder is
//! made 0700 when agmend creates it, the socket is set to 0600 right after
//! bind, and each connection's peer must run as the user that owns the lock
//! file, so another user cannot talk to agmend even in the moment between
//! bind and chmod.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};

use crate::AgmendError;

/// A bound socket and the lock that guards it. Dropping it removes the
/// socket, then releases the lock.
#[derive(Debug)]
pub(crate) struct Bound {
    pub(crate) listener: UnixListener,
    path: PathBuf,
    uid: u32,
    _lock: File,
}

impl Bound {
    /// Whether `stream`'s peer is this user.
    pub(crate) fn is_own(&self, stream: &UnixStream) -> bool {
        stream.peer_cred().is_ok_and(|cred| cred.uid() == self.uid)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        // The lock is still held here, so the file is this agmend's.
        if let Err(err) = fs::remove_file(&self.path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.path.display(), error = %err, "removing the socket failed");
        }
    }
}

fn io(what: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> AgmendError {
    let path = path.to_owned();
    move |source| AgmendError::Io { what, path, source }
}

/// Takes the lock beside `socket`, then binds `socket` with mode 0600.
/// Must run inside a tokio runtime.
pub(crate) fn bind(socket: &Path) -> Result<Bound, AgmendError> {
    let dir = socket
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| AgmendError::SocketPath(socket.to_owned()))?;
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(io("creating the socket folder", dir))?;
    }
    let lock_path = socket.with_extension("lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(io("opening the lock file", &lock_path))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(AgmendError::AlreadyRunning { lock: lock_path });
        }
        Err(TryLockError::Error(err)) => return Err(io("locking", &lock_path)(err)),
    }
    let uid = lock
        .metadata()
        .map_err(io("reading the lock file", &lock_path))?
        .uid();
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(io("removing the old socket", socket)(err)),
    }
    let listener = UnixListener::bind(socket).map_err(io("binding the socket", socket))?;
    let bound = Bound {
        listener,
        path: socket.to_owned(),
        uid,
        _lock: lock,
    };
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
        .map_err(io("setting the socket's mode", socket))?;
    Ok(bound)
}
