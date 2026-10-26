//! The identity file, `.agmen/identity.json` at a git worktree's root, and
//! the test that decides whether agmen may use it (rulings R42, R42a and
//! R42b). `agmen mcp`, `agmen watch` and every CLI command share this code;
//! the Claude Code plugin applies the same test in TypeScript.
//!
//! A cloned repository could carry an identity file whose hub URL sends the
//! user's token elsewhere, so the file is trusted only on a positive test
//! of the exact file opened: it sits at the worktree root; the root lists
//! an entry stored as exactly `.agmen` that is a folder and not a link, and
//! that folder an entry stored as exactly `identity.json` that is a file
//! and not a link; its real path is the root's real path plus
//! `.agmen/identity.json`; and `git check-ignore` says git ignores it, so
//! git does not track it. The stored-name check matters on a
//! case-insensitive file system, where a committed `.AGMEN/identity.json`
//! beside a committed `.gitignore` naming `.agmen/` passes check-ignore.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use agmen_proto::{IdentityId, ProjectId, TenantId, Topics};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::event::Me;
use crate::session::Broker;

/// The folder at the worktree root that holds the identity file.
pub const IDENTITY_DIR: &str = ".agmen";
/// The identity file's name in [`IDENTITY_DIR`].
pub const IDENTITY_FILE: &str = "identity.json";

/// How long one git call may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);
/// The most folders the search walks from the working directory up.
const WALK_MAX: usize = 64;
/// The largest identity file read; a real one is a few hundred bytes.
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// What `.agmen/identity.json` holds: seven fields, all required, which
/// `agmen join` writes from the hub's answers. Fields this version does not
/// know are ignored, so a later version can add some.
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let identity: agmen_client::IdentityFile = serde_json::from_str(r#"{
///     "tenant": "tenant01", "project": "project1", "identity_id": "idn7k2q9",
///     "handle": "andy/sol", "owner_id": "andyandy",
///     "hub_url": "https://api.agmen.example",
///     "broker_url": "wss://agmen.example/mqtt"
/// }"#)?;
/// assert_eq!(identity.broker_url.to_string(), "wss://agmen.example/mqtt");
/// # Ok(()) }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdentityFile {
    /// The tenant the project belongs to.
    pub tenant: TenantId,
    /// The project.
    pub project: ProjectId,
    /// The identity this worktree acts as.
    pub identity_id: IdentityId,
    /// Its handle, such as `andy/sol`.
    pub handle: String,
    /// The person who owns it.
    pub owner_id: IdentityId,
    /// The hub that minted it. Its token is stored under this URL's origin.
    pub hub_url: Url,
    /// The broker its sessions connect to, as the hub's `GET /v1/me` gave
    /// it (`AGMEN_PUBLIC_MQTT_URL`, ruling R57): `mqtt://`, `mqtts://` or
    /// `wss://`. Any other address fails to parse, so the file is refused.
    pub broker_url: Broker,
}

impl IdentityFile {
    /// The project's topics.
    #[must_use]
    pub fn topics(&self) -> Topics {
        Topics::new(self.tenant.clone(), self.project.clone())
    }

    /// Who a session with this file runs as.
    #[must_use]
    pub fn me(&self) -> Me {
        Me {
            identity_id: self.identity_id.clone(),
            handle: self.handle.clone(),
            owner_id: self.owner_id.clone(),
        }
    }
}

/// A trusted identity file and the worktree it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Joined {
    /// The worktree root, as git names it.
    pub root: PathBuf,
    /// The file's contents.
    pub identity: IdentityFile,
}

/// Why no identity could be loaded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IdentityError {
    /// An identity file was found but failed the trust test.
    #[error(
        "agmen does not use {path}: {why}. An identity file that came with the repository could \
         send your agmen token to another hub. Run agmen join at the worktree root (it writes \
         .agmen/identity.json and adds .agmen/ to .git/info/exclude) and remove any copy git tracks."
    )]
    Untrusted {
        /// The file.
        path: PathBuf,
        /// Which part of the test it failed.
        why: String,
    },
    /// git could not be run or did not answer.
    #[error("git {0}")]
    Git(String),
    /// The file could not be read.
    #[error("reading {path} failed")]
    Read {
        /// The file.
        path: PathBuf,
        /// What failed.
        #[source]
        source: io::Error,
    },
    /// The file is not a valid identity file.
    #[error("{path} is not a valid identity file")]
    Parse {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        #[source]
        source: serde_json::Error,
    },
}

