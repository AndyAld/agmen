//! Per-request local caller authentication and identity session publishing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agmen_client::{ClientEvent, ClientKind, HubClient, Me, OutboxState, Session, SessionConfig};
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{ClientEnvelope, IdentityId, ProjectId, StampedEnvelope, Stream, Topics};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Mutex as AsyncMutex, broadcast, oneshot};
use url::Url;

use crate::console::{bad_request, params};
use crate::hub::Hub;
use crate::login::Credentials;
use crate::state::{ManagedIdentity, Store};

const DEADLINE: Duration = Duration::from_secs(10);
const POST_WAIT: Duration = Duration::from_secs(5);

type RuntimeKey = (String, IdentityId);
type RuntimeSlot = Arc<AsyncMutex<Option<Runtime>>>;
type ProjectKey = (String, IdentityId, ProjectId);
type WatchEvent = (Option<IdentityId>, Result<ClientEvent, ApiError>);

#[derive(Debug)]
struct Database {
    store: Arc<Mutex<Store>>,
    jobs: AsyncMutex<tokio::task::JoinSet<()>>,
}

struct Cancellation(Arc<AtomicBool>);

impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Debug)]
struct Collector {
    tx: broadcast::Sender<WatchEvent>,
    task: tokio::task::JoinHandle<()>,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectParams {
    project: ProjectId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxParams {
    project: ProjectId,
    #[serde(default)]
    after: u64,
    #[serde(default = "page_limit")]
    limit: u32,
}

fn page_limit() -> u32 {
    500
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateParams {
    project: ProjectId,
    key: String,
}

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "B T2 specifies a caller owning its managed metadata"
)]
pub(crate) enum Caller {
    Human,
    Identity(ManagedIdentity),
}

// This record deliberately has no Debug implementation: it holds a credential.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalCredential {
    origin: String,
    owner_id: IdentityId,
    local_token: String,
}

#[derive(Debug)]
struct Runtime {
    session: Weak<Session>,
    task: tokio::task::JoinHandle<()>,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
type Publisher = Arc<
    dyn Fn(
            ManagedIdentity,
            ClientEnvelope,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<OutboxState, ApiError>> + Send>,
        > + Send
        + Sync,
>;

pub(crate) struct Identities {
    credentials: Arc<Credentials>,
    store: Arc<Database>,
    state_dir: PathBuf,
    hub: Arc<Hub>,
    runtimes: AsyncMutex<BTreeMap<RuntimeKey, RuntimeSlot>>,
    collectors: AsyncMutex<BTreeMap<ProjectKey, Collector>>,
    launched: AsyncMutex<BTreeSet<RuntimeKey>>,
    #[cfg(test)]
    pub(crate) publisher: Option<Publisher>,
}

impl std::fmt::Debug for Identities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identities").finish_non_exhaustive()
    }
}

impl Identities {
    pub(crate) async fn register(&self, identity: ManagedIdentity) -> Result<(), ApiError> {
        let key = (
            identity.hub_url.origin().ascii_serialization(),
            identity.identity_id.clone(),
        );
        store_work(self.store.clone(), move |store| store.register(&identity)).await?;
        self.launched.lock().await.insert(key);
        Ok(())
    }

