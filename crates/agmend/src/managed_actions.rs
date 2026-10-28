//! Start, Attach, Detach and Remove for the signed-in person's agents.
//!
//! Start re-adopts an agent agmend made (its `agent.json` record) or a
//! folder the person joined by hand, without making a worktree or branch.
//! Attach opens an interactive Claude Code on the agent's owner session and
//! pauses its headless runs until that process exits or Detach. Remove
//! revokes the identity and forgets the agent but keeps its folder and
//! branch.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use agmen_client::{Joined, Login, find_identity};
use agmen_local::console::Status;
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{ClientEnvelope, IdentityId, Payload, ProjectId};
use rustix::process::{Pid, Signal};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::{
    Attached, MAX_AGENTS, Managed, Record, Source, Worker, envelope, error, metadata_for,
    private_json,
};
use crate::budget::{Budget, Money};
use crate::console::{bad_request, params};
use crate::launch::AttachSpec;
use crate::state::ManagedIdentity;
use crate::transcripts;

/// How long Terminal has to start Claude Code before Attach counts it gone.
const ATTACH_GRACE: Duration = Duration::from_secs(120);
/// How long Detach waits for Claude Code to exit after SIGTERM, then after
/// SIGKILL, and how often it looks.
const TERM_WAIT: Duration = Duration::from_secs(2);
const KILL_WAIT: Duration = Duration::from_secs(1);
const EXIT_POLL: Duration = Duration::from_millis(50);
/// How much later than the pid file a process may seem to start and still
/// be the one that wrote it: `ps` counts whole seconds.
const START_SLACK: Duration = Duration::from_secs(2);

/// Whether an attached Claude Code is known to have exited, so its
/// transcript holds all it will ever write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    Confirmed,
    Unconfirmed,
}

/// What `attach.pid` says about the attached Claude Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder {
    /// No file yet, or no process id in it.
    Missing,
    /// The process it names has exited, or the id now belongs to another
    /// process.
    Gone,
    Running(Pid),
}
/// The largest `agent.json` or `status.json` agmend reads.
const MAX_RECORD_BYTES: u64 = 64 * 1024;
/// The most worktrees one folder lookup checks.
const MAX_WORKTREES: usize = 256;
/// What a hand-joined agent gets, as New Agent's defaults.
const JOINED_MODEL: &str = "sonnet";
const JOINED_BUDGET_USD: &str = "5";

/// The agent an action names. `project` and `root` matter only for a folder
/// joined by hand: the project it must belong to, and the folder the person
/// picked when agmend could not find it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    identity: IdentityId,
    #[serde(default)]
    project: Option<ProjectId>,
    #[serde(default)]
    root: Option<PathBuf>,
}

/// `agent.json`, in this format or the older one with a single `session_id`.
#[derive(Debug, Deserialize)]
struct Stored {
    identity: ManagedIdentity,
    repo: PathBuf,
    worktree: PathBuf,
    branch: String,
    #[serde(default)]
    owner_session_id: Option<String>,
    #[serde(default)]
    others_session_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    model: Option<String>,
    budget_usd: String,
    #[serde(default)]
    source: Source,
}

impl Stored {
    /// The record, and whether its format changed so it must be saved again.
    fn into_record(self) -> anyhow::Result<(Record, bool)> {
        // The older format kept one session, which ran every batch, so it
        // becomes the owner session only once Start or Attach checks it.
        let owner = self
            .owner_session_id
            .or(self.session_id)
            .ok_or_else(|| anyhow::anyhow!("agent record has no session id"))?;
        let (others, migrated) = match self.others_session_id {
            Some(others) => (others, false),
            None => (crate::launch::new_session_id()?, true),
        };
        anyhow::ensure!(
            transcripts::is_session_id(&owner) && transcripts::is_session_id(&others),
            "agent record has an invalid session id"
        );
        anyhow::ensure!(owner != others, "agent record shares one session id");
        anyhow::ensure!(
            Money::from_usd(&self.budget_usd).is_ok(),
            "agent record has an invalid budget"
        );
        Ok((
            Record {
                identity: self.identity,
                repo: self.repo,
                worktree: self.worktree,
                branch: self.branch,
                owner_session_id: owner,
                others_session_id: others,
                model: self.model,
                budget_usd: self.budget_usd,
                source: self.source,
            },
            migrated,
        ))
    }
}

#[derive(Debug, Deserialize)]
struct SavedStatus {
    spent_usd: String,
}

/// One record read from disk: the record, its saved spend, and whether it
/// must be saved again in the current format.
type Loaded = (Record, Option<String>, bool);

