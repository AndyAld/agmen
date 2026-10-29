//! Parent authority and admission checks for managed launches.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use agmen_client::{IdentityFile, find_identity};
use agmen_local::safeguards::{MAX_MANAGED, Safeguards};
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{IdentityId, Payload};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Launch, Managed, Placement, Record, Source, Tier, envelope, error, metadata_for, root_depth,
};
use crate::console::{bad_request, params};
use crate::state::ManagedIdentity;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildLaunch {
    name: String,
    role: String,
    model: Option<String>,
    budget_usd: Option<serde_json::Number>,
    brief: Option<String>,
}

impl Managed {
    /// Requires a launched worker of this login, currently serving only its owner.
    async fn tree_caller(&self, caller: &ManagedIdentity) -> Result<Record, ApiError> {
        let refusal = || bad_request("only agents agmen launched can launch agents");
        let worker = self
            .workers
            .lock()
            .await
            .get(&caller.identity_id)
            .cloned()
            .ok_or_else(refusal)?;
        let login = self.hub.credentials().login().await.map_err(error)?;
        let held = worker.lock().await;
        if held.record.source != Source::Launched
            || held.record.identity != *caller
            || caller.owner_id != login.human_id
            || caller.hub_url.origin() != login.hub_url.origin()
        {
            return Err(refusal());
        }
        if held.batch_tier != Some(Tier::Owner) {
            return Err(ApiError {
                code: ErrorCode::Forbidden,
                message: "launching, stopping or briefing agents needs a batch with only your owner's messages; this batch includes other senders".into(),
            });
        }
        Ok(held.record.clone())
    }

    /// Checks tree authority without exposing the worker's private record.
    pub(crate) async fn check_tree_caller(&self, caller: &ManagedIdentity) -> Result<(), ApiError> {
        self.tree_caller(caller).await.map(|_| ())
    }

    /// Launches a child using the authenticated parent's project and checkout.
    pub(crate) async fn launch_child(
        self: &Arc<Self>,
        caller: &ManagedIdentity,
        raw: Value,
    ) -> Result<Value, ApiError> {
        let parent = self.tree_caller(caller).await?;
        let child: ChildLaunch = params(raw)?;
        let launch = Self::validate_launch(json!({
            "project":parent.identity.project, "name":child.name, "role":child.role,
            "model":child.model, "budget_usd":child.budget_usd,
        }))?;
        self.launch_at(launch, parent.worktree.clone(), Some(parent), child.brief)
            .await
    }

    /// Called while `launching` is held, before creating a branch or minting.
    async fn admit(
        &self,
        parent: Option<&Record>,
        owner: &IdentityId,
        safeguards: &Safeguards,
    ) -> Result<u32, ApiError> {
        let workers: Vec<_> = self.workers.lock().await.values().cloned().collect();
        if workers.len() >= MAX_MANAGED {
            return Err(registry_full());
        }
        let depth = match parent {
            Some(parent) => parent
                .depth
                .checked_add(1)
                .ok_or_else(|| bad_request("agent tree depth exceeds its stored range"))?,
            None => root_depth(),
        };
        if safeguards.limit_tree_depth
            && depth > safeguards.max_tree_depth
            && let Some(parent) = parent
        {
            return Err(bad_request(format!(
                "tree depth limit {} reached: {} is at depth {}. Raise or turn off Tree depth in Settings, Agents",
                safeguards.max_tree_depth, parent.identity.handle, parent.depth
            )));
        }
        if safeguards.limit_agents {
            let login = self.hub.credentials().login().await.map_err(error)?;
            let mut running = 0_u32;
            // The registry bounds this snapshot to MAX_MANAGED workers.
            for worker in workers {
                let held = worker.lock().await;
                if held.state != "offline"
                    && held.record.identity.owner_id == *owner
                    && held.record.identity.hub_url.origin() == login.hub_url.origin()
                {
                    running += 1;
                }
            }
            if running >= safeguards.max_agents {
                return Err(bad_request(format!(
                    "{} agents are already running for you. Raise or turn off Agents running per person in Settings, Agents",
                    safeguards.max_agents
                )));
            }
        }
        Ok(depth)
    }

