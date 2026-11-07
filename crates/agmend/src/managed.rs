//! Human-authorized Claude Code workers and their durable pending batches.
// shortcut: activity commands and the 95-percent mid-run wrap-up signal
// remain deferred until the process-per-batch runner supports streaming output.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use agmen_client::{IdentityFile, find_identity};
use agmen_local::safeguards::MAX_MANAGED;
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{ClientEnvelope, IdentityId, IdentityKind, Payload, ProjectId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell, oneshot};

use crate::budget::{Budget, Money, Step};
use crate::console::{bad_request, params};
use crate::hub::Hub;
use crate::identities::{Identities, LocalToken};
use crate::launch::{RoleRun, RunError, RunSpec, Runner};
use crate::state::ManagedIdentity;
use roles::ReviewedBase;

const MAX_ROLE_PROMPT_BYTES: usize = 64 * 1024;
pub(crate) const QUIT_WAIT: Duration = Duration::from_secs(8);
/// How long quit lets an admitted launch finish before abandoning it.
const QUIT_LAUNCH_WAIT: Duration = Duration::from_secs(4);
const QUIT_LEVEL_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Launch {
    project: ProjectId,
    name: String,
    root: Option<PathBuf>,
    model: Option<String>,
    budget_usd: Option<serde_json::Number>,
    role: Option<String>,
}

/// The branch and folder a launch created for its worker.
struct Placement {
    branch: String,
    worktree: PathBuf,
    parent: Option<Record>,
    depth: u32,
    brief: Option<ClientEnvelope>,
    shared_budget: bool,
    role_commit: Option<String>,
    reviewed_base: Option<ReviewedBase>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<agmen_proto::RoleName>,
    /// The hash of the role as minted; a batch refuses to run if the role's
    /// files no longer resolve to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role_sha256: Option<agmen_proto::Sha256Hex>,
    /// The layer that supplied the role at launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role_layer: Option<String>,
    /// The reviewed project-role commit for a child, independent of its worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role_commit: Option<String>,
    /// The commit this agent's tree reads project roles at: recorded when the
    /// person launched its root, inherited by every descendant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reviewed_base: Option<ReviewedBase>,
    /// Hub-granted verbs, pinned at mint rather than read from an agent's files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verbs: Option<std::collections::BTreeSet<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<IdentityId>,
    #[serde(default = "root_depth")]
    depth: u32,
    #[serde(default)]
    shared_budget: bool,
}

fn root_depth() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Owner,
    Others,
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
    stopped_at: Option<Timestamp>,
    owner_resumed: bool,
    others_resumed: bool,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    last_run: Option<tokio::time::Instant>,
    /// The interactive Claude Code while the state is `attached`.
    attached: Option<Attached>,
    role_error: Option<String>,
    batch_tier: Option<Tier>,
    /// Unspent funds still held in the parent's reservation, including live descendants.
    reserved_from_parent: Money,
}