    pub(crate) async fn close_identity(&self, identity: &ManagedIdentity) {
        let key = (
            identity.hub_url.origin().ascii_serialization(),
            identity.identity_id.clone(),
        );
        if let Some(slot) = self.runtimes.lock().await.remove(&key) {
            stop_runtimes([slot], tokio::time::Instant::now() + DEADLINE).await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn enqueue_test(&self, message: StampedEnvelope) {
        let login = self.credentials.login().await.unwrap();
        let origin = login.hub_url.origin().ascii_serialization();
        store_work(self.store.clone(), move |s| {
            s.ingest_for_owner(
                &origin,
                &"project1".parse().unwrap(),
                &message,
                Some(&login.human_id),
            )
        })
        .await
        .unwrap();
    }
    pub(crate) fn new(
        credentials: Arc<Credentials>,
        store: Arc<Mutex<Store>>,
        state_dir: PathBuf,
        hub: Arc<Hub>,
    ) -> Self {
        Self {
            credentials,
            store: Arc::new(Database {
                store,
                jobs: AsyncMutex::new(tokio::task::JoinSet::new()),
            }),
            state_dir,
            hub,
            runtimes: AsyncMutex::new(BTreeMap::new()),
            collectors: AsyncMutex::new(BTreeMap::new()),
            launched: AsyncMutex::new(BTreeSet::new()),
            #[cfg(test)]
            publisher: None,
        }
    }

    pub(crate) async fn authorize(
        &self,
        identity: Option<&IdentityId>,
        token: Option<&str>,
    ) -> Result<Caller, ApiError> {
        let Some(id) = identity else {
            return if token.is_none() {
                Ok(Caller::Human)
            } else {
                Err(unauthorized())
            };
        };
        let supplied = token.ok_or_else(unauthorized)?;
        if !valid_token(supplied) {
            return Err(unauthorized());
        }
        tokio::time::timeout(DEADLINE, async {
            let login = self.credentials.login().await.map_err(|_| unauthorized())?;
            let origin = login.hub_url.origin().ascii_serialization();
            let namespace = Url::parse("https://agmend.invalid").map_err(|_| unauthorized())?;
            let raw = self
                .credentials
                .token(&namespace, id)
                .await?
                .ok_or_else(unauthorized)?;
            let credential: LocalCredential =
                serde_json::from_str(&raw).map_err(|_| unauthorized())?;
            if credential.origin != origin
                || credential.owner_id != login.human_id
                || !tokens_match(&credential.local_token, supplied)
            {
                return Err(unauthorized());
            }
            let rows = self.managed(origin.clone(), login.human_id.clone()).await?;
            let caller = rows
                .into_iter()
                .find(|row| {
                    row.identity_id == *id
                        && row.owner_id == login.human_id
                        && row.hub_url.origin().ascii_serialization() == origin
                })
                .ok_or_else(unauthorized)?;
            Ok(Caller::Identity(caller))
        })
        .await
        .map_err(|_| unauthorized())?
    }

    async fn managed(
        &self,
        origin: String,
        owner: IdentityId,
    ) -> Result<Vec<ManagedIdentity>, ApiError> {
        let store = self.store.clone();
        store_work(store, move |s| s.identities(&origin, &owner)).await
    }

    fn project(caller: &Caller, raw: &Value) -> Result<(), ApiError> {
        if let Caller::Identity(identity) = caller
            && raw
                .get("project")
                .and_then(Value::as_str)
                .is_some_and(|p| p != identity.project.as_str())
        {
            return Err(forbidden());
        }
        Ok(())
    }

    pub(crate) async fn inbox(&self, caller: &Caller, raw: Value) -> Result<Value, ApiError> {
        Self::project(caller, &raw)?;
        let InboxParams {
            project,
            after,
            limit,
        } = params(raw)?;
        if !(1..=500).contains(&limit) {
            return Err(bad_request("limit must be 1 to 500"));
        }
        let identity = match caller {
            Caller::Human => {
                self.credentials
                    .login()
                    .await
                    .map_err(crate::hub::HubError::from)?
                    .human_id
            }
            Caller::Identity(identity) => identity.identity_id.clone(),
        };
        let page =
            tokio::time::timeout(DEADLINE, self.hub.inbox(&project, &identity, after, limit))
                .await
                .map_err(|_| hub_error("inbox read timed out"))??;
        Ok(json!({"messages":page.messages,"next_after":page.next_after,"head":page.head}))
    }

    pub(crate) async fn state(&self, caller: &Caller, raw: Value) -> Result<Value, ApiError> {
        Self::project(caller, &raw)?;
        let StateParams { project, key } = params(raw)?;
        if !matches!(key.as_str(), "head" | "roster") {
            return Err(bad_request("key must be roster or head"));
        }
        tokio::time::timeout(DEADLINE, self.hub.state(&project, &key))
            .await
            .map_err(|_| hub_error("state read timed out"))?
            .map_err(Into::into)
    }

    pub(crate) async fn pending(&self, caller: &ManagedIdentity) -> Result<Value, ApiError> {
        let origin = caller.hub_url.origin().ascii_serialization();
        let identity = caller.identity_id.clone();
        let rows = store_work(self.store.clone(), move |store| {
            store.drain(
                &origin,
                &identity,
                500,
                agmen_local::MAX_FRAME as usize - 4096,
            )
        })
        .await?;
        // A successful drain frees quota and starts a fresh explicit run.
        // The batch is already committed. A restart failure cannot replace it
        // with an error, which would hide the messages that were just drained.
        if let Err(error) = self.project_channel(&caller.project).await {
            tracing::warn!(code = ?error.code,"pending batch committed but collector restart failed");
        }
        serde_json::to_value(rows).map_err(|_| hub_error("encoding pending batch failed"))
    }

    /// Keeps a prompt below operating systems' per-argument size limit.
    pub(crate) async fn launch_pending(
        &self,
        caller: &ManagedIdentity,
    ) -> Result<Vec<StampedEnvelope>, ApiError> {
        let origin = caller.hub_url.origin().ascii_serialization();
        let identity = caller.identity_id.clone();
        let rows = store_work(self.store.clone(), move |store| {
            store.drain(&origin, &identity, 32, 96 * 1024)
        })
        .await?;
        if let Err(error) = self.project_channel(&caller.project).await {
            tracing::warn!(code=?error.code,"launcher collector restart failed");
        }
        Ok(rows)
    }

    async fn project_channel(
        &self,
        project: &ProjectId,
    ) -> Result<broadcast::Sender<WatchEvent>, ApiError> {
        let login = self
            .credentials
            .login()
            .await
            .map_err(crate::hub::HubError::from)?;
        let key = (
            login.hub_url.origin().ascii_serialization(),
            login.human_id.clone(),
            project.clone(),
        );
        let mut collectors = self.collectors.lock().await;
        if let Some(collector) = collectors.get(&key)
            && !collector.task.is_finished()
        {
            return Ok(collector.tx.clone());
        }
        if collectors.len() >= 64 && !collectors.contains_key(&key) {
            return Err(hub_error("64 managed projects are already running"));
        }
        let tx = collectors.get(&key).map_or_else(
            || broadcast::channel(256).0,
            |collector| collector.tx.clone(),
        );
        if let Some(collector) = collectors.get_mut(&key) {
            finish_task(&mut collector.task, tokio::time::Instant::now() + DEADLINE).await;
        }
        let context = Collection {
            hub: self.hub.clone(),
            store: self.store.clone(),
            origin: key.0.clone(),
            owner: key.1.clone(),
            project: project.clone(),
            tx: tx.clone(),
        };
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            tokio::select! { () = context.run() => {}, _ = stopped => {} }
        });
        collectors.insert(
            key,
            Collector {
                tx: tx.clone(),
                task,
                stop: Some(stop),
            },
        );
        Ok(tx)
    }

