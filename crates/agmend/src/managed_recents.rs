//! Stop times retained beside each managed agent's record.

use std::path::Path;

use agmen_proto::{IdentityId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::Managed;
use super::actions::{read_bounded, replace_private_json};

pub(super) const STOPPED_FILE: &str = "stopped.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stopped {
    pub(super) stopped_at: Timestamp,
}

pub(super) fn read_stopped(dir: &Path) -> Option<Timestamp> {
    let path = dir.join(STOPPED_FILE);
    match read_bounded(&path) {
        Ok(bytes) => match serde_json::from_slice::<Stopped>(&bytes) {
            Ok(stopped) => return Some(stopped.stopped_at),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "invalid agent stop mark; using last activity");
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "reading agent stop mark failed; using last activity");
        }
    }
    last_seen(dir)
}

pub(super) fn last_seen(dir: &Path) -> Option<Timestamp> {
    let modified = std::fs::metadata(dir.join("status.json"))
        .and_then(|metadata| metadata.modified())
        .or_else(|_| {
            std::fs::metadata(dir.join("agent.json")).and_then(|metadata| metadata.modified())
        });
    // Activity is unknown if neither file has a readable time in Timestamp's range.
    modified
        .ok()
        .and_then(|time| Timestamp::try_from(time).ok())
}

impl Managed {
    pub(super) async fn mark_stopped(&self, id: &IdentityId) {
        let stopped_at = Timestamp::now();
        let worker = self.workers.lock().await.get(id).cloned();
        if let Some(worker) = worker {
            worker.lock().await.stopped_at = Some(stopped_at);
        }
        let path = self.agent_dir(id).join(STOPPED_FILE);
        match tokio::task::spawn_blocking(move || {
            replace_private_json(&path, &json!(Stopped { stopped_at }))
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, identity = %id, "writing agent stop mark failed")
            }
            Err(error) => tracing::warn!(%error, identity = %id, "writing agent stop mark stopped"),
        }
    }

    pub(super) async fn clear_stopped(&self, id: &IdentityId) {
        let worker = self.workers.lock().await.get(id).cloned();
        if let Some(worker) = worker {
            worker.lock().await.stopped_at = None;
        }
        let path = self.agent_dir(id).join(STOPPED_FILE);
        match tokio::task::spawn_blocking(move || std::fs::remove_file(path)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, identity = %id, "removing agent stop mark failed")
            }
            Err(error) => {
                tracing::warn!(%error, identity = %id, "removing agent stop mark stopped")
            }
        }
    }
}