    /// Serializes admission and registration for human and child launches.
    pub(super) async fn launch_at(
        self: &Arc<Self>,
        launch: Launch,
        root: PathBuf,
        parent: Option<Record>,
        brief: Option<String>,
    ) -> Result<Value, ApiError> {
        let agmen = self.runner.mcp_command().await.map_err(error)?;
        let _launching = self.launching.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(error("daemon is shutting down"));
        }
        // A batch may have ended while the request waited for another launch.
        let parent = match parent {
            Some(parent) => Some(self.tree_caller(&parent.identity).await?),
            None => None,
        };
        let settings_file = self.settings_file.clone();
        let safeguards =
            tokio::task::spawn_blocking(move || crate::safeguards::load(settings_file.as_deref()))
                .await
                .map_err(error)??;
        let login = self.hub.credentials().login().await.map_err(error)?;
        let depth = self
            .admit(parent.as_ref(), &login.human_id, &safeguards)
            .await?;
        let brief = brief
            .as_deref()
            .map(|body| envelope(Payload::Dm {}, body))
            .transpose()?;
        let joined = tokio::task::spawn_blocking(move || find_identity(&root))
            .await
            .map_err(error)?
            .map_err(error)?
            .ok_or_else(|| bad_request("checkout has no trusted agmen identity"))?;
        if joined.identity.project != launch.project
            || joined.identity.owner_id != login.human_id
            || joined.identity.hub_url.origin() != login.hub_url.origin()
        {
            return Err(bad_request(
                "checkout is not joined to this project and owner",
            ));
        }
        // A child starts from its parent's worktree, which the parent can
        // write to. Take nothing for the child from a file or a git pointer
        // there that no longer says what agmend recorded at the parent's launch.
        if let Some(parent) = &parent
            && !file_matches_record(&joined.identity, &parent.identity)
        {
            return Err(bad_request(
                "the parent's .agmen/identity.json no longer matches the identity agmend registered for it",
            ));
        }
        let (worker_root, _) = crate::worktree::worktree_root(&joined.root)
            .await
            .map_err(error)?;
        let repo = worker_root
            .parent()
            .ok_or_else(|| error("worker root has no main checkout"))?
            .to_owned();
        if let Some(parent) = &parent
            && parent.repo != repo
        {
            return Err(bad_request(
                "the parent's worktree no longer belongs to the checkout it was launched from",
            ));
        }
        let role = self.launch_role(&repo, &launch, parent.as_ref()).await?;
        let branch = format!(
            "agent/{}-{}",
            launch.name,
            &crate::launch::new_session_id().map_err(error)?[..8]
        );
        let worktree = crate::worktree::create(&joined.root, "HEAD", &branch)
            .await
            .map_err(error)?;
        let result = self
            .complete_launch(
                launch,
                joined,
                login,
                Placement {
                    branch: branch.clone(),
                    worktree: worktree.clone(),
                    parent,
                    depth,
                    brief,
                },
                agmen,
                role,
            )
            .await;
        match result {
            Ok(result) => Ok(result),
            Err(failure) => match crate::worktree::remove(&repo, &worktree, &branch).await {
                Ok(()) => Err(failure),
                Err(cleanup) => Err(error(format!(
                    "{}; retained {} on branch {} because safe cleanup failed: {cleanup:#}",
                    failure.message,
                    worktree.display(),
                    branch
                ))),
            },
        }
    }

    async fn launch_role(
        &self,
        repo: &std::path::Path,
        launch: &Launch,
        parent: Option<&Record>,
    ) -> Result<Option<agmen_roles::Resolved>, ApiError> {
        Ok(match parent {
            Some(parent) => {
                let name = launch
                    .role
                    .as_deref()
                    .ok_or_else(|| bad_request("a child's role is required"))?;
                let parent_role = match &parent.role {
                    Some(name) => {
                        let role = self.resolve_role(repo, name.as_str()).await?;
                        // Same test as `batch_role`: the list is the one the parent was minted with.
                        if parent.role_sha256.as_ref().map(|sha| sha.as_str())
                            != Some(role.sha256.as_str())
                        {
                            return Err(bad_request(format!(
                                "role {name} changed since launch; relaunch the agent to use it"
                            )));
                        }
                        Some(role)
                    }
                    None => None,
                };
                if !parent_role
                    .as_ref()
                    .is_some_and(|role| role.launches.contains(name))
                {
                    let names = parent_role
                        .as_ref()
                        .map(|role| role.launches.iter().cloned().collect::<Vec<_>>().join(", "))
                        .filter(|names| !names.is_empty())
                        .unwrap_or_else(|| "nothing".into());
                    return Err(bad_request(format!(
                        "role {} may not launch {name}; it may launch: {names}",
                        parent.role.as_ref().map_or("(none)", |name| name.as_str())
                    )));
                }
                let role = self.load_role(repo, name).await?;
                if role.kind != agmen_roles::Kind::Agent {
                    return Err(bad_request("a launched agent's role must be an agent role"));
                }
                Some(role)
            }
            None => match &launch.role {
                Some(name) => Some(self.resolve_role(repo, name).await?),
                None => None,
            },
        })
    }

    /// Takes back what a launch made once the hub minted its identity: the
    /// hub's identity and token, agmend's session, row and local credential,
    /// and the agent's folders. Every step runs, so one that fails cannot
    /// leave the others behind; what it could not remove is added to the
    /// launch's own failure.
    pub(super) async fn discard_identity(
        &self,
        identity: &IdentityFile,
        failure: ApiError,
    ) -> ApiError {
        let id = &identity.identity_id;
        let mut left = Vec::new();
        // Revoke first: the hub token is dead whatever else fails.
        if let Err(cause) = self.hub.revoke_identity(id).await {
            left.push(format!("revoking {id}: {cause}"));
        }
        if let Err(cause) = self
            .hub
            .credentials()
            .delete_token(&identity.hub_url, id)
            .await
        {
            tracing::warn!(identity = %id, "the discarded identity's credential was not deleted: {}", cause.message);
        }
        match self.identities() {
            Some(identities) => {
                if let Err(cause) = identities.forget(&metadata_for(identity)).await {
                    left.push(format!("forgetting {id}: {}", cause.message));
                }
            }
            None => left.push(format!("forgetting {id}: identity service stopped")),
        }
        for dir in [
            self.agent_dir(id),
            self.state_dir.join("agents").join(id.as_str()),
        ] {
            match tokio::fs::remove_dir_all(&dir).await {
                Err(cause) if cause.kind() != std::io::ErrorKind::NotFound => {
                    left.push(format!("removing {}: {cause}", dir.display()));
                }
                _ => {}
            }
        }
        if left.is_empty() {
            return failure;
        }
        ApiError {
            code: failure.code,
            message: format!("{}; left behind: {}", failure.message, left.join("; ")),
        }
    }

    /// Both callers hold `launching`; keep tree and registry insertion atomic.
    pub(super) async fn insert_worker(
        &self,
        worker: super::Worker,
    ) -> Result<Arc<tokio::sync::Mutex<super::Worker>>, ApiError> {
        let mut tree = self.tree.lock().await;
        let mut workers = self.workers.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(error("daemon is shutting down"));
        }
        if workers.len() >= MAX_MANAGED {
            return Err(registry_full());
        }
        let id = worker.record.identity.identity_id.clone();
        tree.insert(id.clone(), worker.record.parent.clone());
        let worker = Arc::new(tokio::sync::Mutex::new(worker));
        workers.insert(id, worker.clone());
        Ok(worker)
    }
}

pub(super) fn registry_full() -> ApiError {
    bad_request(format!(
        "{MAX_MANAGED} managed agents are already registered. agmend stops at {MAX_MANAGED} agents"
    ))
}

/// Whether the identity file in a parent's worktree still names the identity
/// agmend registered for it.
fn file_matches_record(file: &IdentityFile, record: &ManagedIdentity) -> bool {
    file.identity_id == record.identity_id
        && file.tenant == record.tenant
        && file.project == record.project
        && file.owner_id == record.owner_id
        && file.handle == record.handle
        && file.hub_url == record.hub_url
        && file.broker_url == record.broker_url
}