    /// Runs independently of local subscriptions. Removing a login or identity
    /// ends its runtime; durable rows remain for their original owner.
    pub(crate) async fn supervise(&self) {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tick.tick().await;
            let active = tokio::time::timeout(DEADLINE, async {
                let login = self
                    .credentials
                    .login()
                    .await
                    .map_err(crate::hub::HubError::from)?;
                let origin = login.hub_url.origin().ascii_serialization();
                let rows = self.managed(origin.clone(), login.human_id.clone()).await?;
                Ok::<_, ApiError>((origin, login.human_id, rows))
            })
            .await;
            let Ok(Ok((origin, owner, rows))) = active else {
                self.clear().await;
                continue;
            };
            let mut collectors = self.collectors.lock().await;
            let stale: Vec<_> = collectors
                .keys()
                .filter(|(o, h, p)| {
                    *o != origin
                        || *h != owner
                        || (!rows.iter().any(|row| row.project == *p)
                            && collectors
                                .get(&((*o).clone(), (*h).clone(), (*p).clone()))
                                .is_none_or(|collector| collector.tx.receiver_count() == 0))
                })
                .cloned()
                .collect();
            for key in stale {
                if let Some(collector) = collectors.get_mut(&key) {
                    let _receivers = collector.tx.send((None, Err(unauthorized())));
                    if let Some(stop) = collector.stop.take() {
                        let _sent = stop.send(());
                    }
                    finish_task(&mut collector.task, tokio::time::Instant::now() + DEADLINE).await;
                }
                collectors.remove(&key);
            }
            drop(collectors);
            let runtimes = self.runtimes.lock().await;
            let stale_keys: Vec<_> = runtimes
                .keys()
                .filter(|(o, id)| *o != origin || !rows.iter().any(|row| &row.identity_id == id))
                .cloned()
                .collect();
            let stale: Vec<_> = stale_keys
                .iter()
                .filter_map(|key| runtimes.get(key).cloned())
                .collect();
            drop(runtimes);
            stop_runtimes(stale, tokio::time::Instant::now() + DEADLINE).await;
            let mut runtimes = self.runtimes.lock().await;
            for key in stale_keys {
                runtimes.remove(&key);
            }
            drop(runtimes);
            for row in rows {
                // A finished collector stays offline until an explicit call.
                let key = (origin.clone(), owner.clone(), row.project.clone());
                if !self.collectors.lock().await.contains_key(&key)
                    && let Err(error) = self.project_channel(&row.project).await
                {
                    tracing::warn!(code = ?error.code,"starting collector failed");
                }
            }
        }
    }

    async fn clear(&self) {
        let mut collectors = self.collectors.lock().await;
        let deadline = tokio::time::Instant::now() + DEADLINE;
        for collector in collectors.values_mut() {
            let _receivers = collector.tx.send((None, Err(unauthorized())));
            if let Some(stop) = collector.stop.take() {
                let _sent = stop.send(());
            }
            finish_task(&mut collector.task, deadline).await;
        }
        collectors.clear();
        drop(collectors);
        let mut runtimes = self.runtimes.lock().await;
        stop_runtimes(runtimes.values().cloned(), deadline).await;
        runtimes.clear();
    }

    pub(crate) async fn shutdown(&self) {
        self.clear().await;
        let mut jobs = self.store.jobs.lock().await;
        jobs.abort_all();
        while jobs.join_next().await.is_some() {}
    }

    pub(crate) async fn watch(
        &self,
        caller: &Caller,
        id: u64,
        raw: Value,
        mut reader: OwnedReadHalf,
        writer: &mut OwnedWriteHalf,
    ) -> Result<(), ApiError> {
        Self::project(caller, &raw)?;
        let ProjectParams { project } = params(raw)?;
        let login = self
            .credentials
            .login()
            .await
            .map_err(crate::hub::HubError::from)?;
        let me = match caller {
            Caller::Human => Me {
                identity_id: login.human_id.clone(),
                handle: login.handle.clone(),
                owner_id: login.human_id.clone(),
            },
            Caller::Identity(who) => Me {
                identity_id: who.identity_id.clone(),
                handle: who.handle.clone(),
                owner_id: who.owner_id.clone(),
            },
        };
        let tx = self.project_channel(&project).await?;
        let mut events = tx.subscribe();
        write_event(writer, id, &ClientEvent::Me(me.clone())).await?;
        let mut disconnected = [0; 1];
        let mut authority = tokio::time::interval(Duration::from_secs(1));
        loop {
            let (target, event) = tokio::select! {
                _ = reader.read(&mut disconnected) => return Ok(()),
                event = events.recv() => event.map_err(|_|hub_error("watch queue overflowed or closed"))?,
                _ = authority.tick() => {
                    let valid = tokio::time::timeout(DEADLINE,async {
                        match caller {
                            Caller::Identity(identity) => still_managed(&self.credentials,self.store.clone(),identity).await,
                            Caller::Human => self.credentials.login().await.is_ok_and(|current| current.human_id == me.identity_id && current.hub_url == login.hub_url),
                        }
                    }).await;
                    if !matches!(valid,Ok(true)) { return Err(unauthorized()); }
                    continue;
                }
            };
            if target
                .as_ref()
                .is_some_and(|target| target != &me.identity_id)
            {
                continue;
            }
            let event = event?;
            if let ClientEvent::Message { message } = &event
                && !matches!(&message.stream, Stream::Feed)
                && message.stream != Stream::Inbox(me.identity_id.clone())
            {
                continue;
            }
            write_event(writer, id, &event).await?;
        }
    }

    pub(crate) async fn post(
        &self,
        caller: &ManagedIdentity,
        envelope: ClientEnvelope,
    ) -> Result<Value, ApiError> {
        let id = envelope.id.clone();
        let size = serde_json::to_vec(&envelope)
            .map_err(|_| crate::console::bad_request("invalid envelope"))?
            .len();
        if size > agmen_proto::MAX_ENVELOPE_BYTES {
            return Err(crate::console::bad_request(
                "message exceeds protocol envelope cap",
            ));
        }
        let operation = async {
            #[cfg(test)]
            if let Some(publish) = &self.publisher {
                return tokio::time::timeout(POST_WAIT, publish(caller.clone(), envelope))
                    .await
                    .unwrap_or(Ok(OutboxState::Pending { attempts: 0 }));
            }
            let deadline = tokio::time::Instant::now() + DEADLINE;
            let session = tokio::time::timeout_at(deadline, async {
                let session = self.session(caller).await?;
                session
                    .publish(envelope)
                    .await
                    .map_err(|_| hub_error("writing identity outbox failed"))?;
                Ok::<_, ApiError>(session)
            })
            .await
            .map_err(|_| hub_error("identity post timed out before enqueue completed"))??;
            let wait = async {
                // 100 reads at 50 ms bound the ACK wait independently of event volume.
                for _ in 0..100 {
                    let outbox = session.outbox().clone();
                    let id = id.clone();
                    let state = tokio::task::spawn_blocking(move || outbox.state(&id))
                        .await
                        .map_err(|_| hub_error("reading identity outbox failed"))?
                        .map_err(|_| hub_error("reading identity outbox failed"))?;
                    if let Some(state) = state
                        && !matches!(state, OutboxState::Pending { .. })
                    {
                        return Ok(state);
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Ok::<_, ApiError>(OutboxState::Pending { attempts: 0 })
            };
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            tokio::time::timeout(POST_WAIT.min(remaining), wait)
                .await
                .unwrap_or(Ok(OutboxState::Pending { attempts: 0 }))
        };
        let state = operation.await?;
        match state {
            OutboxState::Recorded { stream, seq } => Ok(json!({"stream":stream,"seq":seq})),
            OutboxState::Pending { .. } => Ok(json!({"queued":true,"id":id})),
            OutboxState::Rejected { reason } => Err(hub_error(&format!(
                "hub rejected identity message: {reason}"
            ))),
        }
    }

    async fn session(&self, caller: &ManagedIdentity) -> Result<Arc<Session>, ApiError> {
        let key = (
            caller.hub_url.origin().ascii_serialization(),
            caller.identity_id.clone(),
        );
        let slot = {
            let mut runtimes = self.runtimes.lock().await;
            if !runtimes.contains_key(&key) && runtimes.len() >= 64 {
                return Err(hub_error("64 identity sessions are already running"));
            }
            runtimes
                .entry(key)
                .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
                .clone()
        };
        // Only callers for this identity wait for its connection setup.
        // Cancellation releases the slot without an abandoned in-flight marker.
        let mut runtime = slot.lock().await;
        if let Some(session) = runtime
            .as_ref()
            .filter(|run| !run.task.is_finished())
            .and_then(|run| run.session.upgrade())
        {
            return Ok(session);
        }
        if let Some(run) = runtime.as_mut() {
            if let Some(stop) = run.stop.take() {
                let _sent = stop.send(());
            }
            finish_task(&mut run.task, tokio::time::Instant::now() + DEADLINE).await;
        }
        *runtime = None;
        let token = self
            .credentials
            .token(&caller.hub_url, &caller.identity_id)
            .await?
            .filter(|token| !token.is_empty())
            .ok_or_else(|| hub_error("managed identity has no hub token"))?;
        let hub = HubClient::new(
            caller.hub_url.clone(),
            caller.project.clone(),
            caller.identity_id.clone(),
            token,
        )
        .map_err(|_| hub_error("building identity hub client failed"))?;
        let dir = self
            .state_dir
            .join("identities")
            .join(caller.identity_id.as_str());
        let dir = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)?;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            Ok::<_, std::io::Error>(dir)
        })
        .await
        .map_err(|_| hub_error("creating identity state directory failed"))?
        .map_err(|_| hub_error("creating identity state directory failed"))?;
        let kind = if self.launched.lock().await.contains(&(
            caller.hub_url.origin().ascii_serialization(),
            caller.identity_id.clone(),
        )) {
            ClientKind::Watch
        } else {
            ClientKind::Mcp
        };
        let config = SessionConfig::new(
            kind,
            Topics::new(caller.tenant.clone(), caller.project.clone()),
            hub,
            caller.broker_url.clone(),
            dir,
        );
        let (session, events) = tokio::time::timeout(DEADLINE, Session::connect(config))
            .await
            .map_err(|_| hub_error("connecting identity session timed out"))?
            .map_err(|_| hub_error("connecting identity session failed"))?;
        let session = Arc::new(session);
        if !still_managed(&self.credentials, self.store.clone(), caller).await {
            return Err(unauthorized());
        }
        let tx = self.project_channel(&caller.project).await?;
        *runtime = Some(Runtime::start(
            session.clone(),
            events,
            self.credentials.clone(),
            self.store.clone(),
            caller.clone(),
            tx,
            kind == ClientKind::Watch,
        ));
        Ok(session)
    }
}

