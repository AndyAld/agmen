//! Daemon-owned listener sessions, pending delivery and owner-authorized settings.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{
    ClientEnvelope, IdentityId, IdentityKind, MessageId, Payload, ProjectId, StampedEnvelope,
    Timestamp,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell, mpsc, oneshot};
use tokio::time::Instant;
use tracing::instrument::WithSubscriber as _;

use super::batch;
use super::git_read::{self, GitRead};
use super::policy::UsdCents;
use super::process::{ListenerArgs, ListenerProcess};
use super::queue::{Batch, Incoming, PostDecision, Queue};
use super::settings::{self, Applied, ChangeId, Changes, ListenerSettings, Reviews};
use crate::budget::{Budget, Money};
use crate::console::{bad_request, params};
use crate::git::Git;
use crate::hub::{Hub, HubError};
use crate::identities::{Identities, LocalToken, forbidden};
use crate::managed::Managed;
use crate::state::ManagedIdentity;

const MAX_LISTENERS: usize = 64;
const MAX_ROOTS: usize = 4096;
const RESTART_WINDOW: Duration = Duration::from_secs(3600);
const MAX_RESTARTS: usize = 3;
const IO_LIMIT: Duration = Duration::from_secs(10);

type Clock = Arc<dyn Fn() -> jiff::Zoned + Send + Sync>;
#[cfg(test)]
type CreationHook = Arc<dyn Fn(&Path) + Send + Sync>;

pub(crate) struct Listeners {
    hub: Arc<Hub>,
    identities: OnceCell<Weak<Identities>>,
    managed: OnceCell<Weak<Managed>>,
    state_dir: PathBuf,
    settings_file: PathBuf,
    program: PathBuf,
    helper: Option<PathBuf>,
    environment: Option<Vec<(OsString, OsString)>>,
    turn_timeout: Duration,
    tick: Duration,
    time_scale: u32,
    restart_delay: Duration,
    now: Clock,
    entries: Mutex<BTreeMap<ProjectId, Listener>>,
    changing: Mutex<()>,
    projects: Mutex<BTreeSet<ProjectId>>,
    changes: Arc<StdMutex<Changes>>,
    groups: Arc<StdMutex<BTreeSet<i32>>>,
    #[cfg(test)]
    created: Option<CreationHook>,
}

impl std::fmt::Debug for Listeners {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listeners").finish_non_exhaustive()
    }
}

struct Listener {
    live: Arc<Mutex<Live>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

struct Live {
    identity: ManagedIdentity,
    person: String,
    root: PathBuf,
    prompt: PathBuf,
    settings: ListenerSettings,
    state: &'static str,
    started_at: String,
    error: Option<String>,
    // shortcut: daily spend is memory-only; persist dated spend in state.rs
    // before caps must survive a daemon restart.
    day: jiff::civil::Date,
    spent: Money,
    queue: Queue,
    source: mpsc::Sender<Incoming>,
    roots: BTreeMap<MessageId, MessageId>,
    root_order: VecDeque<MessageId>,
    current: Option<(u64, Vec<StampedEnvelope>)>,
    cap_alert: Option<(ClientEnvelope, u8)>,
}

impl Live {
    fn reset_day(&mut self, now: &jiff::Zoned) {
        if self.day != now.date() {
            self.day = now.date();
            self.spent = zero();
            self.cap_alert = None;
            if self.state == "capped" {
                self.state = "running";
            }
        }
    }
    fn root_for(&self, id: &MessageId) -> MessageId {
        self.roots.get(id).unwrap_or(id).clone()
    }
    fn remember(&mut self, id: MessageId, root: MessageId) {
        if self.roots.contains_key(&id) {
            return;
        }
        if self.roots.len() == MAX_ROOTS
            && let Some(old) = self.root_order.pop_front()
        {
            self.roots.remove(&old);
        }
        self.root_order.push_back(id.clone());
        self.roots.insert(id, root);
    }
    fn remaining(&self) -> Result<Money, ApiError> {
        let mut budget = Budget::new(money(self.settings.listener_daily)?);
        budget
            .observe_total_cost_usd(&self.spent.usd(), |_| {})
            .map_err(failure)?;
        Money::from_usd(&budget.remaining_usd()).map_err(failure)
    }
    fn status(&self) -> Value {
        let mut out = json!({"project":self.identity.project,"identity_id":self.identity.identity_id,"handle":self.identity.handle,"state":self.state,"model":self.settings.model,"today_usd":usd(self.spent),"daily_usd":cents(self.settings.listener_daily),"started_at":self.started_at});
        if let Some(error) = &self.error {
            out["error"] = json!(error);
        }
        out
    }
}

#[derive(Clone)]
struct Launch {
    program: PathBuf,
    helper: PathBuf,
    env: Vec<(OsString, OsString)>,
    timeout: Duration,
}

struct Running {
    process: ListenerProcess,
    _config: McpFile,
    secret: LocalToken,
    model: String,
    day: jiff::civil::Date,
    cap: UsdCents,
    _group: ListenerGroup,
}

struct ListenerGroup {
    pid: i32,
    groups: Arc<StdMutex<BTreeSet<i32>>>,
}
impl Drop for ListenerGroup {
    fn drop(&mut self) {
        self.groups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.pid);
    }
}

// A failed start, a cancelled task and a normal stop all remove the config.
struct McpFile(PathBuf);
impl Drop for McpFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("removing listener MCP config failed");
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    project: ProjectId,
    root: PathBuf,
    model: Option<String>,
    #[serde(default)]
    coordinator: bool,
}