impl Managed {
    pub(super) fn agent_dir(&self, id: &IdentityId) -> PathBuf {
        self.state_dir.join("managed").join(id.as_str())
    }

    /// Reads every saved agent record as an offline agent. Nothing starts:
    /// each waits for the person's Start.
    pub(crate) async fn load(&self) {
        let dir = self.state_dir.join("managed");
        let found = match tokio::task::spawn_blocking(move || read_records(&dir)).await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(%error, "reading agent records stopped");
                return;
            }
        };
        for (record, spent, migrated) in found {
            let id = record.identity.identity_id.clone();
            if migrated && let Err(error) = self.save_record(&record).await {
                tracing::warn!(code = ?error.code, "saving a migrated agent record failed");
                continue;
            }
            let Ok(cap) = Money::from_usd(&record.budget_usd) else {
                continue;
            };
            let mut budget = Budget::new(cap);
            if let Some(spent) = spent
                && budget.observe_total_cost_usd(&spent, |_| {}).is_err()
            {
                tracing::warn!("an agent's saved spend is invalid; counting from zero");
            }
            // Start and Attach check the sessions before either tier resumes.
            let worker = Worker::offline(record, budget);
            let mut workers = self.workers.lock().await;
            if workers.len() < MAX_AGENTS && !workers.contains_key(&id) {
                workers.insert(id, Arc::new(Mutex::new(worker)));
            }
        }
    }

    /// `agent_start {identity, project?, root?}`: brings an offline agent
    /// back with its previous context.
    pub(crate) async fn start(self: &Arc<Self>, raw: Value) -> Result<Value, ApiError> {
        let target: Target = params(raw)?;
        let _launching = self.launching.lock().await;
        self.refuse_if_still_attached(&target.identity).await?;
        let (worker, context) = self.adopt(target).await?;
        let mut started = self.bring_online(&worker, "idle").await?;
        started["context"] = json!(context);
        Ok(started)
    }

    /// `agent_attach {identity, project?, root?}`: opens an interactive
    /// Claude Code on the owner session and pauses headless runs.
    pub(crate) async fn attach_claude(self: &Arc<Self>, raw: Value) -> Result<Value, ApiError> {
        let target: Target = params(raw)?;
        let _launching = self.launching.lock().await;
        let login = self.hub.credentials().login().await.map_err(error)?;
        let existing = self.workers.lock().await.get(&target.identity).cloned();
        let online = match &existing {
            Some(worker) => {
                let held = worker.lock().await;
                owned(&held.record.identity, &login)?;
                match held.state {
                    "attached" => {
                        return Err(bad_request(format!(
                            "{} is already attached",
                            held.record.identity.handle
                        )));
                    }
                    "offline" => None,
                    _ => Some(worker.clone()),
                }
            }
            None => None,
        };
        self.refuse_if_still_attached(&target.identity).await?;
        let identity = target.identity.clone();
        let adopted = online.is_none();
        let (worker, mut context) = match online {
            Some(worker) => (worker, "resumed"),
            None => {
                let (worker, context) = self.adopt(target).await?;
                self.bring_online(&worker, "attached").await?;
                (worker, context)
            }
        };
        let pid_file = self.agent_dir(&identity).join("attach.pid");
        remove_quietly(&pid_file).await;
        // Mark it attached before cancelling, so no new batch starts and the
        // cancelled one leaves the state alone.
        let task = {
            let mut held = worker.lock().await;
            held.state = "attached";
            held.attached = Some(Attached {
                pid_file: pid_file.clone(),
                since: tokio::time::Instant::now(),
                seen: false,
                pid: None,
            });
            if let Some(stop) = held.stop.take() {
                let _sent = stop.send(());
            }
            held.task.take()
        };
        if let Some(mut task) = task
            && tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _joined = task.await;
        }
        let spec = {
            let mut held = worker.lock().await;
            self.refresh_sessions(&mut held).await.map(|fresh| {
                if let Some(found) = fresh {
                    context = fresh_context(found);
                }
                AttachSpec {
                    cwd: held.record.worktree.clone(),
                    // Only ever the owner session: others' messages never
                    // reach an interactive Claude Code.
                    session_id: held.record.owner_session_id.clone(),
                    resume: held.owner_resumed,
                    script: self.agent_dir(&identity).join("attach.command"),
                    pid_file,
                }
            })
        };
        let opened = match spec {
            Ok(spec) => self.runner.attach(spec).await.map_err(error),
            Err(failure) => Err(failure),
        };
        let Err(failure) = opened else {
            return Ok(json!({"identity": identity, "state": "attached", "context": context}));
        };
        // Leave the agent as it was: offline if Attach brought it online.
        {
            let mut held = worker.lock().await;
            held.attached = None;
            if held.state == "attached" {
                held.state = "idle";
            }
        }
        if adopted && let Err(stop) = self.stop_worker(worker).await {
            tracing::warn!(code = ?stop.code, "stopping an agent after a failed attach failed");
        }
        Err(failure)
    }

    /// `agent_detach {identity}`: ends the interactive Claude Code and
    /// resumes headless runs on the same session.
    pub(crate) async fn detach(&self, raw: Value) -> Result<Value, ApiError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Detach {
            identity: IdentityId,
        }
        let Detach { identity } = params(raw)?;
        let worker = self
            .workers
            .lock()
            .await
            .get(&identity)
            .cloned()
            .ok_or_else(|| bad_request("agent is not managed by this daemon"))?;
        let login = self.hub.credentials().login().await.map_err(error)?;
        let attached = {
            let held = worker.lock().await;
            owned(&held.record.identity, &login)?;
            if held.state != "attached" {
                return Err(bad_request(format!(
                    "{} is not attached",
                    held.record.identity.handle
                )));
            }
            held.attached
                .as_ref()
                .map(|attached| (attached.pid_file.clone(), attached.pid))
        };
        // Wait unlocked, so the other agents' batches keep running.
        let exit = match attached {
            Some((pid_file, pid)) => end_interactive(&pid_file, pid).await,
            None => Exit::Confirmed,
        };
        let mut held = worker.lock().await;
        // The supervisor may have seen the exit first and finished it.
        if held.state == "attached" {
            self.finish_attach(&mut held, exit).await;
        }
        Ok(json!({"identity": identity, "state": held.state}))
    }

    /// `agent_remove {identity, project?, root?}`: stops the agent, revokes
    /// its identity and forgets it, keeping its folder and branch.
    pub(crate) async fn remove(self: &Arc<Self>, raw: Value) -> Result<Value, ApiError> {
        let target: Target = params(raw)?;
        let _launching = self.launching.lock().await;
        let login = self.hub.credentials().login().await.map_err(error)?;
        let existing = self.workers.lock().await.get(&target.identity).cloned();
        let (identity, folder, branch) = match &existing {
            Some(worker) => {
                let held = worker.lock().await;
                owned(&held.record.identity, &login)?;
                (
                    held.record.identity.clone(),
                    held.record.worktree.clone(),
                    held.record.branch.clone(),
                )
            }
            None => {
                let project = target.project.ok_or_else(|| {
                    bad_request("project is required for an agent agmend has not started")
                })?;
                let folder = match target.root {
                    Some(root) => root,
                    None => self
                        .find_folder(&project, &target.identity)
                        .await
                        .ok_or_else(|| no_folder(&target.identity))?,
                };
                let joined = verify_folder(&folder, &target.identity, &project, &login).await?;
                let branch = current_branch(&joined.root).await;
                (metadata_for(&joined.identity), joined.root, branch)
            }
        };
        // A record's identity file may already be gone; one that is there
        // must be this identity's before it is deleted.
        let present = tokio::fs::try_exists(identity_file(&folder))
            .await
            .unwrap_or(true);
        let joined =
            match verify_folder(&folder, &identity.identity_id, &identity.project, &login).await {
                Ok(joined) => Some(joined),
                Err(_) if existing.is_some() && !present => None,
                Err(refusal) => return Err(refusal),
            };
        // An attached Claude Code, or one left open across a restart, is
        // ended: its identity is about to be revoked.
        let known = match &existing {
            Some(worker) => worker
                .lock()
                .await
                .attached
                .as_ref()
                .and_then(|attached| attached.pid),
            None => None,
        };
        end_interactive(
            &self.agent_dir(&identity.identity_id).join("attach.pid"),
            known,
        )
        .await;
        if let Some(worker) = existing {
            self.stop_worker(worker).await?;
        }
        if !self
            .hub
            .revoke_identity(&identity.identity_id)
            .await
            .map_err(ApiError::from)?
        {
            tracing::info!("the hub had no such identity; removing it here");
        }
        if let Some(joined) = joined {
            delete_identity_file(&joined.root).await?;
        }
        // The hub has revoked it, so a credential left behind no longer
        // works. The Keychain refuses agmend a delete of one that another
        // program saved, such as the CLI's `agmen join`.
        if let Err(failure) = self
            .hub
            .credentials()
            .delete_token(&identity.hub_url, &identity.identity_id)
            .await
        {
            tracing::warn!(
                identity = %identity.identity_id,
                "the revoked credential was not deleted: {}",
                failure.message
            );
        }
        self.identities()
            .ok_or_else(|| error("identity service stopped"))?
            .forget(&identity)
            .await?;
        let dir = self.agent_dir(&identity.identity_id);
        match tokio::fs::remove_dir_all(&dir).await {
            Err(failure) if failure.kind() != std::io::ErrorKind::NotFound => {
                return Err(error(format!("removing {}: {failure}", dir.display())));
            }
            _ => {}
        }
        self.workers.lock().await.remove(&identity.identity_id);
        Ok(json!({
            "identity": identity.identity_id,
            "removed": true,
            "kept": {"folder": folder, "branch": branch},
        }))
    }

    /// The offline agent `target` names, read from memory or adopted from a
    /// hand-joined folder, after its identity file and presence check out,
    /// and what became of its previous context: `resumed`, `new`, or, for a
    /// hand-joined folder whose last transcript is not resumed,
    /// `skipped_others` or `skipped_unreadable`. The caller holds `launching`.
    async fn adopt(&self, target: Target) -> Result<(Arc<Mutex<Worker>>, &'static str), ApiError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(error("daemon is shutting down"));
        }
        let login = self.hub.credentials().login().await.map_err(error)?;
        let existing = self.workers.lock().await.get(&target.identity).cloned();
        if let Some(worker) = existing {
            let identity = {
                let held = worker.lock().await;
                owned(&held.record.identity, &login)?;
                if held.state != "offline" {
                    return Err(bad_request(format!(
                        "{} is already running",
                        held.record.identity.handle
                    )));
                }
                held.record.identity.clone()
            };
            if target
                .project
                .as_ref()
                .is_some_and(|project| *project != identity.project)
            {
                return Err(bad_request("agent belongs to another project"));
            }
            let folder = worker.lock().await.record.worktree.clone();
            verify_folder(&folder, &identity.identity_id, &identity.project, &login).await?;
            self.refuse_if_connected(&identity).await?;
            let mut held = worker.lock().await;
            let context = match self.refresh_sessions(&mut held).await? {
                Some(found) => fresh_context(found),
                None if held.owner_resumed || held.others_resumed => "resumed",
                None => "new",
            };
            drop(held);
            return Ok((worker, context));
        }
        if self.workers.lock().await.len() >= MAX_AGENTS {
            return Err(bad_request("64 managed agents are already registered"));
        }
        let project = target.project.ok_or_else(|| {
            bad_request("project is required for an agent agmend has not started")
        })?;
        let folder = match target.root {
            Some(root) => root,
            None => self
                .find_folder(&project, &target.identity)
                .await
                .ok_or_else(|| no_folder(&target.identity))?,
        };
        let joined = verify_folder(&folder, &target.identity, &project, &login).await?;
        let identity = metadata_for(&joined.identity);
        self.refuse_if_connected(&identity).await?;
        let agmen = self.runner.mcp_command().await.map_err(error)?;
        let (worker_root, _) = crate::worktree::worktree_root(&joined.root)
            .await
            .map_err(error)?;
        let repo = worker_root
            .parent()
            .ok_or_else(|| error("worker root has no main checkout"))?
            .to_owned();
        let (newest, context) = self.owner_context(&joined.root, &login).await;
        let record = Record {
            identity,
            repo,
            worktree: joined.root.clone(),
            branch: current_branch(&joined.root).await,
            owner_session_id: match &newest {
                Some(id) => id.clone(),
                None => crate::launch::new_session_id().map_err(error)?,
            },
            others_session_id: crate::launch::new_session_id().map_err(error)?,
            model: Some(JOINED_MODEL.into()),
            budget_usd: JOINED_BUDGET_USD.into(),
            source: Source::Joined,
        };
        self.save_record(&record).await?;
        let mcp = self.agent_dir(&target.identity).join("mcp.json");
        let config = json!({"mcpServers":{"agmen":{"command":agmen,"args":["mcp"]}}});
        tokio::task::spawn_blocking(move || replace_private_json(&mcp, &config))
            .await
            .map_err(error)?
            .map_err(error)?;
        let cap = Money::from_usd(JOINED_BUDGET_USD).map_err(|_| error("invalid budget"))?;
        let mut worker = Worker::offline(record, Budget::new(cap));
        // The newest transcript is the session this folder last used.
        worker.owner_resumed = newest.is_some();
        let worker = Arc::new(Mutex::new(worker));
        self.workers
            .lock()
            .await
            .insert(target.identity, worker.clone());
        Ok((worker, context))
    }

    /// The hand-joined folder's newest session, when the owner tier may
    /// resume it, and what became of it. An interactive session with the
    /// agmen plugin sees every sender's messages, and the owner tier runs
    /// with bypassPermissions, so only a transcript holding nothing from
    /// anyone but the owner is resumed.
    async fn owner_context(&self, folder: &Path, login: &Login) -> (Option<String>, &'static str) {
        let home = self.claude_home.as_deref();
        let Some(newest) = transcripts::newest(home, folder).await else {
            return (None, "new");
        };
        let owner = login.human_id.as_str();
        match transcripts::contents(home, folder, &newest, owner, &login.handle).await {
            transcripts::Contents::OwnerOnly => (Some(newest), "resumed"),
            found => (None, fresh_context(found)),
        }
    }

    /// Registers the agent's identity, sets `state`, and says HELLO.
    async fn bring_online(
        &self,
        worker: &Arc<Mutex<Worker>>,
        state: &'static str,
    ) -> Result<Value, ApiError> {
        let identities = self
            .identities()
            .ok_or_else(|| error("identity service stopped"))?;
        let mut held = worker.lock().await;
        identities.register(held.record.identity.clone()).await?;
        held.state = state;
        held.last_run = None;
        let record = held.record.clone();
        drop(held);
        let hello = hello(record.model.clone(), record.branch.clone())?;
        if let Err(error) = identities.post(&record.identity, hello).await {
            tracing::warn!(code = ?error.code, "managed agent HELLO is pending connection");
        }
        Ok(json!({
            "identity": record.identity.identity_id,
            "handle": record.identity.handle,
            "worktree": record.worktree,
            "branch": record.branch,
            "state": state,
        }))
    }

    /// Refuses when the hub shows the identity connected: an interactive
    /// Claude Code with the agmen plugin would answer the same messages.
    async fn refuse_if_connected(&self, identity: &ManagedIdentity) -> Result<(), ApiError> {
        let roster = self
            .hub
            .roster(&identity.project)
            .await
            .map_err(|failure| {
                let failure = ApiError::from(failure);
                ApiError {
                    code: failure.code,
                    message: format!(
                        "could not check whether {} is connected elsewhere: {}",
                        identity.handle, failure.message
                    ),
                }
            })?;
        if roster.iter().any(|entry| {
            entry.identity_id == identity.identity_id
                && matches!(entry.status, Status::Online | Status::Away)
        }) {
            return Err(bad_request(format!(
                "{} is connected from another session, such as an interactive Claude Code; close it, then try again",
                identity.handle
            )));
        }
        Ok(())
    }

    /// Marks each tier resumable once Claude Code has saved its session. The
    /// owner session runs with bypassPermissions, so a saved one is resumed
    /// only when it holds nothing from other senders; otherwise the owner
    /// tier moves to a fresh session, and this says why.
    async fn refresh_sessions(
        &self,
        worker: &mut Worker,
    ) -> Result<Option<transcripts::Contents>, ApiError> {
        let home = self.claude_home.as_deref();
        let folder = worker.record.worktree.clone();
        worker.others_resumed = worker.others_resumed
            || transcripts::exists(home, &folder, &worker.record.others_session_id).await;
        if !transcripts::exists(home, &folder, &worker.record.owner_session_id).await {
            return Ok(None);
        }
        let identity = &worker.record.identity;
        let owner_handle = identity.handle.split('/').next().unwrap_or_default();
        match transcripts::contents(
            home,
            &folder,
            &worker.record.owner_session_id,
            identity.owner_id.as_str(),
            owner_handle,
        )
        .await
        {
            transcripts::Contents::OwnerOnly => {
                worker.owner_resumed = true;
                Ok(None)
            }
            found => {
                self.fresh_owner_session(worker).await?;
                Ok(Some(found))
            }
        }
    }

    /// Moves the owner tier to a new, unsaved session.
    async fn fresh_owner_session(&self, worker: &mut Worker) -> Result<(), ApiError> {
        worker.record.owner_session_id = crate::launch::new_session_id().map_err(error)?;
        worker.owner_resumed = false;
        // The new id must survive a restart, or the old one comes back.
        self.save_record(&worker.record).await
    }

    /// One supervisor look at an attached agent: back to headless runs once
    /// its Claude Code has exited.
    pub(super) async fn watch_attached(&self, worker: &mut Worker) {
        if self.attach_ended(worker).await {
            self.finish_attach(worker, Exit::Confirmed).await;
        }
    }

    /// Whether the attached Claude Code has exited, or never started.
    async fn attach_ended(&self, worker: &mut Worker) -> bool {
        let Some(attached) = worker.attached.as_mut() else {
            return true;
        };
        let late = attached.since.elapsed() > ATTACH_GRACE;
        // A pid already checked against its file's age is only probed: no
        // `ps` on every tick.
        let named = read_pid(&attached.pid_file).await;
        if let (Some(pid), Some(known)) = (named, attached.pid)
            && pid == known
        {
            return !running(pid);
        }
        match holder(&attached.pid_file).await {
            Holder::Running(pid) => {
                attached.seen = true;
                attached.pid = Some(pid);
                false
            }
            Holder::Gone => {
                attached.seen = true;
                true
            }
            Holder::Missing => attached.seen || late,
        }
    }

    /// Refuses while `attach.pid` names a running attached Claude Code, as
    /// after Stop or a restart during an attach: headless batches would
    /// share its session.
    async fn refuse_if_still_attached(&self, id: &IdentityId) -> Result<(), ApiError> {
        match holder(&self.agent_dir(id).join("attach.pid")).await {
            Holder::Running(_) => Err(bad_request(
                "an attached Claude Code is still open for this agent; quit it, then try again",
            )),
            Holder::Missing | Holder::Gone => Ok(()),
        }
    }

    /// The session the next batch of a tier runs on, and whether it resumes
    /// it. The tiers never share one, so untrusted conversation history stays
    /// out of bypassPermissions batches. An owner session is checked before
    /// every resume: it may have gained other senders' messages since, from a
    /// Claude Code run by hand in the folder or one left open across a
    /// restart.
    pub(super) async fn batch_session(
        &self,
        worker: &mut Worker,
        owner_only: bool,
    ) -> Result<(String, bool), ApiError> {
        if !owner_only {
            return Ok((
                worker.record.others_session_id.clone(),
                worker.others_resumed,
            ));
        }
        if worker.owner_resumed {
            self.refresh_sessions(worker).await?;
        }
        Ok((worker.record.owner_session_id.clone(), worker.owner_resumed))
    }

    /// Back to headless runs on the same owner session, or a fresh one when
    /// the attached Claude Code may still be writing to it.
    async fn finish_attach(&self, worker: &mut Worker, exit: Exit) {
        let seen = match worker.attached.take() {
            Some(attached) => {
                remove_quietly(&attached.pid_file).await;
                attached.seen
            }
            None => false,
        };
        // Resume what the interactive session saved, once it checks out.
        // Without Claude Code's folder to look in, a session that ran cannot
        // be checked, so the owner tier starts fresh.
        let checked = if exit == Exit::Unconfirmed {
            self.fresh_owner_session(worker).await
        } else if self.claude_home.is_some() {
            self.refresh_sessions(worker).await.map(drop)
        } else if seen {
            self.fresh_owner_session(worker).await
        } else {
            Ok(())
        };
        if let Err(failure) = checked {
            // No safe session to resume: leave the agent offline until Start.
            tracing::error!(code = ?failure.code, "checking the attached session failed");
            worker.state = "offline";
            worker.last_run = None;
            return;
        }
        if worker.state == "attached" {
            worker.state = "idle";
        }
        worker.last_run = None;
    }

    /// The folder in `project`'s open checkout whose identity file names `id`.
    async fn find_folder(&self, project: &ProjectId, id: &IdentityId) -> Option<PathBuf> {
        let root = self.roots.lock().await.get(project).cloned()?;
        let (_, trees) = crate::worktree::worktree_root(&root).await.ok()?;
        for tree in trees
            .into_iter()
            .filter(|tree| !tree.skip)
            .take(MAX_WORKTREES)
        {
            let path = tree.path;
            let found = tokio::task::spawn_blocking(move || find_identity(&path)).await;
            if let Ok(Ok(Some(joined))) = found
                && joined.identity.identity_id == *id
            {
                return Some(joined.root);
            }
        }
        None
    }

    async fn save_record(&self, record: &Record) -> Result<(), ApiError> {
        let dir = self.agent_dir(&record.identity.identity_id);
        let value = serde_json::to_value(record).map_err(error)?;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)?;
            replace_private_json(&dir.join("agent.json"), &value)
        })
        .await
        .map_err(error)?
        .map_err(error)
    }
}