impl Runtime {
    fn start(
        held: Arc<Session>,
        mut events: agmen_client::Events,
        credentials: Arc<Credentials>,
        store: Arc<Database>,
        identity: ManagedIdentity,
        tx: broadcast::Sender<WatchEvent>,
        reconnect: bool,
    ) -> Self {
        let session = Arc::downgrade(&held);
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut check = tokio::time::interval(Duration::from_secs(1));
            // Managed Watch sessions retain the client's reconnect loop so idle
            // agents keep presence after transient broker loss. Manual MCP
            // sessions still require an explicit post to restart.
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    event = events.recv() => match event {
                        None => break,
                        Some(event @ (ClientEvent::Delivery { .. } | ClientEvent::Connection { .. })) => {
                            if let ClientEvent::Delivery { id, .. } = &event {
                                let id = id.clone();
                                let origin = identity.hub_url.origin().ascii_serialization();
                                let who = identity.identity_id.clone();
                                if let Err(error) = store_work(store.clone(), move |s| s.remember_own(&origin, &who, &id)).await {
                                    tracing::warn!(code = ?error.code, "remembering outgoing message failed");
                                }
                            }
                            // No watcher is fine; authoritative messages come from SSE.
                            let disconnected = matches!(event,ClientEvent::Connection { connected:false });
                            let _receivers = tx.send((Some(identity.identity_id.clone()), Ok(event)));
                            if disconnected && !reconnect { break; }
                        }
                        Some(_) => {}
                    },
                    _ = check.tick() => {
                        let valid = tokio::time::timeout(DEADLINE, still_managed(&credentials, store.clone(), &identity)).await;
                        if !matches!(valid, Ok(true)) { break; }
                    }
                }
            }
            if let Ok(session) = Arc::try_unwrap(held)
                && let Err(error) = session.close().await
            {
                tracing::debug!(%error,"identity session close failed");
            }
        });
        Self {
            session,
            task,
            stop: Some(stop),
        }
    }
}