// Only "running, not stopped by the person". The model lives in the settings
// file, so a later `listener set --model` wins after a restart. An intent
// written with a `model` key by an earlier build still reads; the key is ignored.
#[derive(Deserialize, Serialize)]
struct Intent {
    project: ProjectId,
    root: PathBuf,
    identity_id: IdentityId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Project {
    project: ProjectId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    project: Option<ProjectId>,
}
#[derive(Deserialize)]
struct Set {
    project: ProjectId,
    #[serde(flatten)]
    changes: BTreeMap<String, Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decide {
    project: ProjectId,
    id: String,
    apply: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Undo {
    project: ProjectId,
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    target: String,
    key: String,
    value: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Apply {
    reply_to: MessageId,
    batch_id: u64,
    changes: Vec<Change>,
}

impl Listeners {
    pub(crate) fn new(hub: Arc<Hub>, state_dir: PathBuf, settings_file: PathBuf) -> Self {
        Self {
            hub,
            identities: OnceCell::new(),
            managed: OnceCell::new(),
            state_dir,
            settings_file,
            program: "claude".into(),
            helper: None,
            environment: None,
            turn_timeout: Duration::from_secs(180),
            tick: Duration::from_secs(1),
            time_scale: 1,
            restart_delay: Duration::from_secs(5),
            now: Arc::new(jiff::Zoned::now),
            entries: Mutex::new(BTreeMap::new()),
            changing: Mutex::new(()),
            projects: Mutex::new(BTreeSet::new()),
            changes: Arc::new(StdMutex::new(Changes::default())),
            groups: Arc::default(),
            #[cfg(test)]
            created: None,
        }
    }

    pub(crate) async fn attach(&self, identities: &Arc<Identities>, managed: &Arc<Managed>) {
        self.identities
            .get_or_init(|| async { Arc::downgrade(identities) })
            .await;
        self.managed
            .get_or_init(|| async { Arc::downgrade(managed) })
            .await;
    }
    fn identities(&self) -> Result<Arc<Identities>, ApiError> {
        self.identities
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| failure("identity service stopped"))
    }

    pub(crate) fn contains_peer(&self, pid: i32) -> bool {
        let groups = self
            .groups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if groups.is_empty() {
            return false;
        }
        rustix::process::Pid::from_raw(pid)
            .and_then(|pid| rustix::process::getpgid(Some(pid)).ok())
            .is_none_or(|group| groups.contains(&group.as_raw_nonzero().get()))
    }
    fn managed(&self) -> Result<Arc<Managed>, ApiError> {
        self.managed
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| failure("managed service stopped"))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "keep owner-authorized listener command dispatch together"
    )]
    pub(crate) async fn human_call(
        self: &Arc<Self>,
        method: &str,
        raw: Value,
    ) -> Result<Value, ApiError> {
        // Local API dispatch has already checked the owner's peer. Re-read the
        // login too so another owner's retained session cannot be controlled.
        let login = self
            .hub
            .credentials()
            .login()
            .await
            .map_err(HubError::from)?;
        let _changing = self.changing.lock().await;
        match method {
            "listener_start" => self.start(params(raw)?, &login).await,
            "listener_status" => {
                let Status { project } = params(raw)?;
                let entries = self.entries.lock().await;
                let mut rows = Vec::new();
                for (id, entry) in entries.iter() {
                    let mut live = entry.live.lock().await;
                    if owns(&live.identity, &login)
                        && project.as_ref().is_none_or(|project| project == id)
                    {
                        live.reset_day(&(self.now)());
                        rows.push(live.status());
                    }
                }
                Ok(json!(rows))
            }
            "listener_stop" => {
                let Project { project } = params(raw)?;
                let mut entries = self.entries.lock().await;
                if let Some(entry) = entries.get_mut(&project) {
                    if !owns(&entry.live.lock().await.identity, &login) {
                        return Err(forbidden());
                    }
                    stop(entry).await;
                    let mut live = entry.live.lock().await;
                    live.state = "stopped";
                    live.current = None;
                } else {
                    let path = self
                        .state_dir
                        .join("listener")
                        .join(project.as_str())
                        .join("intent.json");
                    let intent = tokio::task::spawn_blocking(move || read_intent(&path))
                        .await
                        .map_err(|_| failure("listener intent reader failed"))?;
                    match intent {
                        Ok(intent) => {
                            let registered = self
                                .identities()?
                                .managed(
                                    login.hub_url.origin().ascii_serialization(),
                                    login.human_id.clone(),
                                )
                                .await?;
                            if intent.project != project
                                || !registered.iter().any(|identity| {
                                    identity.kind == IdentityKind::Listener
                                        && identity.identity_id == intent.identity_id
                                        && identity.project == project
                                })
                            {
                                return Err(forbidden());
                            }
                        }
                        // A missing, corrupt or oversized intent names no owner, and
                        // readopt skips it, so stop may clear it below.
                        Err(_) => {}
                    }
                }
                remove_intent(
                    self.state_dir
                        .join("listener")
                        .join(project.as_str())
                        .join("intent.json"),
                )
                .await?;
                Ok(json!({}))
            }
            "listener_set" => {
                let Set { project, changes } = params(raw)?;
                self.track(&project).await?;
                let coordinators = self.coordinators(&project).await?;
                let path = self.settings_file.clone();
                let project_copy = project.clone();
                tokio::task::spawn_blocking(move || {
                    let mut after = settings::load(&path, &project_copy)?;
                    for (key, value) in changes {
                        update(&mut after, &key, value, &coordinators)?;
                    }
                    settings::save(&path, &project_copy, &after)
                })
                .await
                .map_err(failure)?
                .map_err(failure)?;
                self.refresh(&project).await?;
                Ok(json!({}))
            }
            "settings_decide" | "settings_undo" => {
                let (project, id, decision) = if method == "settings_decide" {
                    let Decide { project, id, apply } = params(raw)?;
                    (project, id, Some(apply))
                } else {
                    let Undo { project, id } = params(raw)?;
                    (project, id, None)
                };
                self.track(&project).await?;
                let path = self.settings_file.clone();
                let history = self.changes.clone();
                let project_copy = project.clone();
                tokio::task::spawn_blocking(move || {
                    let mut history = history
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match decision {
                        Some(apply) => history.decide(&path, &project_copy, &ChangeId(id), apply),
                        None => history.undo(&path, &project_copy, &ChangeId(id)),
                    }
                })
                .await
                .map_err(failure)?
                .map_err(failure)?;
                self.refresh(&project).await?;
                Ok(json!({}))
            }
            _ => Err(bad_request("unknown listener method")),
        }
    }

    async fn track(&self, project: &ProjectId) -> Result<(), ApiError> {
        let mut projects = self.projects.lock().await;
        if projects.len() == MAX_LISTENERS && !projects.contains(project) {
            return Err(bad_request("64 listener projects are already registered"));
        }
        projects.insert(project.clone());
        Ok(())
    }

    async fn load(&self, project: &ProjectId) -> Result<ListenerSettings, ApiError> {
        let (path, project) = (self.settings_file.clone(), project.clone());
        tokio::task::spawn_blocking(move || settings::load(&path, &project))
            .await
            .map_err(failure)?
            .map_err(failure)
    }

    async fn refresh(&self, project: &ProjectId) -> Result<(), ApiError> {
        let settings = self.load(project).await?;
        if let Some(entry) = self.entries.lock().await.get(project) {
            let mut live = entry.live.lock().await;
            live.reset_day(&(self.now)());
            if live.state == "capped" && money(settings.listener_daily)? > live.spent {
                live.state = "running";
                live.cap_alert = None;
                live.error = None;
            }
            live.settings = settings;
        }
        Ok(())
    }