/// The HELLO a managed agent says when it comes online.
pub(super) fn hello(model: Option<String>, branch: String) -> Result<ClientEnvelope, ApiError> {
    envelope(
        Payload::Hello {
            client: format!("agmend/{}", env!("CARGO_PKG_VERSION")),
            agent: Some("claude-code".into()),
            model,
            looking_at: Some(branch),
        },
        "Ready for messages.",
    )
}

/// Saves the agent's lifetime spend so a restart keeps counting from it.
pub(super) async fn save_spent(dir: &Path, spent: Money) {
    let path = dir.join("status.json");
    let value = json!({"spent_usd": spent.usd()});
    match tokio::task::spawn_blocking(move || replace_private_json(&path, &value)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "saving an agent's spend failed"),
        Err(error) => tracing::warn!(%error, "saving an agent's spend stopped"),
    }
}

/// Start's or Attach's `context` when the owner tier began a fresh session
/// because its last one held `found`.
const fn fresh_context(found: transcripts::Contents) -> &'static str {
    match found {
        transcripts::Contents::Others => "skipped_others",
        transcripts::Contents::Unreadable => "skipped_unreadable",
        transcripts::Contents::OwnerOnly => "resumed",
    }
}

fn owned(identity: &ManagedIdentity, login: &Login) -> Result<(), ApiError> {
    if identity.owner_id == login.human_id && identity.hub_url.origin() == login.hub_url.origin() {
        Ok(())
    } else {
        Err(crate::identities::forbidden())
    }
}

