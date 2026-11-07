//! Bounded, best-effort containment of same-user socket peers by ancestry.
//!
//! A process that deliberately daemonizes by double forking to pid 1, or whose
//! parent exits first, still escapes. Same-user containment is not a security
//! boundary.

#[cfg(any(target_os = "macos", test))]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

const ANCESTRY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ANCESTOR_HOPS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    Read(std::io::ErrorKind),
    InvalidSnapshot,
    #[cfg(target_os = "macos")]
    MissingProcess,
    InvalidAncestry,
    DepthLimit,
    #[cfg(target_os = "macos")]
    SnapshotExit,
    ProcessGroup(rustix::io::Errno),
    TimedOut,
}

#[derive(Debug, thiserror::Error)]
#[error("{detail}")]
pub(crate) struct Error {
    cause: Cause,
    detail: String,
}

impl Error {
    fn new(cause: Cause, detail: impl Into<String>) -> Self {
        Self {
            cause,
            detail: detail.into(),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::new(Cause::Read(error.kind()), error.to_string())
    }
}

#[derive(Debug, Default)]
pub(crate) struct Guard {
    failure: Mutex<Option<Cause>>,
    /// Test seam: each walk first takes a permit from here, so a test decides when it ends.
    #[cfg(test)]
    pub(crate) gate: Mutex<Option<std::sync::Arc<tokio::sync::Semaphore>>>,
}

impl Guard {
    #[cfg(test)]
    async fn hold(&self) {
        let gate = self
            .gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(gate) = gate
            && let Ok(permit) = gate.acquire().await
        {
            permit.forget();
        }
    }