    async fn coordinators(&self, project: &ProjectId) -> Result<Vec<String>, ApiError> {
        Ok(self
            .hub
            .roster(project)
            .await?
            .into_iter()
            .filter(|entry| entry.kind == IdentityKind::Coordinator)
            .map(|entry| entry.handle)
            .collect())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "keep validated start, registration and process handoff in one serialized operation"
    )]
    async fn start(
        self: &Arc<Self>,
        request: Start,
        login: &agmen_client::Login,
    ) -> Result<Value, ApiError> {
        if request.coordinator {
            return Err(bad_request("coordinator listeners are not supported yet"));
        }
        if let Some(model) = &request.model {
            text("model", &json!(model)).map_err(failure)?;
        }
        self.track(&request.project).await?;
        let mut entries = self.entries.lock().await;
        let mut spent = zero();
        if let Some(entry) = entries.get_mut(&request.project) {
            let mut live = entry.live.lock().await;
            if owns(&live.identity, login) {
                live.reset_day(&(self.now)());
                if matches!(live.state, "running" | "capped") {
                    return Err(already_running());
                }
                spent = live.spent;
            }
            drop(live);
            stop(entry).await;
        }
        let git = Git::new(&request.root, Instant::now() + IO_LIMIT);
        let inside = git
            .run_str(&["rev-parse", "--is-inside-work-tree"], 32)
            .await
            .map_err(|_| bad_request("listener root must be a git worktree"))?;
        if inside.stdout != b"true\n" {
            return Err(bad_request("listener root must be a git worktree"));
        }
        let role = self
            .managed()?
            .resolve_any_role(&request.root, "listener")
            .await?;
        if role.kind != agmen_roles::Kind::Listener {
            return Err(bad_request("the listener role must have kind listener"));
        }
        let mut settings = self.load(&request.project).await?;
        if let Some(model) = &request.model {
            settings.model.clone_from(model);
        }
        let launch = self.resolve().await?;
        let origin = login.hub_url.origin().ascii_serialization();
        let registered = self
            .identities()?
            .managed(origin, login.human_id.clone())
            .await?;
        let identity = if let Some(identity) = registered.into_iter().find(|identity| {
            identity.project == request.project && identity.kind == IdentityKind::Listener
        }) {
            identity
        } else {
            self.mint(&request.project, login, &role).await?
        };
        self.identities()?.register(identity.clone()).await?;
        let prompt = self
            .managed()?
            .write_role_prompt(&identity.identity_id, &role)
            .await?
            .prompt_file;
        let (source, queue) = Queue::new(identity.identity_id.clone(), identity.owner_id.clone());
        let live = Arc::new(Mutex::new(Live {
            identity: identity.clone(),
            person: login.handle.clone(),
            root: request.root.clone(),
            prompt,
            settings,
            state: "running",
            started_at: (self.now)().timestamp().to_string(),
            error: None,
            day: (self.now)().date(),
            spent,
            queue,
            source,
            roots: BTreeMap::new(),
            root_order: VecDeque::new(),
            current: None,
            cap_alert: None,
        }));
        let running = self.spawn(&live, &launch).await?;
        // The collector is an ingester. Its broadcast is never a batch source.
        self.identities()?.project_channel(&request.project).await?;
        write_intent(
            self.state_dir
                .join("listener")
                .join(request.project.as_str())
                .join("intent.json"),
            Intent {
                project: request.project.clone(),
                root: request.root,
                identity_id: identity.identity_id.clone(),
            },
        )
        .await?;
        let (stop, stopped) = oneshot::channel();
        let context = RunContext {
            hub: self.hub.clone(),
            identities: self.identities()?,
            groups: self.groups.clone(),
            live: live.clone(),
            launch,
            state_dir: self.state_dir.clone(),
            now: self.now.clone(),
            tick: self.tick,
            time_scale: self.time_scale,
            restart_delay: self.restart_delay,
            #[cfg(test)]
            created: self.created.clone(),
        };
        let task = tokio::spawn(run(context, running, stopped).with_current_subscriber());
        entries.insert(
            request.project,
            Listener {
                live,
                stop: Some(stop),
                task: Some(task),
            },
        );
        Ok(json!({"identity_id":identity.identity_id,"handle":identity.handle}))
    }

    async fn mint(
        &self,
        project: &ProjectId,
        login: &agmen_client::Login,
        role: &agmen_roles::Resolved,
    ) -> Result<ManagedIdentity, ApiError> {
        let broker = self.broker().await?;
        let handle = format!("{}/listener", login.handle);
        let spec = agmen_proto::RoleSpec {
            role: role.role.name.parse().map_err(failure)?,
            verbs: role.verbs.clone(),
            edits: role.edits.clone().into(),
            role_sha256: role.sha256.parse().map_err(failure)?,
        };
        let body = json!({"project":project,"handle":handle,"kind":"listener","role":spec.role,"verbs":spec.verbs,"edits":spec.edits,"role_sha256":spec.role_sha256});
        let minted = self
            .hub
            .mint_identity(&body)
            .await
            .map_err(|error| match error {
                HubError::Refused {
                    status: 409,
                    ref code,
                    ..
                } if code == "listener_exists" => already_running(),
                other => other.into(),
            })?;
        // This boundary never derives Debug or includes the response in an error.
        #[derive(Deserialize)]
        struct Minted {
            id: IdentityId,
            handle: String,
            project: ProjectId,
            tenant: agmen_proto::TenantId,
            token: String,
        }
        let minted: Minted = serde_json::from_value(minted)
            .map_err(|_| failure("invalid listener mint response"))?;
        if minted.handle != handle || minted.project != *project {
            return Err(failure("hub minted a different listener than requested"));
        }
        self.hub
            .credentials()
            .set_token(&login.hub_url, &minted.id, minted.token)
            .await?;
        Ok(ManagedIdentity {
            hub_url: login.hub_url.clone(),
            broker_url: broker,
            tenant: minted.tenant,
            project: minted.project,
            identity_id: minted.id,
            owner_id: login.human_id.clone(),
            handle: minted.handle,
            kind: IdentityKind::Listener,
        })
    }

    async fn broker(&self) -> Result<agmen_client::Broker, ApiError> {
        #[derive(Deserialize)]
        struct Me {
            broker_url: agmen_client::Broker,
        }
        let (login, credential) = self
            .hub
            .credentials()
            .access()
            .await
            .map_err(HubError::from)?;
        agmen_client::check_hub_url(&login.hub_url).map_err(failure)?;
        let mut url = login.hub_url;
        url.path_segments_mut()
            .map_err(|_| failure("hub URL has no path segments"))?
            .pop_if_empty()
            .extend(["v1", "me"]);
        let response = Hub::http()
            .map_err(HubError::Http)?
            .get(url)
            .bearer_auth(credential)
            .timeout(IO_LIMIT)
            .send()
            .await
            .map_err(HubError::Http)?
            .error_for_status()
            .map_err(HubError::Http)?;
        let bytes = agmen_client::read_hub_body(response)
            .await
            .map_err(HubError::Body)?;
        let me: Me =
            serde_json::from_slice(&bytes).map_err(|_| failure("invalid hub broker response"))?;
        Ok(me.broker_url)
    }

