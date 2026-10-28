//! Human-authorized Claude Code workers and their durable pending batches.
// shortcut: this launch subset defers Task 5 role layering, CLI worker/budget/
// activity commands, prune API and service installation. Add those surfaces
// when the listener launcher is implemented. A process-per-batch launcher also
// defers the 95-percent mid-run wrap-up signal until streaming input is enabled.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use agmen_client::{IdentityFile, find_identity};
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{ClientEnvelope, IdentityId, IdentityKind, Payload, ProjectId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell, oneshot};

use crate::budget::{Budget, Money, Step};
use crate::console::{bad_request, params};
use crate::hub::Hub;
use crate::identities::Identities;
use crate::launch::{RunError, RunSpec, Runner};
use crate::state::ManagedIdentity;

const MAX_AGENTS: usize = 64;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Launch {
    project: ProjectId,
    name: String,
    root: Option<PathBuf>,
    model: Option<String>,
    budget_usd: Option<serde_json::Number>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    identity: ManagedIdentity,
    repo: PathBuf,
    worktree: PathBuf,
    branch: String,
    owner_session_id: String,
    others_session_id: String,
    model: Option<String>,
    budget_usd: String,
    #[serde(default)]
    source: Source,
}

/// How the agent's folder came to be.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Source {
    /// New Agent made its worktree and branch.
    #[default]
    Launched,
    /// The person joined the folder by hand; Start adopted it.
    Joined,
}

#[derive(Debug)]
struct Worker {
    record: Record,
    budget: Budget,
    state: &'static str,
    owner_resumed: bool,
    others_resumed: bool,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    last_run: Option<tokio::time::Instant>,
    /// The interactive Claude Code while the state is `attached`.
    attached: Option<Attached>,
}

impl Worker {
    fn offline(record: Record, budget: Budget) -> Self {
        Self {
            record,
            budget,
            state: "offline",
            owner_resumed: false,
            others_resumed: false,
            stop: None,
            task: None,
            last_run: None,
            attached: None,
        }
    }
}

/// An interactive Claude Code on the owner session, opened by Attach.
#[derive(Debug)]
struct Attached {
    pid_file: PathBuf,
    since: tokio::time::Instant,
    /// Whether the pid file has named a process yet.
    seen: bool,
}

/// One launch authority for the current signed-in human.
#[derive(Debug)]
pub(crate) struct Managed {
    hub: Arc<Hub>,
    identities: OnceCell<Weak<Identities>>,
    runner: Arc<dyn Runner>,
    state_dir: PathBuf,
    claude_home: Option<PathBuf>,
    roots: Mutex<BTreeMap<ProjectId, PathBuf>>,
    workers: Mutex<BTreeMap<IdentityId, Arc<Mutex<Worker>>>>,
    launching: Mutex<()>,
    shutting_down: AtomicBool,
}

impl Managed {
    pub(crate) fn contains_peer(&self, pid: i32) -> bool {
        self.runner.contains_peer(pid)
    }
    pub(crate) fn new(
        hub: Arc<Hub>,
        runner: Arc<dyn Runner>,
        state_dir: PathBuf,
        claude_home: Option<PathBuf>,
    ) -> Self {
        Self {
            hub,
            identities: OnceCell::new(),
            runner,
            state_dir,
            claude_home,
            roots: Mutex::new(BTreeMap::new()),
            workers: Mutex::new(BTreeMap::new()),
            launching: Mutex::new(()),
            shutting_down: AtomicBool::new(false),
        }
    }

    pub(crate) async fn attach(&self, identities: &Arc<Identities>) {
        self.identities
            .get_or_init(|| async { Arc::downgrade(identities) })
            .await;
    }

    fn identities(&self) -> Option<Arc<Identities>> {
        self.identities.get().and_then(Weak::upgrade)
    }

    pub(crate) async fn remember_root(&self, raw: &Value) {
        let (Some(project), Some(root)) = (
            raw.get("project").and_then(Value::as_str),
            raw.get("root").and_then(Value::as_str),
        ) else {
            return;
        };
        let Ok(project) = project.parse::<ProjectId>() else {
            return;
        };
        if let Some(root) = crate::local::checkout(&project, Path::new(root)).await {
            let mut roots = self.roots.lock().await;
            if roots.len() < MAX_AGENTS || roots.contains_key(&project) {
                roots.insert(project, root);
            }
        }
    }