impl Worker {
    fn offline(record: Record, budget: Budget) -> Self {
        Self {
            record,
            budget,
            state: "offline",
            stopped_at: None,
            owner_resumed: false,
            others_resumed: false,
            stop: None,
            task: None,
            last_run: None,
            attached: None,
            role_error: None,
            batch_tier: None,
            reserved_from_parent: Money::ZERO,
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
    /// The process the pid file named, once checked to be the one the
    /// script started.
    pid: Option<rustix::process::Pid>,
}

/// One launch authority for the current signed-in human.
#[derive(Debug)]
pub(crate) struct Managed {
    origins: Arc<crate::origin::Origins>,
    hub: Arc<Hub>,
    identities: OnceCell<Weak<Identities>>,
    runner: Arc<dyn Runner>,
    state_dir: PathBuf,
    claude_home: Option<PathBuf>,
    personal_roles: Option<PathBuf>,
    settings_file: Option<PathBuf>,
    roots: Mutex<BTreeMap<ProjectId, PathBuf>>,
    workers: Mutex<BTreeMap<IdentityId, Arc<Mutex<Worker>>>>,
    tree: Mutex<BTreeMap<IdentityId, Option<IdentityId>>>,
    launching: Mutex<()>,
    /// Serializes reservations, refunds, cap raises and spend propagation.
    accounting: Mutex<()>,
    safeguards_cache: Mutex<budgets::SettingsCache>,
    /// At most MAX_MANAGED successful resolutions, keyed by repo, name and commit.
    role_cache: Mutex<BTreeMap<(PathBuf, String, String), agmen_roles::Resolved>>,
    shutting_down: AtomicBool,
    /// Set when quit gives up on an admitted launch; it then discards what it made.
    abandon_launch: tokio::sync::watch::Sender<bool>,
    quit_launch_wait: Duration,
}

impl Managed {
    pub(crate) fn contains_peer(&self, pid: i32) -> bool {
        self.runner.contains_peer(pid)
    }
    pub(crate) fn new(
        origins: Arc<crate::origin::Origins>,
        runner: Arc<dyn Runner>,
        state_dir: PathBuf,
        claude_home: Option<PathBuf>,
        personal_roles: Option<PathBuf>,
        settings_file: Option<PathBuf>,
    ) -> Self {
        Self {
            hub: origins.hub().clone(),
            origins,
            identities: OnceCell::new(),
            runner,
            state_dir,
            claude_home,
            personal_roles,
            settings_file,
            roots: Mutex::new(BTreeMap::new()),
            workers: Mutex::new(BTreeMap::new()),
            tree: Mutex::new(BTreeMap::new()),
            launching: Mutex::new(()),
            accounting: Mutex::new(()),
            safeguards_cache: Mutex::new(budgets::SettingsCache::default()),
            role_cache: Mutex::new(BTreeMap::new()),
            shutting_down: AtomicBool::new(false),
            abandon_launch: tokio::sync::watch::channel(false).0,
            quit_launch_wait: QUIT_LAUNCH_WAIT,
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
            if roots.len() < MAX_MANAGED || roots.contains_key(&project) {
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
        self.launch_at(launch, root, None, None).await
    }

    async fn complete_launch(
        self: &Arc<Self>,
        launch: Launch,
        joined: agmen_client::Joined,
        login: agmen_client::Login,
        placement: Placement,
        agmen: PathBuf,
        role: Option<agmen_roles::Resolved>,
    ) -> Result<Value, ApiError> {
        let (worker_root, _) = crate::worktree::worktree_root(&joined.root)
            .await
            .map_err(error)?;
        let repo = worker_root
            .parent()
            .ok_or_else(|| error("worker root has no main checkout"))?
            .to_owned();
        let identity = self
            .mint(
                &launch,
                &joined,
                &login,
                role.as_ref(),
                placement.parent.as_ref(),
            )
            .await?;
        // The hub now holds a live identity and agmend a credential for it, so
        // any failure from here on takes both back.
        let mut abandoned = self.abandon_launch.subscribe();
        let registered = tokio::select! {
            registered = self.register_launch(launch, &identity, placement, agmen, role, repo) => registered,
            _ = abandoned.wait_for(|abandoned| *abandoned) => Err(error("daemon is shutting down")),
        };
        match registered {
            Ok(answer) => Ok(answer),
            Err(failure) => Err(self.discard_identity(&identity, failure).await),
        }
    }

    /// Everything a launch does once the hub has minted the identity.
    async fn register_launch(
        self: &Arc<Self>,
        launch: Launch,
        identity: &IdentityFile,
        placement: Placement,
        agmen: PathBuf,
        role: Option<agmen_roles::Resolved>,
        repo: PathBuf,
    ) -> Result<Value, ApiError> {
        let Placement {
            branch,
            worktree,
            parent,
            depth,
            brief,
            shared_budget,
            role_commit,
            reviewed_base,
        } = placement;
        write_identity(&worktree, identity).await?;
        let mut metadata = metadata_for(identity);
        if let Some(role) = &role {
            metadata.kind = role_kind(role);
            self.write_role_prompt(&identity.identity_id, role).await?;
        }
        let identities = self
            .identities()
            .ok_or_else(|| error("identity service stopped"))?;
        identities.register(metadata.clone()).await?;
        let budget_usd = launch
            .budget_usd
            .as_ref()
            .map_or_else(|| "5".into(), ToString::to_string);
        let budget_usd = if parent.is_some() {
            Money::from_usd(&budget_usd).map_err(error)?.usd()
        } else {
            budget_usd
        };
        let record = Record {
            identity: metadata.clone(),
            repo,
            worktree: worktree.clone(),
            branch: branch.clone(),
            owner_session_id: crate::launch::new_session_id().map_err(error)?,
            others_session_id: crate::launch::new_session_id().map_err(error)?,
            model: launch.model.or_else(|| Some("sonnet".into())),
            budget_usd,
            source: Source::Launched,
            role: identity.role.clone(),
            role_sha256: identity.role_sha256.clone(),
            role_layer: role.as_ref().map(|role| role.layer.to_string()),
            role_commit,
            reviewed_base,
            verbs: identity.verbs.clone(),
            parent: parent
                .as_ref()
                .map(|parent| parent.identity.identity_id.clone()),
            depth,
            shared_budget,
        };
        let record_layer = record.role_layer.clone();
        let local_token = identities.local_token(&identity.identity_id).await?;
        let config = mcp_config(&agmen, &identity.identity_id, local_token.as_ref());
        write_agent_files(
            self.agent_dir(&identity.identity_id),
            record.clone(),
            config,
        )
        .await?;
        let budget = Budget::new(Money::from_usd(&record.budget_usd).map_err(error)?);
        let model = record.model.clone();
        let mut worker = Worker::offline(record, budget);
        if shared_budget {
            worker.reserved_from_parent = worker.budget.unspent();
        }
        worker.state = "idle";
        // HELLO goes through the same durable identity outbox as agent chat.
        let hello = actions::hello(model, branch.clone())?;
        if let Err(error) = identities.post(&metadata, hello).await {
            tracing::warn!(code=?error.code,"managed agent HELLO is pending connection");
        }
        if let (Some(parent), Some(message)) = (&parent, brief) {
            tree::deliver_brief(
                &identities,
                &parent.identity,
                &identity.identity_id,
                message,
            )
            .await?;
        }
        self.insert_worker(worker).await?;
        let mut answer = json!({"identity":identity.identity_id,"identity_id":identity.identity_id,"handle":identity.handle,"worktree":worktree,"branch":branch,"state":"idle"});
        answer["parent"] = json!(parent.as_ref().map(|parent| &parent.identity.identity_id));
        answer["depth"] = json!(depth);
        if let Some(layer) = &record_layer {
            answer["layer"] = json!(layer);
        }
        Ok(answer)
    }

    async fn mint(
        &self,
        launch: &Launch,
        joined: &agmen_client::Joined,
        login: &agmen_client::Login,
        role: Option<&agmen_roles::Resolved>,
        parent: Option<&Record>,
    ) -> Result<IdentityFile, ApiError> {
        let handle = format!("{}/{}", login.handle, launch.name);
        let mut body = json!({"handle":handle,"project":launch.project,"kind":"agent"});
        if let Some(parent) = parent {
            body["delegated_by"] = json!(parent.identity.identity_id);
        }
        let spec = role
            .map(|role| {
                Ok::<_, ApiError>(agmen_proto::RoleSpec {
                    role: role.role.name.parse().map_err(error)?,
                    verbs: role.verbs.clone(),
                    edits: role.edits.clone().into(),
                    role_sha256: role.sha256.parse().map_err(error)?,
                })
            })
            .transpose()?;
        if let (Some(role), Some(spec)) = (role, &spec) {
            body["kind"] = serde_json::to_value(role_kind(role)).map_err(error)?;
            body["role"] = json!(spec.role);
            body["verbs"] = json!(spec.verbs);
            body["edits"] = json!(spec.edits);
            body["role_sha256"] = json!(spec.role_sha256);
        }
        if self.origins.of(&launch.project).await? == crate::origin::Origin::Local {
            let minted = self
                .origins
                .local()
                .mint(crate::record::admin::LocalMint {
                    project: launch.project.clone(),
                    name: launch.name.clone(),
                    kind: role.map_or(IdentityKind::Agent, role_kind),
                    spec: spec.clone(),
                    agent: None,
                    delegated_by: parent.map(|parent| parent.identity.identity_id.clone()),
                })
                .await?;
            return Ok(IdentityFile {
                tenant: minted.topics.tenant().clone(),
                project: minted.topics.project().clone(),
                identity_id: minted.id,
                handle: minted.handle,
                owner_id: login.human_id.clone(),
                hub_url: agmen_client::local_hub_url(),
                broker_url: agmen_client::local_broker(),
                role: spec.as_ref().map(|spec| spec.role.clone()),
                role_sha256: spec.map(|spec| spec.role_sha256),
                verbs: minted.verbs,
            });
        }
        let minted = self.hub.mint_identity(&body).await?;
        #[derive(Deserialize)]
        struct Minted {
            id: IdentityId,
            handle: String,
            tenant: agmen_proto::TenantId,
            project: ProjectId,
            token: String,
            verbs: Option<std::collections::BTreeSet<String>>,
        }
        let minted: Minted = serde_json::from_value(minted).map_err(error)?;
        if minted.handle != handle
            || minted.project != launch.project
            || minted.tenant != joined.identity.tenant
        {
            return Err(error("hub minted a different identity than requested"));
        }
        if spec.is_some() && minted.verbs.is_none() {
            return Err(error("hub omitted the role's capped verbs"));
        }
        let identity = IdentityFile {
            tenant: minted.tenant,
            project: minted.project,
            identity_id: minted.id,
            handle: minted.handle,
            owner_id: login.human_id.clone(),
            hub_url: login.hub_url.clone(),
            broker_url: joined.identity.broker_url.clone(),
            role: spec.as_ref().map(|spec| spec.role.clone()),
            role_sha256: spec.as_ref().map(|spec| spec.role_sha256.clone()),
            verbs: spec.and(minted.verbs),
        };
        self.hub
            .credentials()
            .set_token(&identity.hub_url, &identity.identity_id, minted.token)
            .await?;
        Ok(identity)
    }

    async fn resolve_role(
        &self,
        repo: &Path,
        name: &str,
    ) -> Result<agmen_roles::Resolved, ApiError> {
        let role = self.resolve_any_role(repo, name).await?;
        if role.kind == agmen_roles::Kind::Listener {
            return Err(bad_request(
                "listeners are started by the listener runtime, not agent_launch",
            ));
        }
        Ok(role)
    }

    pub(crate) async fn resolve_any_role(
        &self,
        repo: &Path,
        name: &str,
    ) -> Result<agmen_roles::Resolved, ApiError> {
        self.resolve_role_layers(Some(repo), name).await
    }

    pub(crate) async fn known_checkout(&self, project: &ProjectId) -> Option<PathBuf> {
        let root = self.roots.lock().await.get(project).cloned()?;
        crate::local::checkout(project, &root).await
    }

    /// Without a known checkout, only built-in and personal roles apply.
    pub(crate) async fn resolve_role_layers(
        &self,
        repo: Option<&Path>,
        name: &str,
    ) -> Result<agmen_roles::Resolved, ApiError> {
        let layers = agmen_roles::Layers {
            project: repo.map(Path::to_owned),
            personal: self.personal_roles.clone(),
            ..agmen_roles::Layers::default()
        };
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let catalog = layers.load().map_err(|failure| {
                let names = layers.builtin.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(", ");
                bad_request(format!("role {name:?} could not load: {failure}; available built-in names: {names}; local names unavailable"))
            })?;
            let role = catalog.get(&name).ok_or_else(|| {
                let names = catalog.iter().map(|role| role.role.name.as_str()).collect::<Vec<_>>().join(", ");
                bad_request(format!("unknown role {name:?}; available names: {names}"))
            })?;
            if let Some(shadow) = shadowed_builtin(&catalog, &layers.builtin, role) {
                return Err(bad_request(format!("role {name:?} uses the project-layer role {shadow:?}, which takes the name of a built-in role; project roles may not shadow built-in roles (use a personal role)")));
            }
            if agmen_roles::render_prompt(role).len() > MAX_ROLE_PROMPT_BYTES {
                return Err(bad_request(format!("role {name:?} prompt exceeds 64 KiB")));
            }
            Ok(role.clone())
        }).await.map_err(error)?
    }

    pub(crate) async fn write_role_prompt(
        &self,
        id: &IdentityId,
        role: &agmen_roles::Resolved,
    ) -> Result<RoleRun, ApiError> {
        let prompt = agmen_roles::render_prompt(role);
        if prompt.len() > MAX_ROLE_PROMPT_BYTES {
            return Err(bad_request(format!(
                "role {:?} prompt exceeds 64 KiB",
                role.role.name
            )));
        }
        let dir = self.state_dir.join("agents").join(id.as_str());
        let prompt_file = dir.join("role-prompt.md");
        let path = prompt_file.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)?;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            let mut fresh = tempfile::NamedTempFile::new_in(&dir)?;
            fresh
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
            fresh.write_all(prompt.as_bytes())?;
            fresh.as_file().sync_all()?;
            fresh.persist(path)?;
            Ok(())
        })
        .await
        .map_err(error)?
        .map_err(error)?;
        // shortcut: only an edit scope of none is enforced here, by dropping the
        // edit tools. Path-scoped scopes (lanes, within, paths) rest on the
        // plugin's lane guard staying on in managed sessions; revisit when
        // Claude Code path-scoped permission rules can carry them.
        let no_edits = role.edits == agmen_roles::EditScope::None;
        let tools = agmen_roles::claude_tools(role)
            .into_iter()
            .filter(|tool| !(no_edits && matches!(*tool, "Edit" | "Write" | "NotebookEdit")))
            .collect::<Vec<_>>()
            .join(",");
        Ok(RoleRun { tools, prompt_file })
    }

    fn run_spec(
        &self,
        held: &Worker,
        prompt: String,
        (session_id, resume): (String, bool),
        owner_only: bool,
        role: Option<RoleRun>,
        cap_budget: bool,
    ) -> RunSpec {
        RunSpec {
            cwd: held.record.worktree.clone(),
            prompt,
            session_id,
            resume,
            model: held.record.model.clone(),
            budget_remaining: held.budget.remaining_usd(),
            cap_budget,
            identity: held.record.identity.identity_id.clone(),
            local_token: None,
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
            role,
        }
    }

    /// The role's tools and a freshly written prompt file for the next batch,
    /// or `None` for an agent launched without a role. A role that no longer
    /// resolves is an error: the batch must not run unrestricted.
    async fn batch_role(&self, record: &Record) -> Result<Option<RoleRun>, ApiError> {
        let Some(name) = &record.role else {
            return Ok(None);
        };
        let role = self.record_role(record, name.as_str()).await?;
        if record.role_sha256.as_ref().map(|sha| sha.as_str()) != Some(role.sha256.as_str()) {
            return Err(error(format!(
                "role {name} changed since launch; relaunch the agent to use it"
            )));
        }
        self.write_role_prompt(&record.identity.identity_id, &role)
            .await
            .map(Some)
    }

    pub(crate) async fn list(&self, raw: Value) -> Result<Value, ApiError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct List {
            project: Option<ProjectId>,
        }
        let list: List = params(raw)?;
        let _settings = self.safeguards().await;
        let settings_error = self.safeguards_cache.lock().await.message();
        let _accounting = self.accounting.lock().await;
        let mut agents = Vec::new();
        let mut spends = BTreeMap::new();
        let mut local_members =
            BTreeMap::<ProjectId, std::collections::BTreeSet<IdentityId>>::new();
        for worker in self.workers.lock().await.values() {
            let worker = worker.lock().await;
            let record = &worker.record;
            let login = match self.origins.person_for(&record.identity.hub_url).await {
                Ok(login) => login,
                Err(failure) if agmen_client::is_local(&record.identity.hub_url) => {
                    return Err(failure);
                }
                Err(_) => continue,
            };
            if record.identity.owner_id != login.human_id
                || agmen_client::origin_key(&record.identity.hub_url)
                    != agmen_client::origin_key(&login.hub_url)
                || list
                    .project
                    .as_ref()
                    .is_some_and(|project| project != &record.identity.project)
            {
                continue;
            }
            let mut entry = json!({"identity":record.identity.identity_id,"project":record.identity.project,"handle":record.identity.handle,"state":worker.state,"model":record.model,"budget_usd":record.budget_usd,"spent_usd":worker.budget.spent().usd(),"worktree":record.worktree,"branch":record.branch,"source":record.source,"role":record.role,"layer":record.role_layer});
            entry["parent"] = json!(record.parent);
            entry["depth"] = json!(record.depth);
            entry["stopped_at"] = json!(worker.stopped_at);
            // One roster read per local project, not per agent: the desktop polls this.
            let resumable = if agmen_client::is_local(&record.identity.hub_url) {
                let members = match local_members.entry(record.identity.project.clone()) {
                    std::collections::btree_map::Entry::Occupied(found) => found.into_mut(),
                    std::collections::btree_map::Entry::Vacant(slot) => {
                        let roster = self
                            .origins
                            .local()
                            .roster(&record.identity.project)
                            .await?;
                        slot.insert(roster.into_iter().map(|entry| entry.identity_id).collect())
                    }
                };
                members.contains(&record.identity.identity_id)
            } else {
                true
            };
            entry["resumable"] = json!(resumable);
            entry["reserved_usd"] = json!(worker.budget.reserved().usd());
            entry["children_spent_usd"] = json!(worker.budget.children_spent().usd());
            spends.insert(record.identity.identity_id.clone(), worker.budget.spent());
            if let Some(failure) = &worker.role_error {
                entry["role_error"] = json!(failure);
            }
            agents.push(entry);
        }
        let totals = budgets::tree_spend(&spends, &*self.tree.lock().await);
        for entry in &mut agents {
            entry["tree_spent_usd"] = json!(
                entry["identity"]
                    .as_str()
                    .and_then(|id| id.parse::<IdentityId>().ok())
                    .and_then(|id| totals.get(&id))
                    .copied()
                    .unwrap_or(Money::ZERO)
                    .usd()
            );
        }
        let mut answer = json!({"agents":agents});
        if let Some(message) = settings_error {
            answer["settings_error"] = json!(message);
        }
        Ok(answer)
    }

    pub(crate) async fn stop(self: &Arc<Self>, raw: Value) -> Result<Value, ApiError> {
        let tree::Stop { identity, subtree } = params(raw)?;
        let stopped = self
            .stop_tree(identity.clone(), subtree.unwrap_or(true))
            .await?;
        Ok(json!({"identity":identity,"state":"offline","stopped":stopped}))
    }

    pub(crate) async fn agent_budget(&self, raw: Value) -> Result<Value, ApiError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raise {
            identity: IdentityId,
            add_usd: serde_json::Number,
        }
        let Raise { identity, add_usd } = params(raw)?;
        let invalid =
            || bad_request("add_usd must be a positive decimal up to 1000, at least 0.000001 USD");
        if !add_usd
            .as_f64()
            .is_some_and(|value| value > 0.0 && value <= 1000.0)
        {
            return Err(invalid());
        }
        let add = Money::from_usd(&add_usd.to_string()).map_err(|_| invalid())?;
        if add == Money::from_usd("0").map_err(error)? {
            return Err(invalid());
        }
        let _launching = self.launching.lock().await;
        let _accounting = self.accounting.lock().await;
        let worker = self
            .workers
            .lock()
            .await
            .get(&identity)
            .cloned()
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
        let cap = Money::from_usd(&held.record.budget_usd)
            .map_err(error)?
            .checked_add(add)
            .map_err(error)?;
        let mut record = held.record.clone();
        record.budget_usd = cap.usd();
        drop(held);
        let parent = self.shared_parent(&record).await?;
        let reservation = match parent {
            Some(parent) => Some(self.reserve_from_parent(parent, add, Money::ZERO).await?),
            None => None,
        };
        if let Err(failure) = self.save_record(&record).await {
            if let Some(reservation) = reservation {
                reservation.rollback().await;
            }
            return Err(failure);
        }
        let mut worker = worker.lock().await;
        worker.record = record;
        worker.budget.raise_cap(cap);
        if reservation.is_some() {
            worker.reserved_from_parent = worker.reserved_from_parent.saturating_add(add);
        }
        Ok(json!({"identity":identity,"budget_usd":cap.usd()}))
    }

    async fn stop_worker(&self, worker: Arc<Mutex<Worker>>) -> Result<(), ApiError> {
        // An outer subtree deadline can cancel this future while it owns the
        // batch handle. Dropping a JoinHandle alone would detach the batch.
        struct AbortBatchOnDrop(tokio::task::AbortHandle);
        impl Drop for AbortBatchOnDrop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let (task, _abort_batch, identity, already_offline) = {
            let _accounting = self.accounting.lock().await;
            let mut worker = worker.lock().await;
            let already_offline = worker.state == "offline";
            worker.state = "offline";
            worker.batch_tier = None;
            // An attached Claude Code is the person's own session: it keeps running.
            worker.attached = None;
            if let Some(stop) = worker.stop.take() {
                let _sent = stop.send(());
            }
            // Return what this agent holds in its parent beyond the caps of its
            // own live children, which stay held. Counting by what it holds, not
            // by its state, keeps a second stop from returning them again.
            let released = worker
                .reserved_from_parent
                .saturating_sub(worker.budget.reserved());
            worker.reserved_from_parent = worker.reserved_from_parent.saturating_sub(released);
            let record = worker.record.clone();
            let task = worker.task.take();
            let abort_batch = task
                .as_ref()
                .map(|task| AbortBatchOnDrop(task.abort_handle()));
            drop(worker);
            self.release_to_parent(&record, released).await;
            (task, abort_batch, record.identity, already_offline)
        };
        if already_offline {
            return Ok(());
        }
        self.mark_stopped(&identity.identity_id).await;
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
        let deadline = tokio::time::Instant::now() + QUIT_WAIT;
        let launch_deadline = deadline.min(tokio::time::Instant::now() + self.quit_launch_wait);
        let _launching = match tokio::time::timeout_at(launch_deadline, self.launching.lock()).await
        {
            Ok(guard) => Some(guard),
            Err(_) => {
                tracing::warn!("managed shutdown is abandoning an in-flight launch");
                // The launch discards its identity, then frees the lock.
                self.abandon_launch.send_replace(true);
                tokio::time::timeout_at(deadline, self.launching.lock())
                    .await
                    .ok()
            }
        };
        let workers: Vec<_> = self
            .workers
            .lock()
            .await
            .values()
            .take(MAX_MANAGED)
            .cloned()
            .collect();
        let mut levels = BTreeMap::<u32, Vec<_>>::new();
        for worker in workers {
            let depth = worker.lock().await.record.depth;
            levels.entry(depth).or_default().push(worker);
        }
        // The registry bounds the number of levels and their workers by MAX_MANAGED.
        for (_, level) in levels.into_iter().rev() {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            let level_deadline = deadline.min(now + QUIT_LEVEL_WAIT);
            let mut tasks = tokio::task::JoinSet::new();
            for worker in level {
                let manager = self.clone();
                tasks.spawn(async move {
                    if let Err(error) = manager.stop_worker(worker).await {
                        tracing::warn!(code=?error.code,"managed agent goodbye failed");
                    }
                });
            }
            for _ in 0..MAX_MANAGED {
                match tokio::time::timeout_at(level_deadline, tasks.join_next()).await {
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => {
                        tasks.abort_all();
                        break;
                    }
                }
            }
            // Drain at most MAX_MANAGED cancelled stops before advancing to parents.
            tasks.shutdown().await;
        }
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
        let cap_budget = match self.safeguards().await {
            Ok(settings) => settings.budgets,
            Err(_) => self.safeguards_cache.lock().await.last_good.budgets,
        };
        for worker in workers {
            let mut held = worker.lock().await;
            match self.origins.person_for(&held.record.identity.hub_url).await {
                Ok(login) if actions::owned(&held.record.identity, &login).is_ok() => {}
                // A busy or stopped local record is retryable, not a logout.
                Err(error) if crate::local_origin::is_unavailable(&error) => continue,
                _ => {
                    drop(held);
                    if let Err(error) = self.stop_worker(worker).await {
                        tracing::warn!(code=?error.code,"stopping agent after login changed failed");
                    }
                    continue;
                }
            }
            if held.state == "attached" {
                self.watch_attached(&mut held).await;
                continue;
            }
            if held.state != "idle"
                || (cap_budget && held.budget.exhausted())
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
            let role = match self.batch_role(&held.record).await {
                Ok(role) => role,
                Err(failure) => {
                    held.role_error = Some(failure.message);
                    held.state = "offline";
                    continue;
                }
            };
            held.role_error = None;
            let rows = match identities.launch_pending(&held.record.identity).await {
                Ok(rows) if rows.is_empty() => continue,
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(code=?error.code,"managed pending read failed");
                    continue;
                }
            };
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
            let (session_id, resume) = match self.batch_session(&mut held, owner_only).await {
                Ok(session) => session,
                Err(failure) => {
                    tracing::error!(code = ?failure.code, "checking the owner session failed");
                    held.state = "offline";
                    continue;
                }
            };
            let mut spec = self.run_spec(
                &held,
                prompt,
                (session_id, resume),
                owner_only,
                role,
                cap_budget,
            );
            spec.local_token = batch_local_token(&identities, &spec.identity).await;
            held.last_run = Some(tokio::time::Instant::now());
            held.state = "running";
            held.batch_tier = Some(if owner_only {
                Tier::Owner
            } else {
                Tier::Others
            });
            let (stop, stopped) = oneshot::channel();
            held.stop = Some(stop);
            let worker = worker.clone();
            let managed = self.clone();
            held.task = Some(tokio::spawn(run_worker(managed, worker, spec, stopped)));
        }
    }
}

