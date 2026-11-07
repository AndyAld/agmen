//! The process-shared lock for login, logout and renewal writes.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use super::LoginError;

/// Upper bound on waiting for another login transaction.
pub(super) const LOGIN_LOCK_TIMEOUT: Duration = Duration::from_secs(1);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub(super) struct LoginLock(
    #[expect(dead_code, reason = "the file holds its lock until dropped")] File,
);

impl LoginLock {
    /// Takes the lock, polling for up to [`LOGIN_LOCK_TIMEOUT`] while another
    /// process holds it. Call only on a blocking thread.
    ///
    /// # Errors
    ///
    /// [`LoginError::LockTimeout`] when another process holds it too long,
    /// and [`LoginError::LockUnusable`] when the file cannot be opened or
    /// locked for any other reason. A login or logout passes both on and
    /// changes nothing; only a renewal may carry on without an unusable lock.
    pub(super) fn hold(login_file: &Path) -> Result<Self, LoginError> {
        let path = login_file.with_file_name("login.lock");
        let file = open(&path).map_err(|source| LoginError::LockUnusable {
            path: path.clone(),
            source,
        })?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self(file)),
                Err(TryLockError::WouldBlock) => {
                    let remaining = LOGIN_LOCK_TIMEOUT.saturating_sub(start.elapsed());
                    if remaining.is_zero() {
                        return Err(LoginError::LockTimeout {
                            path,
                            timeout: LOGIN_LOCK_TIMEOUT,
                        });
                    }
                    std::thread::sleep(LOCK_POLL_INTERVAL.min(remaining));
                }
                Err(TryLockError::Error(source)) => {
                    return Err(LoginError::LockUnusable { path, source });
                }
            }
        }
    }
}

fn open(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Keep the inode after the guard drops. Unlinking it could let another
    // process lock a different inode while this one is still held.
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(file)
}