    async fn spawn(&self, live: &Arc<Mutex<Live>>, launch: &Launch) -> Result<Running, ApiError> {
        let identities = self.identities()?;
        spawn(
            &identities,
            &self.groups,
            &self.state_dir,
            live,
            launch,
            #[cfg(test)]
            self.created.as_ref(),
        )
        .await
    }

    async fn caller(&self, caller: &ManagedIdentity) -> Result<Arc<Mutex<Live>>, ApiError> {
        let login = self
            .hub
            .credentials()
            .login()
            .await
            .map_err(|_| forbidden())?;
        if !owns(caller, &login) || caller.kind != IdentityKind::Listener {
            return Err(forbidden());
        }
        let entries = self.entries.lock().await;
        let entry = entries.get(&caller.project).ok_or_else(forbidden)?;
        let live = entry.live.lock().await;
        if live.state != "running" || live.identity != *caller {
            return Err(forbidden());
        }
        Ok(entry.live.clone())
    }

    pub(crate) async fn post(
        &self,
        caller: &ManagedIdentity,
        envelope: ClientEnvelope,
    ) -> Result<Value, ApiError> {
        let live = self.caller(caller).await?;
        let mut live = live.lock().await;
        let parent = envelope.reply_to.as_ref().unwrap_or(&envelope.id);
        let root = live.root_for(parent);
        let decision = live.queue.try_post(&root);
        if decision != PostDecision::Allow {
            tracing::warn!(thread=%root,?decision,"listener post dropped");
            return Ok(
                json!({"dropped":if decision==PostDecision::ThreadCap {"thread_cap"} else {"capacity"}}),
            );
        }
        live.remember(envelope.id.clone(), root);
        drop(live);
        self.identities()?.post(caller, envelope).await
    }

    pub(crate) async fn identity_call(
        &self,
        caller: &ManagedIdentity,
        method: &str,
        raw: Value,
    ) -> Result<Value, ApiError> {
        let live = self.caller(caller).await?;
        match method {
            "settings_get" => {
                params::<Empty>(raw)?;
                Ok(settings_json(&self.load(&caller.project).await?))
            }
            "settings_apply" => self.apply(caller, live, params(raw)?).await,
            "roster" => {
                params::<Empty>(raw)?;
                serde_json::to_value(self.hub.roster(&caller.project).await?).map_err(failure)
            }
            "git_log" | "git_show" | "git_diff" | "git_status" => {
                let request = git_request(method, raw)?;
                let git = Git::new(live.lock().await.root.clone(), Instant::now() + IO_LIMIT);
                let answer = git_read::run(&git, &request).await.map_err(failure)?;
                Ok(json!({"text":answer.text,"truncated":answer.truncated}))
            }
            _ => Err(bad_request("unknown listener tool")),
        }
    }

    async fn apply(
        &self,
        caller: &ManagedIdentity,
        live: Arc<Mutex<Live>>,
        request: Apply,
    ) -> Result<Value, ApiError> {
        let _changing = self.changing.lock().await;
        let live_guard = live.lock().await;
        let cited = live_guard
            .current
            .as_ref()
            .filter(|(batch, _)| *batch == request.batch_id)
            .and_then(|(_, messages)| messages.iter().find(|m| m.client.id == request.reply_to));
        settings::check_request(
            cited.map_or("", |m| m.from.as_str()),
            caller.owner_id.as_str(),
            cited.is_some(),
        )
        .map_err(|code| ApiError {
            code,
            message: if code == ErrorCode::StaleRequest {
                "settings request is outside the current batch"
            } else {
                "only your person can request settings changes"
            }
            .into(),
        })?;
        let person = live_guard.person.clone();
        drop(live_guard);
        if request.changes.is_empty() || request.changes.len() > 32 {
            return Err(bad_request("settings changes must contain 1 to 32 entries"));
        }
        // shortcut: role and agent targets wait for L21-DR's target resolution.
        if request
            .changes
            .iter()
            .any(|change| change.target != "listener")
        {
            return Err(bad_request("only listener settings targets are supported"));
        }
        let coordinators = self.coordinators(&caller.project).await?;
        let path = self.settings_file.clone();
        let project = caller.project.clone();
        let history = self.changes.clone();
        let (applied, body) = tokio::task::spawn_blocking(move || {
            let before = settings::load(&path, &project)?;
            let mut after = before.clone();
            for change in &request.changes {
                update(&mut after, &change.key, change.value.clone(), &coordinators)?;
            }
            let before_json = settings_json(&before);
            let after_json = settings_json(&after);
            let lines: Vec<_> = request
                .changes
                .iter()
                .map(|change| {
                    format!(
                        "{}: {} -> {}",
                        change.key, before_json[&change.key], after_json[&change.key]
                    )
                })
                .collect();
            let refs: Vec<_> = coordinators.iter().map(String::as_str).collect();
            let mut history = history.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let applied = history.propose(&path, &project, &person, &refs, after)?;
            tracing::debug!(project=%project, audit_rows=history.audit(&project).count(), "listener settings proposal recorded");
            Ok::<_, settings::SettingsError>((applied, lines.join("\n")))
        })
        .await
        .map_err(failure)?
        .map_err(failure)?;
        let (state, verb, id) = match applied {
            Applied::Now { undo } => ("applied", "undo", undo.0),
            Applied::Pending { apply, .. } => ("pending", "apply", apply.0),
        };
        let mut card = envelope(Payload::Fyi {}, &format!("{body}\n{verb}:{id}"))?;
        card.reply_to = Some(request.reply_to);
        let posted = self.post(caller, card).await;
        self.refresh(&caller.project).await?;
        // The proposal already committed. A card delivery failure must neither
        // leave the old cap active nor hide the change id needed for undo/apply.
        if let Err(error) = posted {
            tracing::warn!(code=?error.code, change=%id, "listener settings recorded but change card could not be queued");
        }
        Ok(json!({"state":state,"id":id}))
    }

    pub(crate) async fn shutdown(&self) {
        let _changing = self.changing.lock().await;
        let mut entries = self.entries.lock().await;
        for entry in entries.values_mut() {
            stop(entry).await;
        }
    }

