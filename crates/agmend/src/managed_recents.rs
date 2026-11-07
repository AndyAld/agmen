//! Stop times and resuming saved agent trees.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use agmen_local::ApiError;
use agmen_proto::{IdentityId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::actions::{Target, read_bounded, replace_private_json};
use super::{Managed, error};
use crate::console::bad_request;

pub(super) const STOPPED_FILE: &str = "stopped.json";
pub(super) const RESUME_TREE_WAIT: Duration = Duration::from_secs(30);

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
    /// Caller holds `launching`; parent reservations must precede child starts.
    pub(super) async fn start_tree_locked(
        self: &Arc<Self>,
        target: Target,
    ) -> Result<Value, ApiError> {
        let deadline = tokio::time::Instant::now() + RESUME_TREE_WAIT;
        let root = target.identity.clone();
        let mut abandoned = self.abandon_launch.subscribe();
        let mut answer = tokio::select! {
            biased;
            _ = abandoned.wait_for(|abandoned| *abandoned) => Err(bad_request("daemon is shutting down")),
            _ = tokio::time::sleep_until(deadline) => Err(bad_request("deadline")),
            result = self.start_one_locked(target) => result,
        }?;
        let mut started = vec![root.clone()];
        let mut skipped = Vec::new();
        // tree_levels bounds all levels and members by MAX_MANAGED.
        for id in self.tree_levels(&root).await.into_iter().skip(1).flatten() {
            let result = if self.shutting_down.load(Ordering::Acquire) {
                Err(bad_request("daemon is shutting down"))
            } else {
                tokio::select! {
                    biased;
                    // Quit can abandon a stuck start before its own deadline.
                    _ = abandoned.wait_for(|abandoned| *abandoned) => Err(bad_request("daemon is shutting down")),
                    _ = tokio::time::sleep_until(deadline) => Err(bad_request("deadline")),
                    result = async {
                        let worker = self.workers.lock().await.get(&id).cloned()
                            .ok_or_else(|| error("agent is no longer managed"))?;
                        let held = worker.lock().await;
                        if held.state != "offline" {
                            return Err(bad_request("already running"));
                        }
                        if !held.record.parent.as_ref().is_some_and(|parent| started.contains(parent)) {
                            return Err(bad_request("parent not started"));
                        }
                        drop(held);
                        self.start_one_locked(Target {
                            identity: id.clone(),
                            project: None,
                            root: None,
                        }).await
                    } => result,
                }
            };
            match result {
                Ok(_) => started.push(id),
                Err(failure) => skipped.push(json!({"identity":id, "reason":failure.message})),
            }
        }
        answer["started"] = json!(started);
        answer["skipped"] = json!(skipped);
        Ok(answer)
    }

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