async fn stop_runtimes(
    slots: impl IntoIterator<Item = RuntimeSlot>,
    deadline: tokio::time::Instant,
) {
    for slot in slots {
        if let Ok(mut run) = slot.try_lock()
            && let Some(runtime) = run.as_mut()
        {
            if let Some(stop) = runtime.stop.take() {
                let _sent = stop.send(());
            }
            finish_task(&mut runtime.task, deadline).await;
            *run = None;
        }
    }
}

async fn finish_task(task: &mut tokio::task::JoinHandle<()>, deadline: tokio::time::Instant) {
    let until = deadline.min(tokio::time::Instant::now() + Duration::from_secs(5));
    if tokio::time::timeout_at(until, &mut *task).await.is_err() {
        task.abort();
        if let Err(error) = task.await
            && !error.is_cancelled()
        {
            tracing::warn!(%error,"daemon task shutdown failed");
        }
    }
}

async fn write_event(
    writer: &mut OwnedWriteHalf,
    id: u64,
    event: &ClientEvent,
) -> Result<(), ApiError> {
    let response = agmen_local::Response::ok(
        id,
        serde_json::to_value(event).map_err(|_| hub_error("encoding watch event failed"))?,
    );
    tokio::time::timeout(DEADLINE, agmen_local::frame::write_async(writer, &response))
        .await
        .map_err(|_| hub_error("watch write timed out"))?
        .map_err(|_| hub_error("watch write failed"))
}

async fn store_work<T: Send + 'static>(
    database: Arc<Database>,
    work: impl FnOnce(&mut Store) -> Result<T, crate::state::StoreError> + Send + 'static,
) -> Result<T, ApiError> {
    let store = database.store.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = Cancellation(cancelled.clone());
    let (send, receive) = oneshot::channel();
    let mut jobs = database.jobs.lock().await;
    while jobs.try_join_next().is_some() {}
    if jobs.len() >= 64 {
        return Err(hub_error("64 daemon state operations are already running"));
    }
    jobs.spawn_blocking(move || {
        let result = (|| {
            // Bound lock contention to 5 s independently of SQLite's timeout.
            // Cancelled queued work must not mutate the store later.
            for _ in 0..100 {
                if cancelled.load(Ordering::Acquire) {
                    return Err(hub_error("daemon state operation cancelled"));
                }
                match store.try_lock() {
                    Ok(mut guard) => {
                        if cancelled.load(Ordering::Acquire) {
                            return Err(hub_error("daemon state operation cancelled"));
                        }
                        return work(&mut guard)
                            .map_err(|_| hub_error("daemon state operation failed"));
                    }
                    Err(std::sync::TryLockError::Poisoned(_)) => {
                        return Err(hub_error("daemon state lock failed"));
                    }
                    Err(std::sync::TryLockError::WouldBlock) => {
                        std::thread::sleep(Duration::from_millis(50))
                    }
                }
            }
            Err(hub_error("daemon state lock timed out"))
        })();
        // A caller can leave after a transaction committed.
        let _received = send.send(result);
    });
    drop(jobs);
    tokio::time::timeout(DEADLINE, receive)
        .await
        .map_err(|_| hub_error("daemon state operation timed out"))?
        .map_err(|_| hub_error("daemon state worker failed"))?
}

struct Collection {
    hub: Arc<Hub>,
    store: Arc<Database>,
    origin: String,
    owner: IdentityId,
    project: ProjectId,
    tx: broadcast::Sender<WatchEvent>,
}

impl Collection {
    fn send(&self, event: Result<ClientEvent, ApiError>) {
        // No local watchers is normal; persistence does not depend on them.
        let _receivers = self.tx.send((None, event));
    }

    async fn positions(&self) -> Result<BTreeMap<Stream, u64>, ApiError> {
        let origin = self.origin.clone();
        let project = self.project.clone();
        let owner = self.owner.clone();
        store_work(self.store.clone(), move |s| {
            s.positions_for_owner(&origin, &owner, &project)
        })
        .await
    }

    async fn commit(&self, message: StampedEnvelope) -> Result<(), ApiError> {
        if serde_json::to_vec(&message.client)
            .map_err(|_| hub_error("invalid envelope"))?
            .len()
            > agmen_proto::MAX_ENVELOPE_BYTES
        {
            return Err(hub_error("message exceeds protocol envelope cap"));
        }
        let login = self
            .hub
            .credentials()
            .login()
            .await
            .map_err(crate::hub::HubError::from)?;
        if login.human_id != self.owner
            || login.hub_url.origin().ascii_serialization() != self.origin
        {
            return Err(unauthorized());
        }
        let origin = self.origin.clone();
        let project = self.project.clone();
        let saved = message.clone();
        let owner = self.owner.clone();
        store_work(self.store.clone(), move |s| {
            s.ingest_for_owner(&origin, &project, &saved, Some(&owner))
        })
        .await?;
        self.send(Ok(ClientEvent::Message {
            message: Box::new(message),
        }));
        Ok(())
    }