    pub(crate) async fn readopt(self: &Arc<Self>) {
        // Stop must not delete an intent between the startup scan and start.
        let _changing = self.changing.lock().await;
        let folder = self.state_dir.join("listener");
        let intents = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<Intent>> {
            let folders = match std::fs::read_dir(folder) {
                Ok(folders) => folders,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error),
            };
            let mut intents = Vec::new();
            for folder in folders.take(MAX_LISTENERS) {
                let read = || -> Result<Intent, Box<dyn std::error::Error>> {
                    let folder = folder?;
                    if !folder.file_type()?.is_dir() {
                        return Err("listener intent folder is not a directory".into());
                    }
                    let intent = read_intent(&folder.path().join("intent.json"))?;
                    if folder.file_name() != OsStr::new(intent.project.as_str()) {
                        return Err("listener intent project does not match its folder".into());
                    }
                    Ok(intent)
                };
                match read() {
                    Ok(intent) => intents.push(intent),
                    Err(_) => tracing::warn!("skipping unreadable listener intent"),
                }
            }
            Ok(intents)
        })
        .await;
        let Ok(Ok(intents)) = intents else {
            tracing::warn!("reading listener intents failed");
            return;
        };
        let Ok(login) = self.hub.credentials().login().await else {
            tracing::warn!("listener re-adoption needs a login");
            return;
        };
        let Ok(identities) = self.identities() else {
            return;
        };
        let Ok(registered) = identities
            .managed(
                login.hub_url.origin().ascii_serialization(),
                login.human_id.clone(),
            )
            .await
        else {
            tracing::warn!("reading registered listeners for re-adoption failed");
            return;
        };
        for intent in intents {
            if !registered.iter().any(|identity| {
                identity.kind == IdentityKind::Listener
                    && identity.identity_id == intent.identity_id
                    && identity.project == intent.project
            }) {
                continue;
            }
            if let Err(error) = self
                .start(
                    Start {
                        project: intent.project,
                        root: intent.root,
                        model: None,
                        coordinator: false,
                    },
                    &login,
                )
                .await
            {
                tracing::warn!(code=?error.code, "listener re-adoption failed");
            }
        }
    }

    async fn resolve(&self) -> Result<Launch, ApiError> {
        if let (Some(helper), Some(env)) = (&self.helper, &self.environment) {
            return Ok(Launch {
                program: self.program.clone(),
                helper: helper.clone(),
                env: env.clone(),
                timeout: self.turn_timeout,
            });
        }
        resolve(&self.program, self.turn_timeout).await
    }
}

async fn stop(entry: &mut Listener) {
    if let Some(stop) = entry.stop.take() {
        let _closed = stop.send(());
    }
    if let Some(mut task) = entry.task.take()
        && tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _joined = task.await;
    }
}

struct RunContext {
    hub: Arc<Hub>,
    identities: Arc<Identities>,
    groups: Arc<StdMutex<BTreeSet<i32>>>,
    live: Arc<Mutex<Live>>,
    launch: Launch,
    state_dir: PathBuf,
    now: Clock,
    tick: Duration,
    time_scale: u32,
    restart_delay: Duration,
    #[cfg(test)]
    created: Option<CreationHook>,
}

async fn run(context: RunContext, running: Running, mut stopped: oneshot::Receiver<()>) {
    let mut process = Some(running);
    let start = Instant::now();
    let mut retries = VecDeque::with_capacity(MAX_RESTARTS);
    let mut tick = tokio::time::interval(context.tick);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let stopping = async {
        tokio::select! {
            _ = &mut stopped => {},
            () = login_changed(&context) => {
                context.live.lock().await.state = "stopped";
            }
        }
    };
    tokio::pin!(stopping);
    // Each iteration waits for a tick or a bounded turn. The stop receiver is
    // also selected around the turn, so shutdown cannot wait for its timeout.
    loop {
        tokio::select! { () = &mut stopping => break, _ = tick.tick() => {} }
        let step = advance(&context, &mut process, start, &mut retries);
        let result = tokio::select! { () = &mut stopping => break, result=step => result };
        if let Err(error) = result {
            context.live.lock().await.error = Some(error.message);
            context.live.lock().await.state = "failed";
            break;
        }
    }
    if let Some(running) = process {
        running.process.stop().await;
    }
    context.live.lock().await.current = None;
}