/// Finds the identity for the working directory `cwd`: the nearest
/// `.agmen/identity.json` from `cwd` up to its git worktree root and no
/// further, used only when it passes the trust test (see the module
/// docs). `Ok(None)` means not joined: `cwd` is not in a git worktree, or
/// no identity file is on the way up.
///
/// This runs git and reads files; call it off the async runtime's worker
/// threads.
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// match agmen_client::find_identity(&std::env::current_dir()?)? {
///     Some(joined) => println!("{} in {}", joined.identity.handle, joined.root.display()),
///     None => println!("not joined: run agmen join <invite> in this worktree"),
/// }
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// [`IdentityError::Untrusted`] for a file that fails the trust test,
/// [`IdentityError::Git`] when git cannot be run, and
/// [`IdentityError::Read`] or [`IdentityError::Parse`] for a trusted file
/// that cannot be read.
pub fn find_identity(cwd: &Path) -> Result<Option<Joined>, IdentityError> {
    let Some((root, prefix)) = worktree(cwd)? else {
        return Ok(None);
    };
    // The folders from the root down to cwd, deepest first.
    let mut folders = vec![root.clone()];
    for part in Path::new(&prefix).components().take(WALK_MAX) {
        let next = folders
            .last()
            .map_or_else(PathBuf::new, |dir| dir.join(part));
        folders.push(next);
    }
    for dir in folders.iter().rev() {
        let path = dir.join(IDENTITY_DIR).join(IDENTITY_FILE);
        if fs::symlink_metadata(&path).is_err() {
            continue;
        }
        if *dir != root {
            return Err(IdentityError::Untrusted {
                why: format!("it is not at the worktree root, {}", root.display()),
                path,
            });
        }
        let identity = load_trusted(&root, &path)?;
        return Ok(Some(Joined { root, identity }));
    }
    Ok(None)
}

/// The worktree root and the path from it to `cwd`, or `None` outside a
/// git worktree.
fn worktree(cwd: &Path) -> Result<Option<(PathBuf, String)>, IdentityError> {
    let output = git(cwd, &["rev-parse", "--show-toplevel", "--show-prefix"])?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| IdentityError::Git("printed a path that is not UTF-8".to_owned()))?;
    let mut lines = stdout.lines();
    let root = lines.next().filter(|root| !root.is_empty());
    let Some(root) = root else {
        // A bare repository or the .git folder itself: no worktree.
        return Ok(None);
    };
    Ok(Some((
        PathBuf::from(root),
        lines.next().unwrap_or_default().to_owned(),
    )))
}

/// Runs the trust test on `<root>/.agmen/identity.json` (`path`) and reads
/// the file it tested.
fn load_trusted(root: &Path, path: &Path) -> Result<IdentityFile, IdentityError> {
    let untrusted = |why: String| IdentityError::Untrusted {
        path: path.to_owned(),
        why,
    };
    let read_err = |source| IdentityError::Read {
        path: path.to_owned(),
        source,
    };
    // Open first, then test, then check that the file opened is the file
    // tested, so a swap in between cannot slip another file through.
    let mut file = File::open(path).map_err(read_err)?;
    if let Some(why) = untrusted_because(root)? {
        return Err(untrusted(why));
    }
    if !same_file(&file, path).map_err(read_err)? {
        return Err(untrusted(
            "it changed while agmen was checking it".to_owned(),
        ));
    }
    let mut json = Vec::new();
    file.by_ref()
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut json)
        .map_err(read_err)?;
    if json.len() as u64 > MAX_FILE_BYTES {
        return Err(untrusted(format!(
            "it is larger than {MAX_FILE_BYTES} bytes"
        )));
    }
    serde_json::from_slice(&json).map_err(|source| IdentityError::Parse {
        path: path.to_owned(),
        source,
    })
}