    async fn accept(&self, message: StampedEnvelope) -> Result<(), ApiError> {
        let stream = message.stream.clone();
        let mut last = self.positions().await?.get(&stream).copied().unwrap_or(0);
        if message.seq.0 <= last {
            return Ok(());
        }
        // At most 100 pages per catch-up, committing progress after each row.
        for _ in 0..100 {
            if message.seq.0 == last.saturating_add(1) {
                return self.commit(message).await;
            }
            let page = tokio::time::timeout(
                DEADLINE,
                self.hub.stream_page(&self.project, &stream, last, 500),
            )
            .await
            .map_err(|_| hub_error("gap replay timed out"))??;
            if page.messages.is_empty() {
                return Err(hub_error("gap replay made no progress"));
            }
            for row in page.messages {
                if row.seq.0 != last.saturating_add(1) {
                    return Err(hub_error("noncontiguous gap replay"));
                }
                last = row.seq.0;
                self.commit(row).await?;
                if last >= message.seq.0 {
                    return Ok(());
                }
            }
            tokio::task::yield_now().await;
        }
        Err(hub_error("gap replay incomplete after 100 pages"))
    }

    async fn run(self) {
        for attempt in 0..=10_u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs((1_u64 << (attempt - 1).min(5)).min(30)))
                    .await;
            }
            let positions = match self.positions().await {
                Ok(p) => p,
                Err(error) => {
                    self.send(Err(error));
                    return;
                }
            };
            let response =
                tokio::time::timeout(DEADLINE, self.hub.events(&self.project, &positions, None))
                    .await;
            let mut response = match response {
                Ok(Ok(response)) => response,
                Ok(Err(error)) if error.is_final() => {
                    self.send(Err(error.into()));
                    return;
                }
                _ => {
                    self.send(Ok(ClientEvent::Connection { connected: false }));
                    continue;
                }
            };
            self.send(Ok(ClientEvent::Connection { connected: true }));
            let mut parser = crate::sse::SseParser::default();
            loop {
                let chunk = match response.chunk().await {
                    Ok(Some(chunk)) => chunk,
                    _ => break,
                };
                for part in chunk.chunks(16 * 1024) {
                    let events = match parser.push(part) {
                        Ok(events) => events,
                        Err(_) => {
                            self.send(Err(hub_error("invalid SSE stream")));
                            return;
                        }
                    };
                    for event in events {
                        let message = match serde_json::from_str(&event.data) {
                            Ok(message) => message,
                            Err(_) => {
                                self.send(Err(hub_error("invalid SSE envelope")));
                                return;
                            }
                        };
                        if let Err(error) = self.accept(message).await {
                            self.send(Err(error));
                            return;
                        }
                    }
                }
            }
            self.send(Ok(ClientEvent::Connection { connected: false }));
        }
        self.send(Err(hub_error("collector offline after 10 reconnects")));
    }
}

async fn still_managed(
    credentials: &Credentials,
    store: Arc<Database>,
    identity: &ManagedIdentity,
) -> bool {
    let Ok(login) = credentials.login().await else {
        return false;
    };
    let origin = identity.hub_url.origin().ascii_serialization();
    if login.human_id != identity.owner_id || login.hub_url.origin().ascii_serialization() != origin
    {
        return false;
    }
    let expected = identity.clone();
    // Any failed state read ends the runtime, rather than retaining authority
    // after the daemon can no longer verify its managed identity.
    store_work(store, move |s| {
        s.identities(&origin, &expected.owner_id)
            .map(|rows| rows.contains(&expected))
    })
    .await
    .unwrap_or(false)
}