async fn login_changed(context: &RunContext) {
    let identity = context.live.lock().await.identity.clone();
    let mut tick = tokio::time::interval(context.tick);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // This watcher shares the runtime's stop future, including during a turn.
    loop {
        tick.tick().await;
        let login = tokio::time::timeout(IO_LIMIT, context.hub.credentials().login()).await;
        if !login.is_ok_and(|login| login.is_ok_and(|login| owns(&identity, &login))) {
            return;
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "keep cap, process exit and turn transitions together"
)]
async fn advance(
    context: &RunContext,
    running: &mut Option<Running>,
    start: Instant,
    retries: &mut VecDeque<Instant>,
) -> Result<(), ApiError> {
    let mut live = context.live.lock().await;
    live.reset_day(&(context.now)());
    if live.state == "capped" {
        drop(live);
        flush_cap_alert(context).await;
        return Ok(());
    }
    let cap = money(live.settings.listener_daily)?;
    if live.spent >= cap {
        live.state = "capped";
        live.current = None;
        let cap = cents(live.settings.listener_daily);
        drop(live);
        if let Some(process) = running.take() {
            process.process.stop().await;
        }
        let body = format!("listener stopped: daily cap {cap} reached");
        let alert = envelope(
            Payload::Alert {
                need: body.clone(),
                urgency: agmen_proto::Urgency::Normal,
                needs: agmen_proto::Needs::Owner,
                actions: Vec::new(),
            },
            &body,
        )?;
        context.live.lock().await.cap_alert = Some((alert, 0));
        flush_cap_alert(context).await;
        return Ok(());
    }
    // Claude's budget is a process lifetime limit. Renew it after midnight
    // or a cap change, using the day's remaining spend on the new process.
    let launch_changed = running.as_ref().is_some_and(|p| {
        p.model != live.settings.model || p.day != live.day || p.cap != live.settings.listener_daily
    });
    if launch_changed {
        live.current = None;
    }
    drop(live);
    if launch_changed && let Some(old) = running.take() {
        old.process.stop().await;
    }
    if running.is_none() {
        *running = Some(
            spawn(
                &context.identities,
                &context.groups,
                &context.state_dir,
                &context.live,
                &context.launch,
                #[cfg(test)]
                context.created.as_ref(),
            )
            .await?,
        );
    }
    let Some(process) = running.as_mut() else {
        return Err(failure("listener process missing"));
    };
    let exited = process
        .process
        .child
        .try_wait()
        .map_err(|_| failure("checking listener process failed"))?
        .is_some();
    if exited {
        let error = process.process.next_turn().await.err().map_or_else(
            || "listener exited between turns".into(),
            |error| error.to_string(),
        );
        let error = error.replace(process.secret.expose(), "<redacted>");
        return restart(context, running, retries, error).await;
    }
    drain(context).await?;
    let mut live = context.live.lock().await;
    // shortcut: L21-DR supplies workers to queue polling and routing.
    let batch = live
        .queue
        .poll(start.elapsed().saturating_mul(context.time_scale), &[]);
    let Some(mut batch) = batch else {
        return Ok(());
    };
    let context_json = json!({"project":live.identity.project,"person":live.person,"listener":live.identity.handle});
    let (encoded, accepted) = match encode_prefix(&batch, &context_json) {
        Ok(encoded) => encoded,
        Err(error) => {
            live.queue.pending = batch.messages;
            return Err(failure(error));
        }
    };
    let remaining = batch.messages.split_off(accepted);
    debug_assert!(
        live.queue.pending.is_empty(),
        "poll leaves the pending buffer empty"
    );
    live.queue.pending = remaining;
    live.current = Some((batch.batch_id.get(), batch.messages));
    drop(live);
    let Some(process) = running.as_mut() else {
        return Err(failure("listener process missing"));
    };
    let turn = async {
        process.process.send(&encoded).await?;
        process.process.next_turn().await
    }
    .await;
    let failure_text = match turn {
        Ok(turn) => {
            let mut live = context.live.lock().await;
            live.reset_day(&(context.now)());
            live.spent = live.spent.checked_add(turn.cost).map_err(failure)?;
            // MCP answers tools while the turn runs. Retaining raw results or
            // tool inputs here would keep unnecessary untrusted message data.
            tracing::debug!(
                tools = turn.tool_calls.len(),
                is_error = turn.is_error,
                "listener turn finished"
            );
            if turn.is_error && live.spent < money(live.settings.listener_daily)? {
                Some(format!("listener turn failed: {}", turn.result))
            } else {
                None
            }
        }
        Err(error) => {
            // shortcut: a turn that crashes before its result adds no cost, so the daily cap can be overshot by at most 4 uncounted crashed turns an hour (MAX_RESTARTS restarts, then the crash that ends in failed); count partial cost when the CLI reports it on a crash.
            alert_lost_batch(context).await;
            Some(error.to_string())
        }
    };
    if let Some(error) = failure_text {
        let error = error.replace(process.secret.expose(), "<redacted>");
        restart(context, running, retries, error).await?;
    }
    Ok(())
}

async fn alert_lost_batch(context: &RunContext) {
    let mut live = context.live.lock().await;
    let Some((_, messages)) = live.current.take() else {
        return;
    };
    let mut body =
        String::from("listener restarted after a crash; these messages were not handled: ");
    let ids: Vec<_> = messages
        .iter()
        .take(32)
        .map(|message| message.client.id.as_str())
        .collect();
    body.push_str(&ids.join(", "));
    if messages.len() > 32 {
        body.push_str(&format!(" and {} more", messages.len() - 32));
    }
    let identity = live.identity.clone();
    drop(live);
    let alert = envelope(
        Payload::Alert {
            need: body.clone(),
            urgency: agmen_proto::Urgency::Normal,
            needs: agmen_proto::Needs::Owner,
            actions: Vec::new(),
        },
        &body,
    );
    let result = match alert {
        Ok(alert) => context.identities.post(&identity, alert).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        tracing::warn!(code=?error.code, "lost listener batch ALERT could not be queued");
    }
}

async fn flush_cap_alert(context: &RunContext) {
    let mut live = context.live.lock().await;
    let identity = live.identity.clone();
    let Some((alert, attempts)) = &mut live.cap_alert else {
        return;
    };
    if *attempts == 3 {
        return;
    }
    *attempts += 1;
    // Retry the same envelope id at most three times; hub/outbox dedup makes
    // an ambiguous enqueue outcome safe without publishing another ALERT.
    let alert = alert.clone();
    drop(live);
    match context.identities.post(&identity, alert.clone()).await {
        Ok(_) => {
            let mut live = context.live.lock().await;
            if live
                .cap_alert
                .as_ref()
                .is_some_and(|(pending, _)| pending.id == alert.id)
            {
                live.cap_alert = None;
            }
        }
        Err(error) => {
            tracing::warn!(code=?error.code, "listener capped but ALERT could not be queued")
        }
    }
}

/// Overflow returns to Queue.pending, preserving its interval and backpressure.
fn encode_prefix(batch: &Batch, context: &Value) -> Result<(Vec<u8>, usize), batch::BatchError> {
    match batch::encode(batch, context) {
        Ok(bytes) => return Ok((bytes, batch.messages.len())),
        Err(batch::BatchError::TooLarge) => {}
        Err(error) => return Err(error),
    }
    let (mut low, mut high) = (0, batch.messages.len());
    // Queue batches contain at most 256 events, requiring at most nine probes.
    for _ in 0..9 {
        if low == high {
            break;
        }
        let mid = (low + high).div_ceil(2);
        let probe = Batch {
            batch_id: batch.batch_id,
            messages: batch.messages[..mid].to_vec(),
        };
        match batch::encode(&probe, context) {
            Ok(_) => low = mid,
            Err(batch::BatchError::TooLarge) => high = mid - 1,
            Err(error) => return Err(error),
        }
    }
    if low == 0 {
        return Err(batch::BatchError::TooLarge);
    }
    let prefix = Batch {
        batch_id: batch.batch_id,
        messages: batch.messages[..low].to_vec(),
    };
    Ok((batch::encode(&prefix, context)?, low))
}

async fn restart(
    context: &RunContext,
    running: &mut Option<Running>,
    retries: &mut VecDeque<Instant>,
    mut error: String,
) -> Result<(), ApiError> {
    error.truncate(error.floor_char_boundary(8 * 1024));
    if let Some(process) = running.take() {
        process.process.stop().await;
    }
    let now = Instant::now();
    retries.retain(|at| now.duration_since(*at) < RESTART_WINDOW);
    context.live.lock().await.current = None;
    context.live.lock().await.error = Some(error.clone());
    if retries.len() == MAX_RESTARTS {
        return Err(failure(error));
    }
    retries.push_back(now);
    tokio::time::sleep(context.restart_delay).await;
    *running = Some(
        spawn(
            &context.identities,
            &context.groups,
            &context.state_dir,
            &context.live,
            &context.launch,
            #[cfg(test)]
            context.created.as_ref(),
        )
        .await?,
    );
    Ok(())
}

async fn drain(context: &RunContext) -> Result<(), ApiError> {
    // shortcut: drained rows leave the store before the batch runs, so after a daemon stop agmend cannot list the messages that were in the channel or Queue.pending; keep drained ids in the store until their turn ends if the lost-batch ALERT must cover a restart.
    // Reserving before the transaction leaves pending rows in the store when
    // the channel is full. Each tick drains at most its 256 channel slots.
    for _ in 0..256 {
        let live = context.live.lock().await;
        let Ok(permit) = live.source.clone().try_reserve_owned() else {
            break;
        };
        let identity = live.identity.clone();
        drop(live);
        let rows = context
            .identities
            .listener_pending(&identity, 1, agmen_local::MAX_FRAME as usize - 4096)
            .await?;
        let Some(message) = rows.into_iter().next() else {
            break;
        };
        let mut live = context.live.lock().await;
        let root = live.root_for(
            message
                .client
                .reply_to
                .as_ref()
                .unwrap_or(&message.client.id),
        );
        live.remember(message.client.id.clone(), root.clone());
        let accepted = message.from == identity.owner_id
            || message.from_kind == IdentityKind::Coordinator
            || live
                .settings
                .accept_from
                .iter()
                .any(|handle| handle == &message.from_handle);
        if accepted {
            permit.send(Incoming {
                message,
                thread: root,
            });
        }
    }
    Ok(())
}

async fn spawn(
    identities: &Identities,
    groups: &Arc<StdMutex<BTreeSet<i32>>>,
    state_dir: &Path,
    live: &Arc<Mutex<Live>>,
    launch: &Launch,
    #[cfg(test)] created: Option<&CreationHook>,
) -> Result<Running, ApiError> {
    let live = live.lock().await;
    let secret = identities
        .local_token(&live.identity.identity_id)
        .await?
        .ok_or_else(forbidden)?;
    let path = state_dir
        .join("listener")
        .join(live.identity.project.as_str())
        .join("mcp.json");
    let config = write_config(
        path,
        &launch.helper,
        &live.identity.identity_id,
        &secret,
        #[cfg(test)]
        created.cloned(),
    )
    .await?;
    let args = ListenerArgs {
        program: launch.program.clone(),
        model: live.settings.model.clone(),
        mcp_config: config.0.clone(),
        prompt_file: live.prompt.clone(),
        max_budget_usd: live.remaining()?,
    };
    let process = ListenerProcess::spawn(&args, &live.root, &launch.env, launch.timeout)
        .await
        .map_err(|_| failure("spawning listener process failed"))?;
    let pid = process
        .child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or_else(|| failure("listener PID missing"))?;
    groups
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(pid);
    Ok(Running {
        process,
        _config: config,
        secret,
        model: live.settings.model.clone(),
        day: live.day,
        cap: live.settings.listener_daily,
        _group: ListenerGroup {
            pid,
            groups: groups.clone(),
        },
    })
}

async fn write_config(
    path: PathBuf,
    helper: &Path,
    identity: &IdentityId,
    secret: &LocalToken,
    #[cfg(test)] created: Option<CreationHook>,
) -> Result<McpFile, ApiError> {
    let value = json!({"mcpServers":{"agmen":{"command":helper,"args":["mcp","--listener"],"env":{"AGMEN_IDENTITY":identity,"AGMEN_LOCAL_TOKEN":secret.expose()}}}});
    tokio::task::spawn_blocking(move || -> std::io::Result<McpFile> {
        let dir = path
            .parent()
            .ok_or_else(|| std::io::Error::other("listener config has no folder"))?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        for folder in [Some(dir), dir.parent()].into_iter().flatten() {
            let metadata = std::fs::symlink_metadata(folder)?;
            if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(std::io::Error::other(
                    "listener config folder is not private",
                ));
            }
        }
        let temp = dir.join(format!(".mcp-{}", crate::launch::new_session_id()?));
        let cleanup = McpFile(temp.clone());
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        #[cfg(test)]
        if let Some(created) = created {
            created(&temp);
        }
        serde_json::to_writer(&mut file, &value)?;
        file.flush()?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)?;
        drop(cleanup);
        Ok(McpFile(path))
    })
    .await
    .map_err(|_| failure("listener MCP config worker failed"))?
    .map_err(|_| failure("writing private listener MCP config failed"))
}