    pub(crate) fn validate_launch(raw: Value) -> Result<Launch, ApiError> {
        let launch: Launch = params(raw)?;
        if launch.name.is_empty()
            || launch.name.len() > 32
            || !launch
                .name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || launch.name.starts_with('-')
        {
            return Err(bad_request(
                "name must be 1 to 32 lowercase letters, digits or hyphens and start with a letter or digit",
            ));
        }
        // The model is one argv value after --model: keep it a plain model name
        // that cannot read as another flag.
        if launch.model.as_ref().is_some_and(|model| {
            model.is_empty()
                || model.len() > 128
                || model.starts_with('-')
                || !model
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".-_:[]/@".contains(&c))
        }) {
            return Err(bad_request(
                "model must be 1 to 128 letters, digits or . - _ : [ ] / @ and not start with a hyphen",
            ));
        }
        Money::from_usd(
            &launch
                .budget_usd
                .as_ref()
                .map_or_else(|| "5".into(), ToString::to_string),
        )
        .map_err(error)?;
        Ok(launch)
    }

    pub(crate) async fn launch(self: &Arc<Self>, raw: Value) -> Result<Value, ApiError> {
        let launch = Self::validate_launch(raw)?;
        let agmen = self.runner.mcp_command().await.map_err(error)?;
        let _launching = self.launching.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(error("daemon is shutting down"));
        }
        if self.workers.lock().await.len() >= MAX_AGENTS {
            return Err(bad_request("64 managed agents are already registered"));
        }
        let root = match launch.root.clone() {
            Some(root) => root,
            None => self
                .roots
                .lock()
                .await
                .get(&launch.project)
                .cloned()
                .ok_or_else(|| {
                    bad_request("open the project's checkout before launching an agent")
                })?,
        };
        let joined = tokio::task::spawn_blocking(move || find_identity(&root))
            .await
            .map_err(error)?
            .map_err(error)?
            .ok_or_else(|| bad_request("checkout has no trusted agmen identity"))?;
        let login = self.hub.credentials().login().await.map_err(error)?;
        if joined.identity.project != launch.project
            || joined.identity.owner_id != login.human_id
            || joined.identity.hub_url.origin() != login.hub_url.origin()
        {
            return Err(bad_request(
                "checkout is not joined to this project and owner",
            ));
        }
        let branch = format!(
            "agent/{}-{}",
            launch.name,
            &crate::launch::new_session_id().map_err(error)?[..8]
        );
        let worktree = crate::worktree::create(&joined.root, "HEAD", &branch)
            .await
            .map_err(error)?;
        let (worker_root, _) = crate::worktree::worktree_root(&joined.root)
            .await
            .map_err(error)?;
        let repo = worker_root
            .parent()
            .ok_or_else(|| error("worker root has no main checkout"))?
            .to_owned();
        let result = self
            .complete_launch(
                launch,
                joined,
                login,
                branch.clone(),
                worktree.clone(),
                agmen,
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

    async fn complete_launch(
        self: &Arc<Self>,
        launch: Launch,
        joined: agmen_client::Joined,
        login: agmen_client::Login,
        branch: String,
        worktree: PathBuf,
        agmen: PathBuf,
    ) -> Result<Value, ApiError> {
        let (worker_root, _) = crate::worktree::worktree_root(&joined.root)
            .await
            .map_err(error)?;
        let repo = worker_root
            .parent()
            .ok_or_else(|| error("worker root has no main checkout"))?
            .to_owned();
        let identity = self.mint(&launch, &joined, &login).await?;
        let handle = identity.handle.clone();
        write_identity(&worktree, &identity).await?;
        let metadata = metadata_for(&identity);
        let identities = self
            .identities()
            .ok_or_else(|| error("identity service stopped"))?;
        identities.register(metadata.clone()).await?;
        let record = Record {
            identity: metadata,
            repo,
            worktree: worktree.clone(),
            branch: branch.clone(),
            owner_session_id: crate::launch::new_session_id().map_err(error)?,
            others_session_id: crate::launch::new_session_id().map_err(error)?,
            model: launch.model.or_else(|| Some("sonnet".into())),
            budget_usd: launch
                .budget_usd
                .map_or_else(|| "5".into(), |n| n.to_string()),
            source: Source::Launched,
        };
        let config_dir = self.agent_dir(&identity.identity_id);
        let record_copy = record.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&config_dir)?;
            private_json(
                &config_dir.join("agent.json"),
                &serde_json::to_value(&record_copy)?,
            )?;
            private_json(
                &config_dir.join("mcp.json"),
                &json!({"mcpServers":{"agmen":{"command":agmen,"args":["mcp"]}}}),
            )
        })
        .await
        .map_err(error)?
        .map_err(error)?;
        let budget = Budget::new(Money::from_usd(&record.budget_usd).map_err(error)?);
        let model = record.model.clone();
        let mut workers = self.workers.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(error("daemon is shutting down"));
        }
        let mut worker = Worker::offline(record, budget);
        worker.state = "idle";
        workers.insert(identity.identity_id.clone(), Arc::new(Mutex::new(worker)));
        drop(workers);
        // HELLO goes through the same durable identity outbox as agent chat.
        let hello = actions::hello(model, branch.clone())?;
        if let Err(error) = identities.post(&metadata_for(&identity), hello).await {
            tracing::warn!(code=?error.code,"managed agent HELLO is pending connection");
        }
        Ok(
            json!({"identity":identity.identity_id,"identity_id":identity.identity_id,"handle":handle,"worktree":worktree,"branch":branch,"state":"idle"}),
        )
    }

    async fn mint(
        &self,
        launch: &Launch,
        joined: &agmen_client::Joined,
        login: &agmen_client::Login,
    ) -> Result<IdentityFile, ApiError> {
        let handle = format!("{}/{}", login.handle, launch.name);
        let minted = self
            .hub
            .mint_identity(&json!({"handle":handle,"project":launch.project,"kind":"agent"}))
            .await?;
        #[derive(Deserialize)]
        struct Minted {
            id: IdentityId,
            handle: String,
            tenant: agmen_proto::TenantId,
            project: ProjectId,
            token: String,
        }
        let minted: Minted = serde_json::from_value(minted).map_err(error)?;
        if minted.handle != handle
            || minted.project != launch.project
            || minted.tenant != joined.identity.tenant
        {
            return Err(error("hub minted a different identity than requested"));
        }
        let identity = IdentityFile {
            tenant: minted.tenant,
            project: minted.project,
            identity_id: minted.id,
            handle: minted.handle,
            owner_id: login.human_id.clone(),
            hub_url: login.hub_url.clone(),
            broker_url: joined.identity.broker_url.clone(),
        };
        self.hub
            .credentials()
            .set_token(&identity.hub_url, &identity.identity_id, minted.token)
            .await?;
        Ok(identity)
    }

    pub(crate) async fn list(&self, raw: Value) -> Result<Value, ApiError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct List {
            project: Option<ProjectId>,
        }
        let list: List = params(raw)?;
        let login = self.hub.credentials().login().await.map_err(error)?;
        let mut agents = Vec::new();
        for worker in self.workers.lock().await.values() {
            let worker = worker.lock().await;
            let record = &worker.record;
            if record.identity.owner_id != login.human_id
                || record.identity.hub_url.origin() != login.hub_url.origin()
                || list
                    .project
                    .as_ref()
                    .is_some_and(|project| project != &record.identity.project)
            {
                continue;
            }
            agents.push(json!({"identity":record.identity.identity_id,"project":record.identity.project,"handle":record.identity.handle,"state":worker.state,"model":record.model,"budget_usd":record.budget_usd,"spent_usd":worker.budget.spent().usd(),"worktree":record.worktree,"branch":record.branch,"source":record.source}));
        }
        Ok(json!({"agents":agents}))
    }

    pub(crate) async fn stop(&self, raw: Value) -> Result<Value, ApiError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Stop {
            identity: IdentityId,
        }
        let Stop { identity } = params(raw)?;
        let worker = self
            .workers
            .lock()
            .await
            .get(&identity)
            .cloned()
            .ok_or_else(|| bad_request("agent is not managed by this daemon"))?;
        let login = self.hub.credentials().login().await.map_err(error)?;
        {
            let worker = worker.lock().await;
            if worker.record.identity.owner_id != login.human_id
                || worker.record.identity.hub_url.origin() != login.hub_url.origin()
            {
                return Err(crate::identities::forbidden());
            }
        }
        self.stop_worker(worker).await?;
        Ok(json!({"identity":identity,"state":"offline"}))
    }

    async fn stop_worker(&self, worker: Arc<Mutex<Worker>>) -> Result<(), ApiError> {
        let (task, identity) = {
            let mut worker = worker.lock().await;
            if worker.state == "offline" {
                return Ok(());
            }
            worker.state = "offline";
            // An attached Claude Code is the person's own session: it keeps running.
            worker.attached = None;
            if let Some(stop) = worker.stop.take() {
                let _sent = stop.send(());
            }
            (worker.task.take(), worker.record.identity.clone())
        };
        if let Some(mut task) = task
            && tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _joined = task.await;
        }
        let identities = self
            .identities()
            .ok_or_else(|| error("identity service stopped"))?;
        let result = identities
            .post(&identity, envelope(Payload::Bye {}, "Stopped by owner.")?)
            .await;
        identities.close_identity(&identity).await;
        result.map(|_| ())
    }

    pub(crate) async fn shutdown(self: &Arc<Self>) {
        self.shutting_down.store(true, Ordering::Release);
        let workers: Vec<_> = self.workers.lock().await.values().cloned().collect();
        let mut tasks = tokio::task::JoinSet::new();
        for worker in workers {
            let manager = self.clone();
            tasks.spawn(async move {
                if let Err(error) = manager.stop_worker(worker).await {
                    tracing::warn!(code=?error.code,"managed agent goodbye failed");
                }
            });
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while !tasks.is_empty() {
            if tokio::time::timeout_at(deadline, tasks.join_next())
                .await
                .is_err()
            {
                tasks.abort_all();
                break;
            }
        }
        while tasks.join_next().await.is_some() {}
    }

    pub(crate) async fn supervise(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            self.tick().await;
        }
    }

    pub(crate) async fn tick(self: &Arc<Self>) {
        let Some(identities) = self.identities() else {
            return;
        };
        let workers: Vec<_> = self.workers.lock().await.values().cloned().collect();
        if workers.is_empty() {
            return;
        }
        let login = self.hub.credentials().login().await;
        for worker in workers {
            let mut held = worker.lock().await;
            if !login.as_ref().is_ok_and(|login| {
                login.human_id == held.record.identity.owner_id
                    && login.hub_url.origin() == held.record.identity.hub_url.origin()
            }) {
                drop(held);
                if let Err(error) = self.stop_worker(worker).await {
                    tracing::warn!(code=?error.code,"stopping agent after login changed failed");
                }
                continue;
            }
            if held.state == "attached" {
                if self.attach_ended(&mut held).await {
                    self.finish_attach(&mut held).await;
                }
                continue;
            }
            if held.state != "idle"
                || held.budget.exhausted()
                || held.task.as_ref().is_some_and(|task| !task.is_finished())
            {
                continue;
            }
            // shortcut: one batch every 30 seconds; make configurable if real workloads need lower latency.
            if held
                .last_run
                .is_some_and(|last| last.elapsed() < Duration::from_secs(30))
            {
                continue;
            }
            let rows = match identities.launch_pending(&held.record.identity).await {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(code=?error.code,"managed pending read failed");
                    continue;
                }
            };
            if rows.is_empty() {
                continue;
            }
            let prompt = match serde_json::to_string(&rows) {
                Ok(prompt) => prompt,
                Err(error) => {
                    tracing::error!(%error,"encoding pending batch failed");
                    continue;
                }
            };
            let owner_only = rows.iter().all(|row| {
                row.from == held.record.identity.owner_id && row.from_kind == IdentityKind::Human
            });
            // Keep untrusted conversation history out of bypassPermissions batches.
            let (session_id, resume) = if owner_only {
                (held.record.owner_session_id.clone(), held.owner_resumed)
            } else {
                (held.record.others_session_id.clone(), held.others_resumed)
            };
            let spec = RunSpec {
                cwd: held.record.worktree.clone(),
                prompt,
                session_id,
                resume,
                model: held.record.model.clone(),
                budget_remaining: held.budget.remaining_usd(),
                mcp_config: self
                    .agent_dir(&held.record.identity.identity_id)
                    .join("mcp.json"),
                owner_handle: held
                    .record
                    .identity
                    .handle
                    .split('/')
                    .next()
                    .unwrap_or_default()
                    .into(),
                owner_id: held.record.identity.owner_id.clone(),
                owner_only,
            };
            held.last_run = Some(tokio::time::Instant::now());
            held.state = "running";
            let (stop, stopped) = oneshot::channel();
            held.stop = Some(stop);
            let worker = worker.clone();
            let runner = self.runner.clone();
            let identities = identities.clone();
            let status = self.agent_dir(&held.record.identity.identity_id);
            held.task = Some(tokio::spawn(run_worker(
                worker, runner, spec, stopped, identities, status,
            )));
        }
    }
}