fn no_folder(id: &IdentityId) -> ApiError {
    ApiError {
        code: ErrorCode::NotFound,
        message: format!("agmend cannot find the folder joined as {id}; pick the folder"),
    }
}

/// The trusted identity at `folder`, when it is `id` in `project` for the
/// signed-in person at their hub.
async fn verify_folder(
    folder: &Path,
    id: &IdentityId,
    project: &ProjectId,
    login: &Login,
) -> Result<Joined, ApiError> {
    let at = folder.to_owned();
    let joined = tokio::task::spawn_blocking(move || find_identity(&at))
        .await
        .map_err(error)?
        .map_err(error)?
        .ok_or_else(|| {
            bad_request(format!(
                "{} has no trusted agmen identity",
                folder.display()
            ))
        })?;
    if joined.identity.identity_id != *id {
        return Err(bad_request(format!(
            "{} is joined as {}, not {id}",
            joined.root.display(),
            joined.identity.identity_id
        )));
    }
    if joined.identity.project != *project
        || joined.identity.owner_id != login.human_id
        || joined.identity.hub_url.origin() != login.hub_url.origin()
    {
        return Err(bad_request(
            "checkout is not joined to this project and owner",
        ));
    }
    Ok(joined)
}

fn identity_file(root: &Path) -> PathBuf {
    root.join(".agmen").join("identity.json")
}