fn read_intent(path: &Path) -> std::io::Result<Intent> {
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(std::io::Error::other(
            "listener intent is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(std::io::Error::other("listener intent is too large"));
    }
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

// Removes a regular file only. It never follows a symlink, and it refuses a
// directory or symlink, because the person (not agmend) put that there.
async fn remove_intent(path: PathBuf) -> Result<(), ApiError> {
    tokio::task::spawn_blocking(move || match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => std::fs::remove_file(&path)
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(|_| "removing listener intent failed"),
        Ok(_) => Err("listener intent is not a regular file; remove it by hand"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("reading listener intent failed"),
    })
    .await
    .map_err(|_| failure("listener intent worker failed"))?
    .map_err(failure)
}

async fn write_intent(path: PathBuf, intent: Intent) -> Result<(), ApiError> {
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let dir = path
            .parent()
            .ok_or_else(|| std::io::Error::other("listener intent has no folder"))?;
        let metadata = std::fs::symlink_metadata(dir)?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(std::io::Error::other(
                "listener intent folder is not private",
            ));
        }
        let temp = dir.join(format!(".intent-{}", crate::launch::new_session_id()?));
        let cleanup = McpFile(temp.clone());
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer(&mut file, &intent)?;
        file.flush()?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)?;
        drop(cleanup);
        Ok(())
    })
    .await
    .map_err(|_| failure("listener intent worker failed"))?
    .map_err(|_| failure("writing private listener intent failed"))
}