async fn run_worker(
    worker: Arc<Mutex<Worker>>,
    runner: Arc<dyn Runner>,
    spec: RunSpec,
    stopped: oneshot::Receiver<()>,
    identities: Arc<Identities>,
    agent_dir: PathBuf,
) {
    let owner_only = spec.owner_only;
    let result = runner.run(spec, stopped).await;
    let mut held = worker.lock().await;
    held.stop = None;
    // A batch that never spawned Claude created no session: the next batch
    // of this tier must start it with --session-id, not resume it.
    if result.as_ref().map_or_else(RunError::started, |_| true) {
        if owner_only {
            held.owner_resumed = true;
        } else {
            held.others_resumed = true;
        }
    }
    if held.state == "offline" {
        return;
    }
    // Attach cancelled it: like Stop, the person's own cancel does not
    // reserve the rest of the budget, which would end every later run.
    if held.state == "attached" && matches!(result, Err(RunError::Cancelled)) {
        return;
    }
    // Attach cancels a batch and keeps its own state.
    if held.state == "running" {
        held.state = "idle";
    }
    match result {
        Ok(result) => {
            let mut steps = Vec::new();
            // shortcut: conservatively add per-process totals; if resumed
            // totals include history this stops early. Revisit after a live
            // cost-format test establishes resume accounting semantics.
            if let Err(error) = held
                .budget
                .add_cost_usd(&result.total_cost_usd, |step| steps.push(step))
            {
                tracing::error!(%error,"invalid launcher cost");
                return;
            }
            let stopped = steps.contains(&Step::Stop);
            let warn = steps.contains(&Step::Warn);
            let record = held.record.clone();
            let spent = held.budget.spent();
            drop(held);
            actions::save_spent(&agent_dir, spent).await;
            if warn {
                let message = envelope(
                    Payload::Alert {
                        need: "Agent budget is at least 80 percent spent.".into(),
                        urgency: agmen_proto::Urgency::Normal,
                        needs: agmen_proto::Needs::Owner,
                        actions: Vec::new(),
                    },
                    "Agent budget is at least 80 percent spent.",
                );
                if let Ok(message) = message
                    && let Err(error) = identities.post(&record.identity, message).await
                {
                    tracing::warn!(code=?error.code,"budget notification failed");
                }
            }
            if stopped
                && let Err(error) =
                    crate::worktree::save_wip(&record.repo, &record.worktree, &record.branch).await
            {
                tracing::warn!(%error,"budget WIP snapshot failed");
            }
        }
        Err(error) => {
            let cost = error
                .reported_cost()
                .map(str::to_owned)
                .unwrap_or_else(|| held.budget.remaining_usd());
            // shortcut: charge unknown interrupted usage against the remaining cap
            // until failed batches can supply durable usage receipts.
            if let Err(error) = held.budget.add_cost_usd(&cost, |_| {}) {
                tracing::error!(%error,"charging failed batch cost failed");
            }
            let identity = held.record.identity.clone();
            let spent = held.budget.spent();
            drop(held);
            actions::save_spent(&agent_dir, spent).await;
            let explanation = if error.reported_cost().is_none() {
                " Remaining budget is reserved because usage was not reported."
            } else {
                ""
            };
            if let Ok(message) = envelope(
                Payload::Fyi {},
                &format!("Claude Code batch stopped: {error}.{explanation}"),
            ) && let Err(error) = identities.post(&identity, message).await
            {
                tracing::warn!(code=?error.code,"batch failure report failed");
            }
        }
    }
}