/// Deletes the identity file as `agmen leave` does, and its folder when empty.
async fn delete_identity_file(root: &Path) -> Result<(), ApiError> {
    let path = identity_file(root);
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::remove_file(&path)?;
        if let Some(dir) = path.parent()
            && std::fs::remove_dir(dir).is_err()
        {
            tracing::debug!("{} is not empty; leaving it", dir.display());
        }
        Ok(())
    })
    .await
    .map_err(error)?
    .map_err(|failure| error(format!("removing the identity file: {failure}")))
}

/// The branch checked out at `folder`, or empty when HEAD is detached or
/// git cannot say.
async fn current_branch(folder: &Path) -> String {
    let git = crate::git::Git::new(folder, tokio::time::Instant::now() + Duration::from_secs(5));
    match git
        .run_str(&["symbolic-ref", "--quiet", "--short", "HEAD"], 4096)
        .await
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        Err(_) => String::new(),
    }
}

fn parse_pid(text: &str) -> Option<Pid> {
    text.trim().parse::<i32>().ok().and_then(Pid::from_raw)
}

/// Whether `pid` names a process of this user. Another user's process holds
/// a reused id: the attached Claude Code runs as this user.
fn running(pid: Pid) -> bool {
    rustix::process::test_kill_process(pid).is_ok()
}

