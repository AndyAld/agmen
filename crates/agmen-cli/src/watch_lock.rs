//! Shared locks held by `agmen watch` in the identity's state folder.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use anyhow::Context as _;

use crate::client::blocking;

pub(crate) const LOCK_FILE: &str = "watch.lock";

#[derive(Debug)]
pub(crate) struct WatchLock(
    #[expect(dead_code, reason = "the file holds its shared lock until dropped")] File,
);

impl WatchLock {
    pub(crate) async fn hold(state_dir: &Path) -> anyhow::Result<Self> {
        let state_dir = state_dir.to_owned();
        blocking(move || {
            let file = open(&state_dir)?;
            file.lock_shared().context("holding watch.lock")?;
            Ok(Self(file))
        })
        .await
    }

    /// As [`WatchLock::hold`], but a lock that cannot be taken (a filesystem
    /// without locks, an odd entry named `watch.lock`) is logged and gives
    /// `None`: the watch runs on without announcing itself, as it did before
    /// the lock existed.
    pub(crate) async fn try_hold(state_dir: &Path) -> Option<Self> {
        match Self::hold(state_dir).await {
            Ok(lock) => Some(lock),
            Err(err) => {
                tracing::warn!(
                    error = format!("{err:#}"),
                    "no watch.lock; agmen mcp may also publish this identity's presence"
                );
                None
            }
        }
    }
}

pub(crate) async fn watch_running(state_dir: &Path) -> anyhow::Result<bool> {
    let state_dir = state_dir.to_owned();
    blocking(move || {
        let file = open(&state_dir)?;
        match file.try_lock() {
            Ok(()) => {
                file.unlock().context("releasing the watch.lock probe")?;
                Ok(false)
            }
            Err(TryLockError::WouldBlock) => Ok(true),
            Err(TryLockError::Error(err)) => Err(err).context("probing watch.lock"),
        }
    })
    .await
}

fn open(state_dir: &Path) -> anyhow::Result<File> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("creating {}", state_dir.display()))?;
    let path = state_dir.join(LOCK_FILE);
    // Keep this inode even after the last watch exits: unlinking it would
    // let a new watch lock a different file while an old watch still runs.
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    // A flock needs only read access, so a file others can open would let
    // another user hold a lock that makes this identity's mcp think a watch
    // runs (and keeps it offline), or that blocks a watch from starting.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = file.metadata().context("reading watch.lock's mode")?;
        if metadata.permissions().mode() & 0o077 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .context("making watch.lock private")?;
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_watch_without_a_lock() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!watch_running(dir.path()).await.unwrap());
    }

    #[tokio::test]
    async fn a_held_lock_means_a_watch_runs() {
        let dir = tempfile::tempdir().unwrap();
        let _watch = WatchLock::hold(dir.path()).await.unwrap();
        assert!(watch_running(dir.path()).await.unwrap());
    }

    #[tokio::test]
    async fn two_watches_share_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let first = WatchLock::hold(dir.path()).await.unwrap();
        let second = WatchLock::hold(dir.path()).await.unwrap();
        assert!(watch_running(dir.path()).await.unwrap());
        drop(first);
        assert!(watch_running(dir.path()).await.unwrap());
        drop(second);
        assert!(!watch_running(dir.path()).await.unwrap());
    }

    #[tokio::test]
    async fn dropping_the_lock_ends_it() {
        let dir = tempfile::tempdir().unwrap();
        let watch = WatchLock::hold(dir.path()).await.unwrap();
        assert!(watch_running(dir.path()).await.unwrap());
        drop(watch);
        assert!(!watch_running(dir.path()).await.unwrap());
        assert!(dir.path().join(LOCK_FILE).exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_lock_file_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE);
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let _fresh = WatchLock::hold(dir.path()).await.unwrap();
        assert_eq!(
            mode(&path),
            0o600,
            "another user could lock a world-readable file"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _loose = WatchLock::hold(dir.path()).await.unwrap();
        assert_eq!(mode(&path), 0o600);
    }

    #[tokio::test]
    async fn an_unusable_lock_file_means_no_lock_not_a_failed_watch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(LOCK_FILE)).unwrap();
        assert!(WatchLock::hold(dir.path()).await.is_err());
        assert!(WatchLock::try_hold(dir.path()).await.is_none());
        let usable = tempfile::tempdir().unwrap();
        assert!(WatchLock::try_hold(usable.path()).await.is_some());
    }
}
