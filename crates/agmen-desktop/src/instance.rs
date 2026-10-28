//! One running app per person: a second launch hands its arguments to the
//! first and exits.
//!
//! The first launch holds an exclusive lock on `desktop.lock` and listens on
//! `desktop.sock` (mode 0600). A later launch that cannot take the lock
//! connects, writes one JSON line, and exits. A crash releases the lock with
//! the process, so a leftover socket file is simply replaced.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub(crate) const LOCK: &str = "desktop.lock";
pub(crate) const SOCKET: &str = "desktop.sock";
/// A forwarded launch is a short JSON line; anything longer is dropped.
pub(crate) const MAX_FORWARD_BYTES: usize = 64 * 1024;
/// How long a connection may take to send its line.
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// How often a second launch retries while the first is still starting.
const CONNECT_TRIES: u32 = 20;
const CONNECT_PAUSE: Duration = Duration::from_millis(100);
/// The accept loop gives up after this many errors in a row.
const MAX_ACCEPT_ERRORS: u32 = 50;
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(100);

/// What can go wrong taking the instance lock or forwarding to it.
#[derive(Debug, thiserror::Error)]
pub(crate) enum InstanceError {
    #[error("cannot use {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("another agmen-desktop holds {} but does not answer on its socket", path.display())]
    NoAnswer { path: PathBuf },
}

/// A launch forwarded from a second instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Forwarded {
    pub(crate) focus: bool,
    pub(crate) args: Vec<String>,
}

/// Which instance this process is.
#[derive(Debug)]
pub(crate) enum Instance {
    /// The first: keep the listener for the app's life.
    Primary(Listener),
    /// A later launch; its arguments went to the primary.
    Secondary,
}

/// Holds the lock and receives forwarded launches.
#[derive(Debug)]
pub(crate) struct Listener {
    _lock: File,
    forwarded: mpsc::Receiver<Forwarded>,
}

impl Listener {
    /// The next forwarded launch, if one arrives within `timeout`.
    #[cfg(test)]
    pub(crate) fn recv_timeout(&self, timeout: Duration) -> Option<Forwarded> {
        self.forwarded.recv_timeout(timeout).ok()
    }

    /// A forwarded launch that already arrived, without waiting.
    pub(crate) fn try_recv(&self) -> Option<Forwarded> {
        self.forwarded.try_recv().ok()
    }
}

/// Takes the instance lock in `dir`, or forwards `args` to the instance that
/// holds it.
///
/// # Errors
///
/// When `dir` cannot be created or used, or another instance holds the lock
/// and never answers.
pub(crate) fn acquire(dir: &Path, args: &[String]) -> Result<Instance, InstanceError> {
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| InstanceError::Io { path, source }
    };
    // The socket inside accepts launches, so only this user may reach it.
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(io(dir))?;
    let lock_path = dir.join(LOCK);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(io(&lock_path))?;
    let socket = dir.join(SOCKET);
    match lock.try_lock() {
        Ok(()) => listen(lock, &socket).map(Instance::Primary),
        Err(TryLockError::WouldBlock) => match forward(&socket, args) {
            Ok(()) => Ok(Instance::Secondary),
            // The holder may have been exiting; if the lock is free now, take over.
            Err(InstanceError::NoAnswer { .. }) if lock.try_lock().is_ok() => {
                listen(lock, &socket).map(Instance::Primary)
            }
            Err(err) => Err(err),
        },
        Err(TryLockError::Error(source)) => Err(InstanceError::Io {
            path: lock_path,
            source,
        }),
    }
}

fn listen(lock: File, socket: &Path) -> Result<Listener, InstanceError> {
    let io = |source| InstanceError::Io {
        path: socket.to_owned(),
        source,
    };
    // We hold the lock, so any socket file left here is from a crashed run.
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(io(err)),
    }
    let listener = UnixListener::bind(socket).map_err(io)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600)).map_err(io)?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("agmen-instance".into())
        .spawn(move || {
            let mut errors_in_a_row = 0_u32;
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    // A persistent error (EMFILE, say) must not spin a core.
                    errors_in_a_row += 1;
                    if errors_in_a_row >= MAX_ACCEPT_ERRORS {
                        eprintln!("agmen-desktop: stopped taking second launches after {errors_in_a_row} errors");
                        break;
                    }
                    std::thread::sleep(ACCEPT_ERROR_PAUSE);
                    continue;
                };
                errors_in_a_row = 0;
                if let Some(forwarded) = read_forward(stream)
                    && tx.send(forwarded).is_err()
                {
                    // The app dropped its listener; stop accepting.
                    break;
                }
            }
        })
        .map_err(io)?;
    Ok(Listener {
        _lock: lock,
        forwarded: rx,
    })
}

/// One JSON line of at most [`MAX_FORWARD_BYTES`], or nothing.
fn read_forward(stream: UnixStream) -> Option<Forwarded> {
    // macOS refuses the option (EINVAL) once the peer has closed, which a
    // quick second launch does; reads then end at once, so nothing can stall.
    drop(stream.set_read_timeout(Some(READ_TIMEOUT)));
    let limit = u64::try_from(MAX_FORWARD_BYTES).ok()?;
    let mut reader = BufReader::new(stream.take(limit));
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).ok()?;
    if line.last() != Some(&b'\n') {
        return None;
    }
    serde_json::from_slice(&line).ok()
}

fn forward(socket: &Path, args: &[String]) -> Result<(), InstanceError> {
    let message = Forwarded {
        focus: true,
        args: args.to_vec(),
    };
    let encode = |message: &Forwarded| {
        let mut line = serde_json::to_vec(message).map_err(|err| InstanceError::Io {
            path: socket.to_owned(),
            source: err.into(),
        })?;
        line.push(b'\n');
        Ok::<_, InstanceError>(line)
    };
    let mut line = encode(&message)?;
    if line.len() > MAX_FORWARD_BYTES {
        // Too long to forward: still bring the first instance forward.
        line = encode(&Forwarded {
            focus: true,
            args: Vec::new(),
        })?;
    }
    for _ in 0..CONNECT_TRIES {
        if let Ok(mut stream) = UnixStream::connect(socket) {
            return stream.write_all(&line).map_err(|source| InstanceError::Io {
                path: socket.to_owned(),
                source,
            });
        }
        // The primary may hold the lock but not be listening yet.
        std::thread::sleep(CONNECT_PAUSE);
    }
    Err(InstanceError::NoAnswer {
        path: socket.to_owned(),
    })
}

#[cfg(test)]
#[path = "instance_tests.rs"]
mod tests;