/// What `pid_file` says. A process that started after the file was written
/// holds a reused id; when `ps` cannot say, the process counts as the one
/// the script started.
async fn holder(pid_file: &Path) -> Holder {
    let path = pid_file.to_owned();
    let read = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        let written = std::fs::metadata(&path)?.modified()?;
        Ok((read_bounded(&path)?, written))
    })
    .await;
    let Ok(Ok((bytes, written))) = read else {
        return Holder::Missing;
    };
    // Half written, or not a pid at all.
    let Some(pid) = std::str::from_utf8(&bytes).ok().and_then(parse_pid) else {
        return Holder::Missing;
    };
    if !running(pid) {
        return Holder::Gone;
    }
    match started(pid).await {
        Some(start) if start > written + START_SLACK => Holder::Gone,
        _ => Holder::Running(pid),
    }
}

/// The process id `pid_file` names, if any.
async fn read_pid(pid_file: &Path) -> Option<Pid> {
    let path = pid_file.to_owned();
    let bytes = tokio::task::spawn_blocking(move || read_bounded(&path))
        .await
        .ok()?
        .ok()?;
    std::str::from_utf8(&bytes).ok().and_then(parse_pid)
}

/// When `pid` started, from `ps -o etime=`.
async fn started(pid: Pid) -> Option<std::time::SystemTime> {
    let output = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("/bin/ps")
            .args(["-o", "etime=", "-p", &pid.as_raw_nonzero().to_string()])
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let elapsed = parse_etime(std::str::from_utf8(&output.stdout).ok()?.trim())?;
    std::time::SystemTime::now().checked_sub(elapsed)
}