fn valid_token(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn tokens_match(stored: &str, supplied: &str) -> bool {
    if stored.len() != 64 || supplied.len() != 64 {
        return false;
    }
    stored
        .bytes()
        .zip(supplied.bytes())
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

pub(crate) fn unauthorized() -> ApiError {
    ApiError {
        code: ErrorCode::Unauthorized,
        message: "invalid local identity credential".into(),
    }
}

pub(crate) fn forbidden() -> ApiError {
    ApiError {
        code: ErrorCode::Forbidden,
        message: "method is not available to identity callers".into(),
    }
}

fn hub_error(message: &str) -> ApiError {
    ApiError {
        code: ErrorCode::Hub,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agmen_client::{Login, TokenKey, TokenStore};
    use agmen_local::{Request, Response, frame};
    use agmen_proto::{NackReason, Seq, Stream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::UnixStream;

    struct Fixture {
        _dir: tempfile::TempDir,
        daemon: crate::Agmend,
        who: ManagedIdentity,
        tokens: TokenStore,
    }

    impl Fixture {
        fn new(publisher: Option<Publisher>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let tokens = TokenStore::Dir(dir.path().join("tokens"));
            let login_file = dir.path().join("login.json");
            let who = ManagedIdentity {
                hub_url: "https://hub.example".parse().unwrap(),
                broker_url: "mqtt://127.0.0.1:9".parse().unwrap(),
                tenant: "tenant01".parse().unwrap(),
                project: "project1".parse().unwrap(),
                identity_id: "agent001".parse().unwrap(),
                owner_id: "owner001".parse().unwrap(),
                handle: "owner/agent".into(),
                kind: agmen_proto::IdentityKind::Agent,
            };
            std::fs::write(
                &login_file,
                serde_json::to_vec(&Login {
                    hub_url: who.hub_url.clone(),
                    human_id: who.owner_id.clone(),
                    handle: "owner".into(),
                })
                .unwrap(),
            )
            .unwrap();
            let config =
                crate::Config::new(dir.path().join("agmend.sock"), login_file, tokens.clone());
            drop(crate::Agmend::bind(config.clone()).unwrap());
            rusqlite::Connection::open(dir.path().join("agmend.db"))
                .unwrap()
                .execute(
                    "INSERT INTO identities VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![
                        who.hub_url.origin().ascii_serialization(),
                        who.identity_id.as_str(),
                        who.owner_id.as_str(),
                        who.project.as_str(),
                        serde_json::to_string(&who).unwrap()
                    ],
                )
                .unwrap();
            tokens.set(&TokenKey::new(&"https://agmend.invalid".parse().unwrap(), &who.identity_id).unwrap(), &json!({"origin":"https://hub.example", "owner_id":"owner001", "local_token":"a".repeat(64)}).to_string()).unwrap();
            let mut daemon = crate::Agmend::bind(config).unwrap();
            Arc::get_mut(&mut Arc::get_mut(&mut daemon.core).unwrap().identities)
                .unwrap()
                .publisher = publisher;
            Self {
                _dir: dir,
                daemon,
                who,
                tokens,
            }
        }

        async fn call(&self, method: &str, raw: Value) -> Response {
            let (mut client, server) = UnixStream::pair().unwrap();
            let core = self.daemon.core.clone();
            let task = tokio::spawn(crate::api::serve(server, core));
            let response = tokio::time::timeout(Duration::from_secs(8), async {
                frame::write_async(&mut client, &Request::new(1, "hello", json!({"api":1})))
                    .await
                    .unwrap();
                let hello: Response = frame::read_async(&mut client).await.unwrap();
                assert!(matches!(hello.outcome, agmen_local::Outcome::Ok { .. }));
                let mut request = Request::new(2, method, raw);
                request.as_ = Some(self.who.identity_id.clone());
                frame::write_async(&mut client, &request).await.unwrap();
                frame::read_async(&mut client).await.unwrap()
            })
            .await
            .unwrap();
            drop(client);
            task.await.unwrap();
            response
        }
    }

    fn result(response: Response) -> Value {
        match response.outcome {
            agmen_local::Outcome::Ok { result } => result,
            other => panic!("expected result, received {other:?}"),
        }
    }

    fn error(response: Response) -> ApiError {
        match response.outcome {
            agmen_local::Outcome::Err { error } => error,
            other => panic!("expected error, received {other:?}"),
        }
    }

    fn envelope() -> Value {
        json!({"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"FYI","data":{},"body":"hello","sent_at":"2026-10-27T12:00:00Z"})
    }

    #[tokio::test]
    async fn managed_agent_methods_require_a_human_caller() {
        let fixture = Fixture::new(None);
        for method in ["agent_launch", "agent_stop", "agent_list"] {
            let refusal = error(
                fixture
                    .call(method, json!({"local_token":"a".repeat(64)}))
                    .await,
            );
            assert_eq!(refusal.code, ErrorCode::Forbidden);
        }
    }

    #[tokio::test]
    async fn cancelled_store_work_does_not_mutate_later() {
        let fixture = Fixture::new(None);
        let database = fixture.daemon.core.identities.store.clone();
        let store = database.store.clone();
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let (locked, ready) = oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _held = store.lock().unwrap();
            locked.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        ready.await.unwrap();
        let changed = Arc::new(AtomicBool::new(false));
        let mutation = changed.clone();
        let (queued, started) = oneshot::channel();
        let work = tokio::spawn(async move {
            queued.send(()).unwrap();
            store_work(database, move |_| {
                mutation.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
        });
        started.await.unwrap();
        // Give the worker a chance to block on the held database lock.
        tokio::time::sleep(Duration::from_millis(100)).await;
        work.abort();
        assert!(work.await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        blocker.await.unwrap();
        fixture.daemon.core.identities.shutdown().await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !changed.load(Ordering::SeqCst),
            "cancelled queued database work ran later"
        );
    }

    #[tokio::test]
    async fn collector_restart_preserves_session_delivery_channel() {
        let fixture = Fixture::new(None);
        let identities = &fixture.daemon.core.identities;
        let (tx, _) = broadcast::channel(256);
        let task = tokio::spawn(async {});
        tokio::time::timeout(Duration::from_secs(5), async {
            for _ in 0..100 {
                if task.is_finished() {
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("fixture collector did not finish");
        })
        .await
        .unwrap();
        identities.collectors.lock().await.insert(
            (
                fixture.who.hub_url.origin().ascii_serialization(),
                fixture.who.owner_id.clone(),
                fixture.who.project.clone(),
            ),
            Collector {
                tx: tx.clone(),
                task,
                stop: None,
            },
        );
        let channel = identities
            .project_channel(&fixture.who.project)
            .await
            .unwrap();
        let mut watch = channel.subscribe();
        let delivery = ClientEvent::Delivery {
            id: "01JBX6Q4T7W3C2M8Y9K5N0P1RS".parse().unwrap(),
            state: OutboxState::Pending { attempts: 1 },
        };
        tx.send((Some(fixture.who.identity_id.clone()), Ok(delivery.clone())))
            .unwrap();
        let received = tokio::time::timeout(Duration::from_secs(5), watch.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received.1.unwrap(), delivery);
        identities.shutdown().await;
    }

    #[tokio::test]
    async fn pending_drain_survives_collector_restart_failure() {
        let fixture = Fixture::new(None);
        let identities = &fixture.daemon.core.identities;
        let message: StampedEnvelope = serde_json::from_value(json!({
            "v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"DM","data":{},
            "body":"keep the committed response","to":["agent001"],"sent_at":"2026-10-27T12:00:00Z",
            "stream":"inbox:agent001","seq":1,"from":"sender01","from_handle":"sender",
            "from_kind":"human","hub_at":"2026-10-27T12:00:00Z"
        }))
        .unwrap();
        identities
            .store
            .store
            .lock()
            .unwrap()
            .ingest("https://hub.example", &fixture.who.project, &message)
            .unwrap();
        for n in 0..64 {
            let (tx, _) = broadcast::channel(256);
            let (stop, stopped) = oneshot::channel();
            let task = tokio::spawn(async {
                let _stopped = stopped.await;
            });
            identities.collectors.lock().await.insert(
                (
                    "https://hub.example".into(),
                    fixture.who.owner_id.clone(),
                    format!("proj{n:04}").parse().unwrap(),
                ),
                Collector {
                    tx,
                    task,
                    stop: Some(stop),
                },
            );
        }
        let response = fixture
            .call("pending", json!({"local_token":"a".repeat(64)}))
            .await;
        identities.shutdown().await;
        assert_eq!(result(response)[0]["body"], "keep the committed response");
    }

    #[tokio::test]
    async fn slow_connect_does_not_block_sibling_post() {
        use tokio::io::AsyncReadExt as _;
        let fixture = Fixture::new(None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut slow = fixture.who.clone();
        slow.hub_url = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        fixture
            .tokens
            .set(
                &TokenKey::new(&slow.hub_url, &slow.identity_id).unwrap(),
                "fake-hub-token",
            )
            .unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let identities = fixture.daemon.core.identities.clone();
        let connecting = identities.clone();
        let mut sibling = slow.clone();
        sibling.identity_id = "agent002".parse().unwrap();
        let first = tokio::spawn(async move { connecting.session(&slow).await });
        tokio::time::timeout(Duration::from_secs(5), ready)
            .await
            .unwrap()
            .unwrap();
        let answer = tokio::time::timeout(
            Duration::from_millis(250),
            identities.post(&sibling, serde_json::from_value(envelope()).unwrap()),
        )
        .await;
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        assert_eq!(
            answer
                .expect("sibling post must not wait for another identity's connect")
                .unwrap_err()
                .message,
            "managed identity has no hub token"
        );
    }

    #[tokio::test]
    async fn seeded_identity_post_uses_its_session() {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let fixture = Fixture::new(Some(Arc::new(move |caller, env| {
            assert_eq!(caller.identity_id.as_str(), "agent001");
            assert_eq!(caller.project.as_str(), "project1");
            assert_eq!(env.body.as_str(), "hello");
            seen.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(OutboxState::Recorded {
                    stream: Stream::Feed,
                    seq: Seq(7),
                })
            })
        })));
        let response = fixture
            .call(
                "post",
                json!({"project":"project1","local_token":"a".repeat(64),"envelope":envelope()}),
            )
            .await;
        assert_eq!(response.id, 2);
        assert_eq!(result(response), json!({"stream":"feed","seq":7}));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn identity_post_pending_and_final_rejection() {
        for (state, expected) in [
            (OutboxState::Pending { attempts: 1 }, None),
            (
                OutboxState::Rejected {
                    reason: NackReason::RateLimited,
                },
                Some(ErrorCode::Hub),
            ),
        ] {
            let fixture = Fixture::new(Some(Arc::new(move |_, _| {
                let state = state.clone();
                Box::pin(async { Ok(state) })
            })));
            let response = fixture
                .call(
                    "post",
                    json!({"local_token":"a".repeat(64),"envelope":envelope()}),
                )
                .await;
            match expected {
                None => assert_eq!(
                    result(response),
                    json!({"queued":true,"id":envelope()["id"]})
                ),
                Some(code) => {
                    let error = error(response);
                    assert_eq!(error.code, code);
                    assert!(error.message.contains("rate_limited"));
                }
            }
        }
    }

    #[tokio::test]
    async fn identity_post_stops_waiting_after_five_seconds() {
        let fixture = Fixture::new(Some(Arc::new(|_, _| Box::pin(std::future::pending()))));
        let start = tokio::time::Instant::now();
        let response = fixture
            .call(
                "post",
                json!({"local_token":"a".repeat(64),"envelope":envelope()}),
            )
            .await;
        assert_eq!(
            result(response),
            json!({"queued":true,"id":envelope()["id"]})
        );
        assert!(start.elapsed() >= POST_WAIT);
        assert!(start.elapsed() < Duration::from_secs(7));
    }

    #[tokio::test]
    async fn identity_project_cannot_be_selected_by_caller() {
        let fixture = Fixture::new(Some(Arc::new(|_, _| {
            panic!("foreign project must not publish")
        })));
        let response = fixture
            .call(
                "post",
                json!({"project":"project2","local_token":"a".repeat(64),"envelope":envelope()}),
            )
            .await;
        assert_eq!(error(response).code, ErrorCode::Forbidden);
    }

    #[tokio::test]
    async fn missing_hub_token_is_contextual() {
        let fixture = Fixture::new(None);
        assert!(
            fixture
                .tokens
                .get(&TokenKey::new(&fixture.who.hub_url, &fixture.who.identity_id).unwrap())
                .unwrap()
                .is_none()
        );
        let response = fixture
            .call(
                "post",
                json!({"local_token":"a".repeat(64),"envelope":envelope()}),
            )
            .await;
        let error = error(response);
        assert_eq!(error.code, ErrorCode::Hub);
        assert!(error.message.contains("no hub token"));
        assert!(!error.message.contains(&"a".repeat(64)));
    }
}