/// Why `<root>/.agmen/identity.json` is not a file agmen may use, or
/// `None` when it is.
fn untrusted_because(root: &Path) -> Result<Option<String>, IdentityError> {
    let dir = match stored_entry(root, IDENTITY_DIR) {
        Ok(entry) => entry,
        Err(why) => return Ok(Some(why)),
    };
    if dir.is_symlink() {
        return Ok(Some(format!("{IDENTITY_DIR} is a symbolic link")));
    }
    if !dir.is_dir() {
        return Ok(Some(format!("{IDENTITY_DIR} is not a folder")));
    }
    let file = match stored_entry(&root.join(IDENTITY_DIR), IDENTITY_FILE) {
        Ok(entry) => entry,
        Err(why) => return Ok(Some(why)),
    };
    if file.is_symlink() {
        return Ok(Some(format!("{IDENTITY_FILE} is a symbolic link")));
    }
    if !file.is_file() {
        return Ok(Some(format!("{IDENTITY_FILE} is not a file")));
    }

    let relative = Path::new(IDENTITY_DIR).join(IDENTITY_FILE);
    let landed = fs::canonicalize(root.join(&relative)).ok();
    let expected = fs::canonicalize(root).ok().map(|root| root.join(&relative));
    if landed.is_none() || landed != expected {
        let expected = expected.unwrap_or_else(|| root.join(&relative));
        return Ok(Some(format!("it does not land at {}", expected.display())));
    }

    let ignored = git(
        root,
        &[
            "check-ignore",
            "-q",
            "--",
            &format!("{IDENTITY_DIR}/{IDENTITY_FILE}"),
        ],
    )?;
    // check-ignore exits 0 for an ignored path, 1 for one it does not
    // ignore (a tracked file never is), 128 on an error.
    Ok(match ignored.status.code() {
        Some(0) => None,
        Some(1) => Some("git does not ignore it, so it may be tracked by git".to_owned()),
        Some(code) => Some(format!("git check-ignore exited {code}")),
        None => Some("git check-ignore was killed".to_owned()),
    })
}

/// The type of the entry in `folder` whose stored name is exactly `name`,
/// byte for byte, or why there is none.
fn stored_entry(folder: &Path, name: &str) -> Result<fs::FileType, String> {
    let entries =
        fs::read_dir(folder).map_err(|_| format!("{} cannot be listed", folder.display()))?;
    for entry in entries {
        let entry = entry.map_err(|_| format!("{} cannot be listed", folder.display()))?;
        if entry.file_name() == OsStr::new(name) {
            // DirEntry::file_type does not follow a link.
            return entry
                .file_type()
                .map_err(|_| format!("{name} cannot be inspected"));
        }
    }
    Err(format!(
        "{} has no entry named exactly {name}, so the file is reached through another spelling \
         (a case or Unicode alias)",
        folder.display()
    ))
}

/// Whether the open `file` is the file at `path` now.
#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let (opened, named) = (file.metadata()?, fs::symlink_metadata(path)?);
    Ok(opened.dev() == named.dev() && opened.ino() == named.ino())
}

/// Whether the open `file` is the file at `path` now. Without inode numbers
/// this compares what it can: both are files of the same length.
#[cfg(not(unix))]
fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    let (opened, named) = (file.metadata()?, fs::symlink_metadata(path)?);
    Ok(named.is_file() && opened.len() == named.len())
}

/// Runs `git <args>` in `dir`, waiting at most [`GIT_TIMEOUT`].
fn git(dir: &Path, args: &[&str]) -> Result<Output, IdentityError> {
    const POLL: Duration = Duration::from_millis(10);
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| IdentityError::Git(format!("could not run: {err}")))?;
    let deadline = Instant::now() + GIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                // The kill error does not matter: the child is reported as
                // timed out either way, and wait reaps it.
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "a failed kill means the child already exited"
                )]
                let _ = child.kill();
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "reaping only; the call already failed"
                )]
                let _ = child.wait();
                return Err(IdentityError::Git(format!(
                    "{} did not answer within {} s",
                    args.first().unwrap_or(&""),
                    GIT_TIMEOUT.as_secs()
                )));
            }
            Err(err) => return Err(IdentityError::Git(format!("could not be waited on: {err}"))),
        }
    }
    child
        .wait_with_output()
        .map_err(|err| IdentityError::Git(format!("output could not be read: {err}")))
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