/// `ps`'s elapsed time, `[[dd-]hh:]mm:ss`.
pub(super) fn parse_etime(text: &str) -> Option<Duration> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let parts: Vec<u64> = clock
        .split(':')
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0, *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return None,
    };
    Some(Duration::from_secs(
        ((days * 24 + hours) * 60 + minutes) * 60 + seconds,
    ))
}

/// Ends the attached Claude Code: SIGTERM, then SIGKILL, each followed by a
/// bounded wait. `known` is its process id when already checked.
async fn end_interactive(pid_file: &Path, known: Option<Pid>) -> Exit {
    let pid = match known {
        Some(pid) if running(pid) => pid,
        Some(_) => return Exit::Confirmed,
        None => match holder(pid_file).await {
            Holder::Running(pid) => pid,
            Holder::Missing | Holder::Gone => return Exit::Confirmed,
        },
    };
    for (signal, wait) in [(Signal::TERM, TERM_WAIT), (Signal::KILL, KILL_WAIT)] {
        match rustix::process::kill_process(pid, signal) {
            Ok(()) => {}
            Err(rustix::io::Errno::SRCH) => return Exit::Confirmed,
            Err(error) => {
                tracing::warn!(%error, "could not end the attached Claude Code");
                return Exit::Unconfirmed;
            }
        }
        let deadline = tokio::time::Instant::now() + wait;
        while tokio::time::Instant::now() < deadline {
            if !running(pid) {
                return Exit::Confirmed;
            }
            tokio::time::sleep(EXIT_POLL).await;
        }
    }
    Exit::Unconfirmed
}