fn git_request(method: &str, raw: Value) -> Result<GitRead, ApiError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Log {
        rev_range: Option<String>,
        n: Option<u32>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Show {
        rev: String,
        #[serde(default)]
        stat: bool,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Diff {
        base: String,
        head: String,
    }
    match method {
        "git_log" => {
            let Log { rev_range, n } = params(raw)?;
            Ok(GitRead::Log { rev_range, n })
        }
        "git_show" => {
            let Show { rev, stat } = params(raw)?;
            Ok(GitRead::Show { rev, stat })
        }
        "git_diff" => {
            let Diff { base, head } = params(raw)?;
            Ok(GitRead::Diff { base, head })
        }
        _ => {
            params::<Empty>(raw)?;
            Ok(GitRead::Status)
        }
    }
}

fn update(
    settings: &mut ListenerSettings,
    key: &str,
    value: Value,
    coordinators: &[String],
) -> Result<(), settings::SettingsError> {
    use settings::SettingsError;
    let bad = || SettingsError::BadValue {
        key: key.into(),
        message: "invalid value",
    };
    match key {
        "model" => settings.model = text(key, &value)?,
        "effort" => {
            settings.effort = if value.is_null() {
                None
            } else {
                Some(text(key, &value)?)
            }
        }
        "per_delegation_usd" | "daily_usd" | "listener_daily_usd" => {
            let amount =
                settings::parse_money(key, &toml_edit::value(value.as_str().ok_or_else(bad)?))?;
            match key {
                "per_delegation_usd" => settings.per_delegation = amount,
                "daily_usd" => settings.daily = amount,
                _ => settings.listener_daily = amount,
            }
        }
        "max_workers" => {
            settings.max_workers = value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(bad)?
        }
        "reviews" => {
            settings.reviews = match value.as_str() {
                Some("on") => Reviews::On,
                Some("off") => Reviews::Off,
                _ => return Err(bad()),
            }
        }
        "worker_roles" => {
            let values = value
                .as_array()
                .filter(|values| values.len() <= 32)
                .ok_or_else(bad)?;
            settings.worker_roles = values
                .iter()
                .map(|v| text(key, v))
                .collect::<Result<_, _>>()?;
        }
        "accept_from" => {
            let deltas: Vec<Value> = if value.is_string() {
                vec![value]
            } else {
                value
                    .as_array()
                    .filter(|values| values.len() <= 64)
                    .ok_or_else(bad)?
                    .clone()
            };
            for delta in deltas {
                let delta = delta.as_str().ok_or_else(bad)?;
                let (add, handle) = if let Some(handle) = delta.strip_prefix('+') {
                    (true, handle)
                } else if let Some(handle) = delta.strip_prefix('-') {
                    (false, handle)
                } else {
                    return Err(bad());
                };
                text(key, &json!(handle))?;
                if add {
                    if !settings.accept_from.iter().any(|s| s == handle) {
                        settings.accept_from.push(handle.into());
                    }
                } else {
                    if coordinators.iter().any(|s| s == handle) {
                        return Err(SettingsError::CoordinatorRemoval);
                    }
                    settings.accept_from.retain(|s| s != handle);
                }
            }
        }
        _ => {
            return Err(SettingsError::BadValue {
                key: key.into(),
                message: "unsupported listener setting",
            });
        }
    }
    Ok(())
}

fn text(key: &str, value: &Value) -> Result<String, settings::SettingsError> {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 256
                && !s.starts_with('-')
                && !s.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| settings::SettingsError::BadValue {
            key: key.into(),
            message: "expected 1 to 256 characters without controls or a leading dash",
        })
}

fn settings_json(settings: &ListenerSettings) -> Value {
    json!({"model":settings.model,"effort":settings.effort,"per_delegation_usd":cents(settings.per_delegation),"daily_usd":cents(settings.daily),"max_workers":settings.max_workers,"listener_daily_usd":cents(settings.listener_daily),"reviews":if settings.reviews==Reviews::On {"on"} else {"off"},"accept_from":settings.accept_from,"worker_roles":settings.worker_roles})
}
fn cents(amount: UsdCents) -> String {
    format!("{}.{:02}", amount.0 / 100, amount.0 % 100)
}
fn usd(amount: Money) -> String {
    let s = amount.usd();
    s[..s.len() - 4].into()
}
fn money(amount: UsdCents) -> Result<Money, ApiError> {
    Money::from_usd(&cents(amount)).map_err(failure)
}
fn zero() -> Money {
    Money::from_usd("0").unwrap_or_else(|_| std::process::abort())
}
fn owns(identity: &ManagedIdentity, login: &agmen_client::Login) -> bool {
    identity.owner_id == login.human_id && identity.hub_url.origin() == login.hub_url.origin()
}
fn failure(error: impl std::fmt::Display) -> ApiError {
    bad_request(error.to_string())
}
fn already_running() -> ApiError {
    bad_request("a listener already runs in this project")
}

fn envelope(payload: Payload, body: &str) -> Result<ClientEnvelope, ApiError> {
    let random = crate::launch::new_session_id()
        .map_err(failure)?
        .replace('-', "");
    let timestamp = Timestamp::now();
    let ms = u128::try_from(timestamp.as_millisecond()).map_err(failure)? & ((1 << 48) - 1);
    let bits = u128::from_str_radix(&random[..20], 16).map_err(failure)?;
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
            .map_err(failure)?
            .parse()
            .map_err(failure)?,
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: body.to_owned().try_into().map_err(failure)?,
        valid_until: None,
        sent_at: timestamp,
        payload,
    })
}

// launch.rs keeps these helpers private. Use its login-shell PATH and sibling
// helper search here too, without changing managed workers' launch behavior.
async fn resolve(program: &Path, turn_timeout: Duration) -> Result<Launch, ApiError> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt as _;
    let shell = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| "/bin/zsh".into());
    let mut child = tokio::process::Command::new(shell)
        .args([
            "-lic",
            "printf '\\n__AGMEN_LISTENER_PATH__\\n%s\\n' \"$PATH\"",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| failure("starting login PATH lookup failed"))?;
    let pid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| failure("login shell PID missing"))?;
    let group = LoginGroup(pid);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| failure("login PATH stdout missing"))?;
    let bytes = tokio::time::timeout(IO_LIMIT, async {
        let mut bytes = Vec::new();
        stdout
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(failure)?;
        if bytes.len() > 64 * 1024 || !child.wait().await.map_err(failure)?.success() {
            return Err(failure("login PATH lookup failed"));
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| failure("login PATH lookup timed out"))??;
    drop(group);
    let output = String::from_utf8(bytes).map_err(|_| failure("login PATH is not UTF-8"))?;
    let lines: Vec<_> = output.lines().collect();
    let at = lines
        .iter()
        .rposition(|line| *line == "__AGMEN_LISTENER_PATH__")
        .ok_or_else(|| failure("login PATH marker missing"))?;
    let path = OsString::from(
        *lines
            .get(at + 1)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| failure("login PATH missing"))?,
    );
    let program = on_path(program, &path)
        .ok_or_else(|| failure("Claude Code is not installed or not on your login PATH"))?;
    let exe = std::env::current_exe().map_err(failure)?;
    let helper = exe
        .parent()
        .map(|dir| dir.join("agmen"))
        .filter(|p| executable(p))
        .or_else(|| on_path(Path::new("agmen"), &path))
        .ok_or_else(|| failure("agmen executable was not found"))?;
    let mut env: Vec<_> = ["HOME", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "USER"]
        .into_iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (name.into(), value)))
        .collect();
    env.extend([("PATH".into(), path), ("AGMEN_MANAGED".into(), "1".into())]);
    Ok(Launch {
        program,
        helper,
        env,
        timeout: turn_timeout,
    })
}
struct LoginGroup(rustix::process::Pid);
impl Drop for LoginGroup {
    fn drop(&mut self) {
        let _gone = rustix::process::kill_process_group(self.0, rustix::process::Signal::KILL);
    }
}
fn executable(path: &Path) -> bool {
    path.is_absolute()
        && path
            .metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
fn on_path(program: &Path, path: &OsStr) -> Option<PathBuf> {
    if program.is_absolute() {
        return executable(program).then(|| program.to_owned());
    }
    std::env::split_paths(path)
        .take(4096)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|p| executable(p))
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