    pub(crate) async fn contains_peer(
        &self,
        peer: std::io::Result<i32>,
        groups: &BTreeSet<i32>,
    ) -> bool {
        self.check(groups, async {
            #[cfg(test)]
            self.hold().await;
            let pid = peer?;
            for ancestor in ancestor_pids(pid).await? {
                if contains_pid(groups, ancestor)? {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .await
    }

    async fn check(
        &self,
        groups: &BTreeSet<i32>,
        walk: impl Future<Output = Result<bool, Error>>,
    ) -> bool {
        if groups.is_empty() {
            return false;
        }
        let result = tokio::time::timeout(ANCESTRY_TIMEOUT, walk)
            .await
            .unwrap_or_else(|_| {
                Err(Error::new(
                    Cause::TimedOut,
                    "peer ancestry walk exceeded 2 seconds",
                ))
            });
        let mut previous = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match result {
            Ok(contained) => {
                *previous = None;
                contained
            }
            Err(error) => {
                if previous.replace(error.cause) == Some(error.cause) {
                    tracing::debug!(cause = ?error.cause, reason = %error, "peer ancestry lookup failed again; owner-only calls refused");
                } else {
                    tracing::warn!(cause = ?error.cause, reason = %error, "peer ancestry lookup failed; owner-only calls refused");
                }
                true
            }
        }
    }
}

/// One connection's peer. Its ancestry walk can take as long as a client waits for `hello`,
/// so it runs when the first request that only the person may make arrives, and the answer
/// stays with the connection.
#[derive(Debug)]
pub(crate) struct Peer {
    pid: Option<std::io::Result<i32>>,
    groups: BTreeSet<i32>,
    managed: bool,
    owner: Option<bool>,
}

impl Peer {
    /// `groups` are the process groups of the agents running now, and `managed` says
    /// whether the peer itself sits in one.
    pub(crate) fn new(pid: std::io::Result<i32>, groups: BTreeSet<i32>, managed: bool) -> Self {
        Self {
            pid: Some(pid),
            groups,
            managed,
            owner: None,
        }
    }

    /// Whether the peer is the person's own process, which walks its ancestry the first time.
    pub(crate) async fn is_owner(&mut self, guard: &Guard) -> bool {
        if let Some(owner) = self.owner {
            return owner;
        }
        // A walk dropped half way took the pid with it: refuse.
        let owner = match self.pid.take() {
            Some(pid) => {
                let known = pid.is_ok();
                // The walk runs even when `managed` refuses, so its failures are logged.
                let contained = guard.contains_peer(pid, &self.groups).await;
                known && !self.managed && !contained
            }
            None => false,
        };
        self.owner = Some(owner);
        owner
    }
}

pub(crate) fn groups(groups: &Mutex<BTreeSet<i32>>) -> BTreeSet<i32> {
    groups
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub(crate) fn contains_pid(groups: &BTreeSet<i32>, pid: i32) -> Result<bool, Error> {
    if groups.is_empty() {
        return Ok(false);
    }
    if groups.contains(&pid) {
        return Ok(true);
    }
    let pid = rustix::process::Pid::from_raw(pid)
        .ok_or_else(|| Error::new(Cause::InvalidSnapshot, "invalid peer pid"))?;
    let group = rustix::process::getpgid(Some(pid)).map_err(|error| {
        Error::new(
            Cause::ProcessGroup(error),
            format!("reading process group for {pid}: {error}"),
        )
    })?;
    Ok(groups.contains(&group.as_raw_nonzero().get()))
}

async fn ancestor_pids(pid: i32) -> Result<Vec<i32>, Error> {
    #[cfg(target_os = "macos")]
    let parents = snapshot().await?;
    #[cfg(target_os = "macos")]
    let parents = &parents;
    walk(pid, std::process::id(), |current| async move {
        #[cfg(target_os = "macos")]
        {
            parents.get(&current).copied().ok_or_else(|| {
                Error::new(
                    Cause::MissingProcess,
                    format!("pid {current} missing from process snapshot"),
                )
            })
        }
        #[cfg(target_os = "linux")]
        {
            let stat = tokio::fs::read_to_string(format!("/proc/{current}/stat"))
                .await
                .map_err(|error| {
                    Error::new(
                        Cause::Read(error.kind()),
                        format!("reading /proc/{current}/stat: {error}"),
                    )
                })?;
            stat_parent(&stat)
        }
    })
    .await
}

async fn walk<F, Step>(mut current: i32, daemon: u32, mut parent: F) -> Result<Vec<i32>, Error>
where
    F: FnMut(i32) -> Step,
    Step: Future<Output = Result<i32, Error>>,
{
    let mut ancestors = Vec::with_capacity(MAX_ANCESTOR_HOPS + 1);
    for hop in 0..=MAX_ANCESTOR_HOPS {
        if current <= 0 || ancestors.contains(&current) {
            return Err(Error::new(
                Cause::InvalidAncestry,
                "invalid or cyclic process ancestry",
            ));
        }
        ancestors.push(current);
        if current == 1 || u32::try_from(current).ok() == Some(daemon) {
            return Ok(ancestors);
        }
        if hop == MAX_ANCESTOR_HOPS {
            break;
        }
        current = parent(current).await?;
    }
    Err(Error::new(
        Cause::DepthLimit,
        "process ancestry exceeded 32 hops",
    ))
}

#[cfg(target_os = "macos")]
async fn snapshot() -> Result<BTreeMap<i32, i32>, Error> {
    // Not `Command::output`, which pipes stderr again whatever was set before.
    let output = tokio::process::Command::new("/bin/ps")
        .args(["-A", "-o", "pid=,ppid="])
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?
        .wait_with_output()
        .await?;
    if !output.status.success() {
        return Err(Error::new(
            Cause::SnapshotExit,
            format!("/bin/ps exited with {}", output.status),
        ));
    }
    parse_snapshot(&output.stdout)
}

#[cfg(any(target_os = "macos", test))]
fn parse_snapshot(bytes: &[u8]) -> Result<BTreeMap<i32, i32>, Error> {
    let invalid = || Error::new(Cause::InvalidSnapshot, "unreadable process snapshot");
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut parents = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let pid: i32 = fields
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        let parent: i32 = fields
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        if pid <= 0
            || parent < 0
            || fields.next().is_some()
            || parents.insert(pid, parent).is_some()
        {
            return Err(invalid());
        }
    }
    if parents.is_empty() {
        return Err(invalid());
    }
    Ok(parents)
}

#[cfg(any(target_os = "linux", test))]
fn stat_parent(stat: &str) -> Result<i32, Error> {
    stat.rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(1))
        .and_then(|field| field.parse::<i32>().ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| Error::new(Cause::InvalidSnapshot, "unreadable /proc process stat"))
}

#[cfg(test)]
#[path = "peer_walk_tests.rs"]
mod tests;