fn metadata_for(identity: &IdentityFile) -> ManagedIdentity {
    ManagedIdentity {
        hub_url: identity.hub_url.clone(),
        broker_url: identity.broker_url.clone(),
        tenant: identity.tenant.clone(),
        project: identity.project.clone(),
        identity_id: identity.identity_id.clone(),
        owner_id: identity.owner_id.clone(),
        handle: identity.handle.clone(),
        kind: IdentityKind::Agent,
    }
}

fn error(error: impl std::fmt::Display) -> ApiError {
    ApiError {
        code: ErrorCode::Hub,
        message: format!("managed agent: {error}"),
    }
}

fn private_json(path: &Path, value: &Value) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}

async fn write_identity(root: &Path, identity: &IdentityFile) -> Result<(), ApiError> {
    let git = crate::git::Git::new(root, tokio::time::Instant::now() + Duration::from_secs(5));
    let output = git
        .run_str(&["rev-parse", "--git-path", "info/exclude"], 65536)
        .await
        .map_err(error)?;
    let exclude = root.join(std::str::from_utf8(&output.stdout).map_err(error)?.trim());
    let root = root.to_owned();
    let identity = identity.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        if let Some(parent) = exclude.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let previous = std::fs::read_to_string(&exclude).unwrap_or_default();
        if !previous.lines().any(|line| line == ".agmen/") {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(exclude)?;
            writeln!(file, "\n.agmen/")?;
        }
        let dir = root.join(".agmen");
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) => {
                anyhow::ensure!(meta.is_dir(), ".agmen is not a real directory");
                anyhow::ensure!(
                    std::fs::read_dir(&root)?
                        .any(|entry| entry.is_ok_and(|entry| entry.file_name() == ".agmen")),
                    ".agmen directory has another stored spelling"
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new().mode(0o700).create(&dir)?
            }
            Err(error) => return Err(error.into()),
        }
        private_json(
            &dir.join("identity.json"),
            &serde_json::to_value(&identity)?,
        )?;
        anyhow::ensure!(
            find_identity(&root)?.is_some_and(|joined| joined.identity == identity),
            "new identity does not pass trust check"
        );
        Ok(())
    })
    .await
    .map_err(error)?
    .map_err(error)
}

fn envelope(payload: Payload, body: &str) -> Result<ClientEnvelope, ApiError> {
    // Reuse the runner's OS-random UUID bytes for the 80 random ULID bits.
    let random = crate::launch::new_session_id()
        .map_err(error)?
        .replace('-', "");
    let timestamp = Timestamp::now();
    let ms = u128::try_from(timestamp.as_millisecond()).map_err(error)? & ((1 << 48) - 1);
    let bits = u128::from_str_radix(&random[..20], 16).map_err(error)?;
    let mut value = (ms << 80) | bits;
    const DIGITS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut id = [b'0'; 26];
    for digit in id.iter_mut().rev() {
        *digit = DIGITS[(value & 31) as usize];
        value >>= 5;
    }
    Ok(ClientEnvelope {
        v: 1,
        id: std::str::from_utf8(&id)
            .map_err(error)?
            .parse()
            .map_err(error)?,
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: body.to_owned().try_into().map_err(error)?,
        valid_until: None,
        sent_at: timestamp,
        payload,
    })
}

// Start, Attach, Detach and Remove.
#[path = "managed_actions.rs"]
mod actions;

#[cfg(test)]
#[path = "managed_tests.rs"]
mod tests;