async fn batch_local_token(identities: &Identities, identity: &IdentityId) -> Option<LocalToken> {
    match identities.local_token(identity).await {
        Ok(Some(token)) => Some(token),
        Ok(None) => {
            tracing::warn!(%identity, "managed batch has no local token; agent cannot launch");
            None
        }
        Err(failure) => {
            tracing::warn!(%identity, code = ?failure.code, "reading managed batch local token failed; agent cannot launch");
            None
        }
    }
}

async fn run_worker(
    managed: Arc<Managed>,
    worker: Arc<Mutex<Worker>>,
    spec: RunSpec,
    stopped: oneshot::Receiver<()>,
) {
    let Some(identities) = managed.identities() else {
        return;
    };
    let owner_only = spec.owner_only;
    let cap_budget = spec.cap_budget;
    let result = managed.runner.run(spec, stopped).await;
    let accounting = managed.accounting.lock().await;
    let mut held = worker.lock().await;
    held.stop = None;
    held.batch_tier = None;
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
            // shortcut: conservatively add per-process totals; if resumed
            // totals include history this stops early. Revisit after a live
            // cost-format test establishes resume accounting semantics.
            let (record, steps) = match managed.record_cost(held, &result.total_cost_usd).await {
                Ok(charged) => charged,
                Err(error) => {
                    tracing::error!(%error,"invalid launcher cost");
                    return;
                }
            };
            let stopped = cap_budget && steps.contains(&Step::Stop);
            let warn = cap_budget && steps.contains(&Step::Warn);
            drop(accounting);
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
            let record = match managed.record_cost(held, &cost).await {
                Ok((record, _)) => record,
                Err(error) => {
                    tracing::error!(%error,"charging failed batch cost failed");
                    return;
                }
            };
            drop(accounting);
            let explanation = if error.reported_cost().is_none() {
                " Remaining budget is reserved because usage was not reported."
            } else {
                ""
            };
            if let Ok(message) = envelope(
                Payload::Fyi {},
                &format!("Claude Code batch stopped: {error}.{explanation}"),
            ) && let Err(error) = identities.post(&record.identity, message).await
            {
                tracing::warn!(code=?error.code,"batch failure report failed");
            }
        }
    }
}

