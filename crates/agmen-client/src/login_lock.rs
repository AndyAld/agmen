//! The process-shared lock for login, logout and renewal writes.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::LoginError;

/// How long a token renewal waits for another login transaction. It has a
/// request waiting on it. Giving up drops one renewed pair, which can leave a
/// spent refresh token, so the person may need `agmen login` again.
pub(super) const RENEWAL_LOCK_TIMEOUT: Duration = Duration::from_secs(1);
/// How long a login or logout waits. A person runs it, and a failure costs a
/// browser step or a retry, so it can wait out a slow keychain write.
pub(super) const SIGN_IN_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub(super) struct LoginLock(
    #[expect(dead_code, reason = "the file holds its lock until dropped")] File,
);

impl LoginLock {
    /// Takes the lock, polling for up to `timeout` while another process
    /// holds it. Call only on a blocking thread.
    ///
    /// # Errors
    ///
    /// [`LoginError::LockTimeout`] when another process holds it too long,
    /// and [`LoginError::LockUnusable`] when the file cannot be opened or
    /// locked for any other reason. A login or logout passes both on and
    /// changes nothing; only a renewal may carry on without an unusable lock.
    pub(super) fn hold(login_file: &Path, timeout: Duration) -> Result<Self, LoginError> {
        let path = login_file.with_file_name("login.lock");
        let file = open(&path).map_err(|error| LoginError::LockUnusable {
            path: path.clone(),
            error,
        })?;
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => {
                    recovered(&path);
                    return Ok(Self(file));
                }
                Err(TryLockError::WouldBlock) => {
                    let remaining = timeout.saturating_sub(start.elapsed());
                    if remaining.is_zero() {
                        return Err(LoginError::LockTimeout { path, timeout });
                    }
                    std::thread::sleep(LOCK_POLL_INTERVAL.min(remaining));
                }
                Err(TryLockError::Error(error)) => {
                    return Err(LoginError::LockUnusable { path, error });
                }
            }
        }
    }
}

/// Lock paths whose last fallback was logged, with the error it logged. A
/// process has one login file, so the cap is never reached in practice.
static FELL_BACK: Mutex<BTreeMap<PathBuf, String>> = Mutex::new(BTreeMap::new());
const FELL_BACK_CAP: usize = 8;

/// Records that the renewal went on without the lock at `path` because of
/// `error`, and says whether to warn: the first time, or when the error
/// differs from the last one logged for it. A renewal repeats this on every
/// expiry, so a repeat belongs at debug.
pub(super) fn fell_back(path: &Path, error: &io::Error) -> bool {
    let mut seen = FELL_BACK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if seen.len() >= FELL_BACK_CAP && !seen.contains_key(path) {
        seen.clear();
    }
    let error = error.to_string();
    seen.insert(path.to_owned(), error.clone())
        .is_none_or(|last| last != error)
}

/// Records that the lock at `path` worked, so its next failure warns again.
fn recovered(path: &Path) {
    FELL_BACK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(path);
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