async fn remove_quietly(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            tracing::warn!(%error, "removing {} failed", path.display());
        }
        _ => {}
    }
}

/// Writes `value` beside `path` and renames it over, so a reader sees the
/// old file or the new one.
fn replace_private_json(path: &Path, value: &Value) -> anyhow::Result<()> {
    let fresh = path.with_extension("json.new");
    match std::fs::remove_file(&fresh) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    private_json(&fresh, value)?;
    std::fs::rename(&fresh, path)?;
    Ok(())
}

fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(std::io::Error::other("file is over 64 KiB"));
    }
    Ok(bytes)
}

fn read_records(dir: &Path) -> Vec<Loaded> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.take(MAX_AGENTS * 4) {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        match read_record(&path) {
            Ok(Some(loaded)) if entry.file_name() == loaded.0.identity.identity_id.as_str() => {
                found.push(loaded);
            }
            Ok(Some(_)) => tracing::warn!(
                path = %path.display(),
                "skipping an agent record filed under another identity"
            ),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "skipping an unreadable agent record");
            }
        }
        if found.len() >= MAX_AGENTS {
            break;
        }
    }
    found
}

fn read_record(dir: &Path) -> anyhow::Result<Option<Loaded>> {
    let bytes = match read_bounded(&dir.join("agent.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let (record, migrated) = serde_json::from_slice::<Stored>(&bytes)?.into_record()?;
    let spent = read_bounded(&dir.join("status.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SavedStatus>(&bytes).ok())
        .map(|status| status.spent_usd);
    Ok(Some((record, spent, migrated)))
}