/// The first role in `role`'s inheritance chain that comes from the project
/// layer under a built-in role's name. The project layer is whatever the
/// repository's committers wrote, so it may not replace a built-in.
fn shadowed_builtin<'a>(
    catalog: &'a agmen_roles::Catalog,
    builtin: &[(String, String)],
    role: &'a agmen_roles::Resolved,
) -> Option<&'a str> {
    let mut current = Some(role);
    // Role chains are short; the bound only guards a pathological one.
    for _ in 0..16 {
        let link = current?;
        if link.layer == agmen_roles::Layer::Project
            && builtin.iter().any(|(name, _)| *name == link.role.name)
        {
            return Some(&link.role.name);
        }
        current = link
            .role
            .extends
            .as_deref()
            .and_then(|parent| catalog.get(parent));
    }
    None
}

fn role_kind(role: &agmen_roles::Resolved) -> IdentityKind {
    match role.kind {
        agmen_roles::Kind::Coordinator => IdentityKind::Coordinator,
        _ => IdentityKind::Agent,
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

fn check_project(
    identity: &IdentityFile,
    project: &ProjectId,
    login: &agmen_client::Login,
) -> Result<(), ApiError> {
    if identity.project != *project
        || identity.owner_id != login.human_id
        || agmen_client::origin_key(&identity.hub_url) != agmen_client::origin_key(&login.hub_url)
    {
        return Err(bad_request(
            "checkout is not joined to this project and owner",
        ));
    }
    Ok(())
}

fn error(error: impl std::fmt::Display) -> ApiError {
    ApiError {
        code: ErrorCode::Hub,
        message: format!("managed agent: {error}"),
    }
}

/// A new agent's private folder with its record and its MCP configuration.
async fn write_agent_files(
    config_dir: PathBuf,
    record: Record,
    config: Value,
) -> Result<(), ApiError> {
    tokio::task::spawn_blocking(move || {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config_dir)?;
        private_json(
            &config_dir.join("agent.json"),
            &serde_json::to_value(&record)?,
        )?;
        private_json(&config_dir.join("mcp.json"), &config)
    })
    .await
    .map_err(error)?
    .map_err(error)
}

fn mcp_config(agmen: &Path, identity: &IdentityId, local_token: Option<&LocalToken>) -> Value {
    let mut env = json!({"AGMEN_IDENTITY": identity});
    if let Some(token) = local_token {
        env["AGMEN_LOCAL_TOKEN"] = json!(token.expose());
    }
    json!({"mcpServers":{"agmen":{"command":agmen,"args":["mcp"],"env":env}}})
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

#[path = "managed_recents.rs"]
mod recents;

#[path = "managed_tree.rs"]
mod tree;

#[path = "managed_budgets.rs"]
mod budgets;

#[path = "managed_roles.rs"]
mod roles;

#[cfg(test)]
#[path = "managed_tests.rs"]
mod tests;
