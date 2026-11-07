//! Parent authority and admission checks for managed launches.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use agmen_client::{IdentityFile, find_identity};
use agmen_local::safeguards::{MAX_MANAGED, Safeguards};
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{IdentityId, Payload};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Launch, Managed, Placement, Record, ReviewedBase, Source, Tier, envelope, error, metadata_for,
    root_depth,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stop {
    pub(super) identity: IdentityId,
    #[serde(default)]
    pub(super) subtree: Option<bool>,
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
        let login = self.origins.person_for(&caller.hub_url).await?;
        let held = worker.lock().await;
        if held.record.source != Source::Launched
            || held.record.identity != *caller
            || caller.owner_id != login.human_id
            || agmen_client::origin_key(&caller.hub_url) != agmen_client::origin_key(&login.hub_url)
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

    /// The root followed by breadth-first levels, with cycles visited once.
    pub(super) async fn tree_levels(&self, root: &IdentityId) -> Vec<Vec<IdentityId>> {
        let tree = self.tree.lock().await;
        let mut seen = BTreeSet::from([root.clone()]);
        let mut levels = vec![vec![root.clone()]];
        for _ in 1..MAX_MANAGED {
            let Some(level) = levels.last() else { break };
            let mut next = Vec::new();
            for (id, parent) in tree.iter().take(MAX_MANAGED) {
                if parent.as_ref().is_some_and(|parent| level.contains(parent))
                    && seen.len() < MAX_MANAGED
                    && seen.insert(id.clone())
                {
                    next.push(id.clone());
                }
            }
            if next.is_empty() {
                break;
            }
            levels.push(next);
        }
        levels
    }

    /// Caller holds launching, so no child can appear during removal.
    pub(super) async fn refuse_live_descendants(&self, root: &IdentityId) -> Result<(), ApiError> {
        for id in self.tree_levels(root).await.into_iter().skip(1).flatten() {
            let child = self.workers.lock().await.get(&id).cloned();
            if let Some(child) = child
                && child.lock().await.state != "offline"
            {
                return Err(bad_request(
                    "agent has live descendants; stop the subtree first",
                ));
            }
        }
        Ok(())
    }

    /// Stops only a strict descendant, using the same tier guard as launch.
    pub(crate) async fn stop_child(
        self: &Arc<Self>,
        caller: &ManagedIdentity,
        raw: Value,
    ) -> Result<Value, ApiError> {
        self.tree_caller(caller).await?;
        let Stop { identity, subtree } = params(raw)?;
        let _launching = self.launching.lock().await;
        self.tree_caller(caller).await?;
        let descendants = self.tree_levels(&caller.identity_id).await;
        if !descendants
            .iter()
            .skip(1)
            .flatten()
            .any(|id| *id == identity)
        {
            return Err(crate::identities::forbidden());
        }
        let stopped = self
            .stop_tree_locked(identity.clone(), subtree.unwrap_or(true))
            .await?;
        Ok(json!({"identity":identity,"state":"offline","stopped":stopped}))
    }

    pub(super) async fn stop_tree(
        self: &Arc<Self>,
        root: IdentityId,
        subtree: bool,
    ) -> Result<Vec<IdentityId>, ApiError> {
        let _launching = self.launching.lock().await;
        self.stop_tree_locked(root, subtree).await
    }

    /// Caller holds launching until all levels finish or the deadline aborts them.
    async fn stop_tree_locked(
        self: &Arc<Self>,
        root: IdentityId,
        subtree: bool,
    ) -> Result<Vec<IdentityId>, ApiError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let mut tasks = tokio::task::JoinSet::new();
        let operation = async {
            let levels = if subtree {
                self.tree_levels(&root).await
            } else {
                vec![vec![root]]
            };
            let workers = self.workers.lock().await.clone();
            // Check every target before stopping any of them, even if disk
            // records were corrupted into a cross-owner tree.
            for id in levels.iter().flatten() {
                let worker = workers
                    .get(id)
                    .ok_or_else(|| bad_request("agent is not managed by this daemon"))?;
                let held = worker.lock().await;
                let login = self
                    .origins
                    .person_for(&held.record.identity.hub_url)
                    .await?;
                if held.record.identity.owner_id != login.human_id
                    || agmen_client::origin_key(&held.record.identity.hub_url)
                        != agmen_client::origin_key(&login.hub_url)
                {
                    return Err(crate::identities::forbidden());
                }
            }
            let mut stopped = Vec::new();
            let mut failure = None;
            for level in levels.into_iter().rev() {
                for id in &level {
                    let worker = workers
                        .get(id)
                        .cloned()
                        .ok_or_else(|| error("stop target disappeared"))?;
                    let manager = self.clone();
                    tasks.spawn(async move { manager.stop_worker(worker).await });
                }
                for _ in 0..level.len() {
                    if let Some(result) = tasks.join_next().await {
                        let result = result.map_err(error).and_then(|result| result);
                        if let Err(cause) = result {
                            failure.get_or_insert(cause);
                        }
                    }
                }
                stopped.extend(level);
            }
            match failure {
                Some(failure) => Err(failure),
                None => Ok(stopped),
            }
        };
        match tokio::time::timeout_at(deadline, operation).await {
            Ok(result) => result,
            Err(_) => {
                tasks.abort_all();
                tasks.shutdown().await;
                Err(error(
                    "subtree stop exceeded its 10 s deadline; remaining stops were aborted",
                ))
            }
        }
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
        login: &agmen_client::Login,
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
            let mut running = 0_u32;
            // The registry bounds this snapshot to MAX_MANAGED workers.
            for worker in workers {
                let held = worker.lock().await;
                if held.state != "offline"
                    && held.record.identity.owner_id == login.human_id
                    && agmen_client::origin_key(&held.record.identity.hub_url)
                        == agmen_client::origin_key(&login.hub_url)
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
        mut launch: Launch,
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
        let safeguards = self.read_safeguards(true).await?;
        let origin = self.origins.of(&launch.project).await?;
        let login = self.origins.person(origin).await?;
        let depth = self.admit(parent.as_ref(), &login, &safeguards).await?;
        let brief = brief
            .as_deref()
            .map(|body| envelope(Payload::Dm {}, body))
            .transpose()?;
        if brief.is_some()
            && let Some(parent) = &parent
        {
            check_brief_verb(parent)?;
        }
        let (joined, repo) =
            checked_checkout(root, &launch.project, &login, parent.as_ref()).await?;
        let base = self.launch_base(&repo, parent.as_ref()).await?;
        let role = self
            .launch_role(&repo, &launch, parent.as_ref(), base.as_ref())
            .await?;
        // A child's project role is read at its tree's commit. A root's is
        // read from the checkout, where the person launched it.
        let role_commit = parent.as_ref().and_then(|_| {
            role.as_ref()
                .filter(|role| role.layer == agmen_roles::Layer::Project)
                .and(base.as_ref())
                .map(|base| base.commit.clone())
        });
        let _accounting = self.accounting.lock().await;
        let branch = format!(
            "agent/{}-{}",
            launch.name,
            &crate::launch::new_session_id().map_err(error)?[..8]
        );
        let reservation = self
            .reserve_child_budget(&mut launch, parent.as_ref(), &safeguards)
            .await?;
        let result = async {
            // shortcut: the .git pointer can change between the repository
            // check and worktree add. design.md 6.3 item 6 accepts that a
            // same-user agent can already act as the person. Revisit with
            // separate OS identities.
            // The branch starts at this commit, which `remove_unused` checks.
            let start = crate::worktree::commit(&joined.root, "HEAD")
                .await
                .map_err(error)?;
            let worktree = crate::worktree::create(&joined.root, &start, &branch)
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
                        shared_budget: reservation.is_some(),
                        role_commit,
                        reviewed_base: base,
                    },
                    agmen,
                    role,
                )
                .await;
            match result {
                Ok(result) => Ok(result),
                Err(failure) => match discard_worktree(&repo, &worktree, &branch, &start).await {
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
        .await;
        if result.is_err()
            && let Some(reservation) = reservation
        {
            reservation.rollback().await;
        }
        result
    }

    async fn launch_role(
        &self,
        repo: &std::path::Path,
        launch: &Launch,
        parent: Option<&Record>,
        base: Option<&ReviewedBase>,
    ) -> Result<Option<agmen_roles::Resolved>, ApiError> {
        Ok(match parent {
            Some(parent) => {
                let name = launch
                    .role
                    .as_deref()
                    .ok_or_else(|| bad_request("a child's role is required"))?;
                let parent_role = match &parent.role {
                    Some(name) => {
                        let role = self.record_role(parent, name.as_str()).await?;
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
                let base = base.ok_or_else(|| error("child role commit is missing"))?;
                let role = self
                    .load_role_at(repo, name, &base.commit)
                    .await
                    .map_err(|failure| ApiError {
                        code: failure.code,
                        message: format!(
                            "role {name} at {} ({}), the commit recorded when its root launched: {}; relaunch the root to read a newer one",
                            base.reference, base.commit, failure.message
                        ),
                    })?;
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
        if let Err(cause) = self.revoke_identity(&metadata_for(identity)).await {
            left.push(format!("revoking {id}: {}", cause.message));
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
            self.state_dir.join("identities").join(id.as_str()),
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

async fn checked_checkout(
    root: PathBuf,
    project: &agmen_proto::ProjectId,
    login: &agmen_client::Login,
    parent: Option<&Record>,
) -> Result<(agmen_client::Joined, PathBuf), ApiError> {
    let joined = tokio::task::spawn_blocking(move || find_identity(&root))
        .await
        .map_err(error)?
        .map_err(error)?
        .ok_or_else(|| bad_request("checkout has no trusted agmen identity"))?;
    super::check_project(&joined.identity, project, login)?;
    // The parent can write its worktree. Check its identity and repository
    // against agmend's record before taking any placement data from it.
    if let Some(parent) = parent
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
    if let Some(parent) = parent
        && parent.repo != repo
    {
        return Err(bad_request(
            "the parent's worktree no longer belongs to the checkout it was launched from",
        ));
    }
    Ok((joined, repo))
}

/// Undoes a failed launch's worktree. A branch that nothing used goes whether
/// or not the commit it starts at is merged, since a child starts at its
/// parent's commit. One that something used is kept, with its work, as `remove`
/// keeps any worker's.
async fn discard_worktree(
    repo: &std::path::Path,
    worktree: &std::path::Path,
    branch: &str,
    start: &str,
) -> anyhow::Result<()> {
    match crate::worktree::remove_unused(repo, worktree, branch, start).await {
        Ok(()) => Ok(()),
        // Its refusal means something used the tree or git failed: either way
        // the safe removal decides, and its answer is the one reported.
        Err(_) => crate::worktree::remove(repo, worktree, branch).await,
    }
}

/// A brief is a DM from the parent, so the parent's role must grant the verb.
/// The grant is the one the hub returned at the parent's launch and agmend
/// recorded, never a file in the parent's worktree. A parent launched before
/// grants were recorded has none on record and cannot brief until relaunched.
fn check_brief_verb(parent: &Record) -> Result<(), ApiError> {
    match &parent.verbs {
        Some(verbs) if verbs.contains("DM") => Ok(()),
        Some(_) => Err(bad_request(format!(
            "a brief is a DM, and role {} does not grant the DM verb",
            parent.role.as_ref().map_or("(none)", |name| name.as_str())
        ))),
        None => Err(bad_request(format!(
            "a brief is a DM, and agmend has no record of {}'s verbs (it was launched before verbs were recorded); relaunch it to send a brief",
            parent.identity.handle
        ))),
    }
}

pub(super) async fn deliver_brief(
    identities: &crate::identities::Identities,
    sender: &ManagedIdentity,
    child: &IdentityId,
    mut message: agmen_proto::ClientEnvelope,
) -> Result<(), ApiError> {
    message.to = vec![child.clone()];
    let delivered = identities
        .post(sender, message)
        .await
        .map_err(|failure| ApiError {
            code: failure.code,
            message: format!("brief was not delivered: {}", failure.message),
        })?;
    if delivered.get("queued") == Some(&Value::Bool(true)) {
        return Err(error(
            "brief was not delivered: hub acknowledgement timed out",
        ));
    }
    Ok(())
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
