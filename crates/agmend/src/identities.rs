//! Per-request local caller authentication and identity session publishing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agmen_client::{
    ClientEvent, ClientKind, HubClient, IdentityFile, LocalCredential, Me, OutboxState, Session,
    SessionConfig,
};
use agmen_local::safeguards::MAX_MANAGED;
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{ClientEnvelope, IdentityId, ProjectId, StampedEnvelope, Stream, Topics};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc, oneshot};
use url::Url;

use crate::console::{bad_request, params};
use crate::hub::Hub;
use crate::login::Credentials;
use crate::state::{MAX_STREAM_POSITIONS, ManagedIdentity, Store};

#[path = "identity_watch.rs"]
mod watch;

#[cfg(test)]
#[path = "queue_full_tests.rs"]
mod queue_full_tests;

const DEADLINE: Duration = Duration::from_secs(10);
const POST_WAIT: Duration = Duration::from_secs(5);
const MAX_DATABASE_JOBS: usize = 64;
const MAX_COLLECTORS: usize = 64;
const WATCH_CAPACITY: usize = 256;
const LOCAL_RECHECK: Duration = Duration::from_secs(30);
const COLLECTION_PAGE_SIZE: u32 = 500;
const MAX_COLLECTION_PAGES: usize = 100;
const AUTHORITY_RECHECK: Duration = Duration::from_secs(1);
const MAX_QUEUE_ALERT_ATTEMPTS: usize = 3;
/// agmen-hub's events endpoint refuses more positions in one SSE request.
const MAX_HUB_AFTER: usize = 256;
const MAX_HUB_CONNECTIONS: usize = MAX_STREAM_POSITIONS.div_ceil(MAX_HUB_AFTER);
const MAX_HUB_RECONNECTS: u32 = 10;
const HUB_BACKOFF_SHIFT: u32 = 5;
const HUB_MAX_BACKOFF: Duration = Duration::from_secs(30);
const HUB_READ_WAIT: Duration = Duration::from_secs(45);
const HUB_EVENT_QUEUE: usize = 64;
const HUB_PARSE_BYTES: usize = 16 * 1024;

type RuntimeKey = (String, IdentityId);
type RuntimeSlot = Arc<AsyncMutex<Option<Runtime>>>;
type ProjectKey = (String, IdentityId, ProjectId);
type WatchEvent = (Option<IdentityId>, Result<ClientEvent, ApiError>);

#[derive(Debug)]
pub(crate) struct Database {
    store: Arc<Mutex<Store>>,
    jobs: AsyncMutex<tokio::task::JoinSet<()>>,
}

impl Database {
    pub(crate) fn new(store: Arc<Mutex<Store>>) -> Self {
        Self {
            store,
            jobs: AsyncMutex::new(tokio::task::JoinSet::new()),
        }
    }

    pub(crate) async fn shutdown(&self) {
        let mut jobs = self.jobs.lock().await;
        jobs.abort_all();
        for _ in 0..MAX_DATABASE_JOBS {
            if jobs.join_next().await.is_none() {
                break;
            }
        }
    }
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
    after: Option<BTreeMap<Stream, u64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

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

#[derive(Clone)]
pub(crate) struct LocalToken(String);

impl std::fmt::Debug for LocalToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalToken(<redacted>)")
    }
}

impl LocalToken {
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
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

/// Where agmend reads an identity's records and answers a failure with busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Site {
    /// `Identities::authorize`, on each request that names an identity.
    Connect,
    /// The authority recheck of an open identity watch.
    Recheck,
}

/// Why a read failed, as far as the log tells one failure from another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cause {
    /// The read did not finish within `DEADLINE`.
    TimedOut,
    /// The read failed with this code.
    Code(ErrorCode),
}

pub(crate) struct Identities {
    origins: Arc<crate::origin::Origins>,
    credentials: Arc<Credentials>,
    store: Arc<Database>,
    state_dir: PathBuf,
    hub: Arc<Hub>,
    runtimes: AsyncMutex<BTreeMap<RuntimeKey, RuntimeSlot>>,
    collectors: AsyncMutex<BTreeMap<ProjectKey, Collector>>,
    /// The code of the catch-up failure last warned about, for each local project still failing.
    catch_up_failures: Mutex<BTreeMap<ProjectId, ErrorCode>>,
    /// The cause of the busy answer last warned about, for each site whose reads still fail.
    read_failures: Mutex<BTreeMap<Site, Cause>>,
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
    pub(crate) async fn local_token(
        &self,
        id: &IdentityId,
    ) -> Result<Option<LocalToken>, ApiError> {
        tokio::time::timeout(DEADLINE, async {
            let Some(identity) = self.owned_identity(id).await? else {
                return Ok(None);
            };
            let origin = agmen_client::origin_key(&identity.hub_url);
            let namespace = Url::parse("https://agmend.invalid").map_err(|_| unauthorized())?;
            let Some(raw) = self.credentials.token(&namespace, id).await? else {
                return Ok(None);
            };
            let credential: LocalCredential =
                serde_json::from_str(&raw).map_err(|_| unauthorized())?;
            if credential.origin != origin
                || credential.owner_id != identity.owner_id
                || !valid_token(&credential.local_token)
            {
                return Err(unauthorized());
            }
            Ok(Some(LocalToken(credential.local_token)))
        })
        .await
        .map_err(|_| unauthorized())?
    }

    pub(crate) async fn register(&self, identity: ManagedIdentity) -> Result<(), ApiError> {
        let key = (
            agmen_client::origin_key(&identity.hub_url),
            identity.identity_id.clone(),
        );
        // A row from an earlier run holds waiting messages and receipts that
        // a failed token must not erase, so only a row this call adds is
        // rolled back.
        let already_registered = self
            .managed(key.0.clone(), identity.owner_id.clone())
            .await?
            .iter()
            .any(|row| row.identity_id == identity.identity_id);
        let row = identity.clone();
        store_work(self.store.clone(), move |store| store.register(&row)).await?;
        let issued = tokio::time::timeout(DEADLINE, async {
            let namespace = Url::parse("https://agmend.invalid").map_err(|_| unauthorized())?;
            if let Some(raw) = self
                .credentials
                .token(&namespace, &identity.identity_id)
                .await?
                && serde_json::from_str::<LocalCredential>(&raw).is_ok()
            {
                return Ok(());
            }
            let token = tokio::task::spawn_blocking(|| {
                let mut bytes = [0_u8; 32];
                std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
                Ok::<String, std::io::Error>(
                    bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
                )
            })
            .await
            .map_err(|_| hub_error("local identity token worker failed"))?
            .map_err(|_| hub_error("generating local identity token failed"))?;
            let credential = LocalCredential {
                origin: key.0.clone(),
                owner_id: identity.owner_id.clone(),
                local_token: token,
            };
            let raw = serde_json::to_string(&credential)
                .map_err(|_| hub_error("encoding local identity credential failed"))?;
            self.credentials
                .set_token(&namespace, &identity.identity_id, raw)
                .await
        })
        .await
        .unwrap_or_else(|_| Err(hub_error("local identity token storage timed out")));
        if let Err(error) = issued {
            if already_registered {
                return Err(error);
            }
            let origin = key.0;
            let id = identity.identity_id;
            store_work(self.store.clone(), move |store| {
                store.unregister(&origin, &id)
            })
            .await?;
            return Err(error);
        }
        self.launched.lock().await.insert(key);
        Ok(())
    }

    /// Forgets a removed identity: closes its session and drops its row and
    /// waiting messages, so it no longer counts toward the identity cap.
    pub(crate) async fn forget(&self, identity: &ManagedIdentity) -> Result<(), ApiError> {
        self.close_identity(identity).await;
        let origin = agmen_client::origin_key(&identity.hub_url);
        self.launched
            .lock()
            .await
            .remove(&(origin.clone(), identity.identity_id.clone()));
        let id = identity.identity_id.clone();
        store_work(self.store.clone(), move |store| {
            store.unregister(&origin, &id)
        })
        .await?;
        let namespace = Url::parse("https://agmend.invalid").map_err(|_| unauthorized())?;
        if !matches!(
            tokio::time::timeout(
                DEADLINE,
                self.credentials
                    .delete_token(&namespace, &identity.identity_id)
            )
            .await,
            Ok(Ok(()))
        ) {
            tracing::warn!(
                identity = %identity.identity_id,
                "the forgotten identity's local credential was not deleted"
            );
        }
        Ok(())
    }

    pub(crate) async fn close_identity(&self, identity: &ManagedIdentity) {
        let key = (
            agmen_client::origin_key(&identity.hub_url),
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
        origins: Arc<crate::origin::Origins>,
    ) -> Self {
        Self {
            hub: origins.hub().clone(),
            origins,
            credentials,
            store: Arc::new(Database::new(store)),
            state_dir,
            runtimes: AsyncMutex::new(BTreeMap::new()),
            collectors: AsyncMutex::new(BTreeMap::new()),
            catch_up_failures: Mutex::new(BTreeMap::new()),
            read_failures: Mutex::new(BTreeMap::new()),
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
        // A read that fails or runs out of time says nothing against the caller:
        // it is refused with the answer `read_failed` gives and never lets the
        // call through. `unauthorized` stays for an identity that is unknown or
        // revoked, and for a credential that reads and does not match.
        tokio::time::timeout(DEADLINE, async {
            let caller = self
                .owned_identity(id)
                .await
                .map_err(|error| self.read_failed(Site::Connect, id, error))?
                .ok_or_else(unauthorized)?;
            let origin = agmen_client::origin_key(&caller.hub_url);
            let namespace = Url::parse("https://agmend.invalid").map_err(|_| unauthorized())?;
            let raw = self
                .credentials
                .token(&namespace, id)
                .await
                .map_err(|error| self.read_failed(Site::Connect, id, error))?;
            // Both reads came back, so a failure that went before is over.
            self.read_worked(Site::Connect);
            let raw = raw.ok_or_else(unauthorized)?;
            let credential: LocalCredential =
                serde_json::from_str(&raw).map_err(|_| unauthorized())?;
            if credential.origin != origin
                || credential.owner_id != caller.owner_id
                || !tokens_match(&credential.local_token, supplied)
            {
                return Err(unauthorized());
            }
            Ok(Caller::Identity(caller))
        })
        .await
        .unwrap_or_else(|_| Err(self.read_timed_out(Site::Connect, id)))
    }

    /// The answer for a read at `site` that failed with `error`. A newer
    /// local.db is answered as it is, since no retry changes it, and so is a
    /// stopped origin, which the CLI retries. Anything else is answered busy,
    /// which tells the client nothing of the cause, so it is logged here.
    fn read_failed(&self, site: Site, who: &IdentityId, error: ApiError) -> ApiError {
        if error.code == ErrorCode::Version || crate::local_origin::is_unavailable(&error) {
            return error;
        }
        self.log_busy(site, who, Cause::Code(error.code), &error.message);
        crate::local_origin::busy()
    }

    /// The answer for a read at `site` that ran out of time.
    fn read_timed_out(&self, site: Site, who: &IdentityId) -> ApiError {
        self.log_busy(site, who, Cause::TimedOut, "no answer in time");
        crate::local_origin::busy()
    }

    /// The reads at `site` came back, so their next failure is news again.
    fn read_worked(&self, site: Site) {
        self.read_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&site);
    }

    /// Like `log_catch_up`, a failure that stays warns when it first appears or
    /// its cause changes, and logs at debug after that. The record is per site,
    /// not per identity: the stores that fail are shared by all of them, and at
    /// connect the identity is only what the caller claims until a read
    /// confirms it, so keying by it would let a caller dodge the limit.
    // shortcut: a read that works for one identity ends the record of another
    // whose credential cannot be read, so that one warns on each try, key the
    // credential read by its registered identity when that is too noisy.
    fn log_busy(&self, site: Site, who: &IdentityId, cause: Cause, detail: &str) {
        let mut failing = self
            .read_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failing.insert(site, cause) == Some(cause) {
            tracing::debug!(
                ?site,
                identity = %who,
                ?cause,
                detail,
                "a local read failed again; the caller was answered busy"
            );
        } else {
            tracing::warn!(
                ?site,
                identity = %who,
                ?cause,
                detail,
                "a local read failed; the caller was answered busy"
            );
        }
    }

    pub(crate) async fn managed(
        &self,
        origin: String,
        owner: IdentityId,
    ) -> Result<Vec<ManagedIdentity>, ApiError> {
        let store = self.store.clone();
        store_work(store, move |s| s.identities(&origin, &owner)).await
    }

    async fn owned_identity(&self, id: &IdentityId) -> Result<Option<ManagedIdentity>, ApiError> {
        // A missing or unreadable hub login must not remove local authority.
        if let Ok(login) = self.credentials.login().await
            && let Some(identity) = self.owned_by(&login, id).await?
        {
            return Ok(Some(identity));
        }
        match self.origins.local().login().await? {
            Some(login) => self.owned_by(&login, id).await,
            None => Ok(None),
        }
    }

    async fn owned_by(
        &self,
        login: &agmen_client::Login,
        id: &IdentityId,
    ) -> Result<Option<ManagedIdentity>, ApiError> {
        let origin = agmen_client::origin_key(&login.hub_url);
        Ok(self
            .managed(origin.clone(), login.human_id.clone())
            .await?
            .into_iter()
            .find(|row| {
                row.identity_id == *id
                    && row.owner_id == login.human_id
                    && agmen_client::origin_key(&row.hub_url) == origin
            }))
    }

    /// Uses the project's known checkout for roles, or only built-in and personal
    /// layers when no checkout is known.
    pub(crate) async fn local_mint(
        &self,
        managed: &crate::managed::Managed,
        raw: Value,
    ) -> Result<Value, ApiError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Mint {
            project: ProjectId,
            name: String,
            kind: Option<agmen_proto::IdentityKind>,
            role: Option<String>,
        }
        let Mint {
            project,
            name,
            kind,
            role,
        } = params(raw)?;
        let local = self.origins.local();
        // Preserve local.db version errors before mint maps open failures to a hub refusal.
        let login = local.login().await?.ok_or_else(|| ApiError {
            code: ErrorCode::NotFound,
            message: "no local person on this machine".into(),
        })?;
        let resolved = match role {
            Some(name) => {
                let checkout = managed.known_checkout(&project).await;
                Some(
                    managed
                        .resolve_role_layers(checkout.as_deref(), &name)
                        .await?,
                )
            }
            None => None,
        };
        let kind = kind.unwrap_or_else(|| match resolved.as_ref().map(|role| role.kind) {
            Some(agmen_roles::Kind::Coordinator) => agmen_proto::IdentityKind::Coordinator,
            Some(agmen_roles::Kind::Listener) => agmen_proto::IdentityKind::Listener,
            _ => agmen_proto::IdentityKind::Agent,
        });
        let spec = resolved
            .as_ref()
            .map(|role| {
                Ok::<_, ApiError>(agmen_proto::RoleSpec {
                    role: role
                        .role
                        .name
                        .parse()
                        .map_err(|error| bad_request(format!("{error}")))?,
                    verbs: role.verbs.clone(),
                    edits: role.edits.clone().into(),
                    role_sha256: role
                        .sha256
                        .parse()
                        .map_err(|error| bad_request(format!("{error}")))?,
                })
            })
            .transpose()?;
        let minted = local
            .mint(crate::record::admin::LocalMint {
                project,
                name,
                kind,
                spec: spec.clone(),
                agent: None,
                delegated_by: None,
            })
            .await?;
        let identity = IdentityFile {
            tenant: minted.topics.tenant().clone(),
            project: minted.topics.project().clone(),
            identity_id: minted.id,
            handle: minted.handle,
            owner_id: login.human_id,
            hub_url: agmen_client::local_hub_url(),
            broker_url: agmen_client::local_broker(),
            role: spec.as_ref().map(|spec| spec.role.clone()),
            role_sha256: spec.map(|spec| spec.role_sha256),
            verbs: minted.verbs,
        };
        let row = ManagedIdentity {
            hub_url: identity.hub_url.clone(),
            broker_url: identity.broker_url.clone(),
            tenant: identity.tenant.clone(),
            project: identity.project.clone(),
            identity_id: identity.identity_id.clone(),
            owner_id: identity.owner_id.clone(),
            handle: identity.handle.clone(),
            kind,
        };
        if let Err(error) = self.register(row).await {
            // The mint is committed: revoke it so a retry can use the same name.
            if let Err(revoke) = local.revoke(&identity.project, &identity.identity_id).await {
                tracing::warn!(code = ?revoke.code, "revoking an unregistered local identity failed");
            }
            return Err(error);
        }
        serde_json::to_value(identity).map_err(|_| hub_error("encoding local identity failed"))
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
        let origin = self.read_origin(caller, &project).await?;
        let identity = match caller {
            Caller::Human => self.origins.person(origin).await?.human_id,
            Caller::Identity(identity) => identity.identity_id.clone(),
        };
        let page = tokio::time::timeout(DEADLINE, async {
            match origin {
                crate::origin::Origin::Local => {
                    self.origins
                        .local()
                        .page(&project, &Stream::Inbox(identity), after, limit)
                        .await
                }
                crate::origin::Origin::Hub => self
                    .hub
                    .inbox(&project, &identity, after, limit)
                    .await
                    .map_err(Into::into),
            }
        })
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
        tokio::time::timeout(DEADLINE, async {
            match self.read_origin(caller, &project).await? {
                crate::origin::Origin::Local => self.origins.local().state(&project, &key).await,
                crate::origin::Origin::Hub => {
                    self.hub.state(&project, &key).await.map_err(Into::into)
                }
            }
        })
        .await
        .map_err(|_| hub_error("state read timed out"))?
    }

    /// Where `caller` reads `project`. A project routing cannot place answers
    /// that error, since the hub must not be asked in its place.
    async fn read_origin(
        &self,
        caller: &Caller,
        project: &ProjectId,
    ) -> Result<crate::origin::Origin, ApiError> {
        match caller {
            Caller::Identity(identity) if agmen_client::is_local(&identity.hub_url) => {
                Ok(crate::origin::Origin::Local)
            }
            Caller::Identity(_) => Ok(crate::origin::Origin::Hub),
            Caller::Human => self.origins.of(project).await,
        }
    }

    pub(crate) async fn read_local(
        &self,
        caller: &ManagedIdentity,
        method: &str,
        raw: Value,
    ) -> Result<Value, ApiError> {
        if !agmen_client::is_local(&caller.hub_url) {
            return Err(bad_request("hub identities read this from their hub"));
        }
        let local = self.origins.local();
        let project = &caller.project;
        match method {
            "page" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Page {
                    stream: Stream,
                    #[serde(default)]
                    after: u64,
                    #[serde(default = "page_limit")]
                    limit: u32,
                }
                let Page {
                    stream,
                    after,
                    limit,
                } = params(raw)?;
                if stream != Stream::Feed && stream != Stream::Inbox(caller.identity_id.clone()) {
                    return Err(forbidden());
                }
                let page = local.page(project, &stream, after, limit).await?;
                return Ok(
                    json!({"messages": page.messages, "next_after": page.next_after,
                    "head": page.head, "pruned_through": page.pruned_through}),
                );
            }
            "lanes" => {
                params::<Empty>(raw)?;
                serde_json::to_value(local.lanes(project).await?)
            }
            "items" => {
                params::<Empty>(raw)?;
                serde_json::to_value(local.items(project).await?)
            }
            "check_path" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Check {
                    paths: Vec<String>,
                }
                let Check { paths } = params(raw)?;
                let checks = local
                    .check_path(project, &caller.identity_id, paths)
                    .await?;
                return Ok(json!({"checks": checks}));
            }
            _ => return Err(bad_request("unknown local identity read")),
        }
        .map_err(|_| hub_error("encoding local identity read failed"))
    }

    pub(crate) async fn pending(&self, caller: &ManagedIdentity) -> Result<Value, ApiError> {
        if agmen_client::is_local(&caller.hub_url) {
            self.project_channel(&caller.project).await?;
        }
        let origin = agmen_client::origin_key(&caller.hub_url);
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

    /// The listener reserves channel capacity before committing this drain.
    pub(crate) async fn listener_pending(
        &self,
        caller: &ManagedIdentity,
        limit: u32,
        byte_budget: usize,
    ) -> Result<Vec<StampedEnvelope>, ApiError> {
        // The pump marks a listener failed on any error here, so a failed catch-up
        // is logged and retried on the next drain instead of returned.
        if agmen_client::is_local(&caller.hub_url) {
            let caught_up = self.project_channel(&caller.project).await;
            self.log_catch_up(&caller.project, caught_up.err().map(|error| error.code));
        }
        let origin = agmen_client::origin_key(&caller.hub_url);
        let identity = caller.identity_id.clone();
        store_work(self.store.clone(), move |store| {
            store.drain(&origin, &identity, limit, byte_budget)
        })
        .await
    }

    /// The pump retries every second, so a failure that stays warns when it first
    /// appears or its code changes and logs at debug after that. A success resets it.
    fn log_catch_up(&self, project: &ProjectId, failure: Option<ErrorCode>) {
        let mut failing = self
            .catch_up_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(code) = failure else {
            failing.remove(project);
            return;
        };
        if failing.insert(project.clone(), code) == Some(code) {
            tracing::debug!(code = ?code, "listener collector catch-up failed again");
        } else {
            tracing::warn!(code = ?code, "listener collector catch-up failed");
        }
    }

    /// Keeps a prompt below operating systems' per-argument size limit.
    pub(crate) async fn launch_pending(
        &self,
        caller: &ManagedIdentity,
    ) -> Result<Vec<StampedEnvelope>, ApiError> {
        if agmen_client::is_local(&caller.hub_url) {
            self.project_channel(&caller.project).await?;
        }
        let origin = agmen_client::origin_key(&caller.hub_url);
        let identity = caller.clone();
        let rows = store_work(self.store.clone(), move |store| {
            store.drain_launch(&origin, &identity)
        })
        .await?;
        if let Err(error) = self.project_channel(&caller.project).await {
            tracing::warn!(code=?error.code,"launcher collector restart failed");
        }
        Ok(rows)
    }

    pub(crate) async fn project_channel(
        &self,
        project: &ProjectId,
    ) -> Result<broadcast::Sender<WatchEvent>, ApiError> {
        let origin = self.origins.of(project).await?;
        let login = self.origins.person(origin).await?;
        let key = (
            agmen_client::origin_key(&login.hub_url),
            login.human_id.clone(),
            project.clone(),
        );
        let mut collectors = self.collectors.lock().await;
        if let Some(collector) = collectors.get(&key)
            && !collector.task.is_finished()
        {
            return Ok(collector.tx.clone());
        }
        if collectors.len() >= MAX_COLLECTORS && !collectors.contains_key(&key) {
            return Err(hub_error("64 managed projects are already running"));
        }
        let tx = collectors.get(&key).map_or_else(
            || broadcast::channel(WATCH_CAPACITY).0,
            |collector| collector.tx.clone(),
        );
        if let Some(collector) = collectors.get_mut(&key) {
            finish_task(&mut collector.task, tokio::time::Instant::now() + DEADLINE).await;
        }
        let context = Collection {
            hub: self.hub.clone(),
            origins: self.origins.clone(),
            store: self.store.clone(),
            origin: key.0.clone(),
            owner: key.1.clone(),
            project: project.clone(),
            tx: tx.clone(),
        };
        if origin == crate::origin::Origin::Local {
            // Complete the first catch-up before a pending call can drain.
            context
                .local_wake(&mut VecDeque::new(), MAX_COLLECTION_PAGES)
                .await?;
        }
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
        match self.origins.local().projects().await {
            Ok(projects) => {
                for (project, _) in projects.into_iter().take(MAX_COLLECTORS) {
                    if let Err(error) = self.project_channel(&project).await {
                        tracing::warn!(code = ?error.code, "starting local collector failed");
                    }
                }
            }
            // A person who only uses a hub has no local.db, as LocalOrigin::start allows.
            Err(error) if error.code == ErrorCode::NotFound => {}
            Err(error) => tracing::warn!(code = ?error.code, "reading local projects failed"),
        }
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tick.tick().await;
            let active = tokio::time::timeout(DEADLINE, async {
                let login = self
                    .credentials
                    .login()
                    .await
                    .map_err(crate::hub::HubError::from)?;
                let origin = agmen_client::origin_key(&login.hub_url);
                let rows = self.managed(origin.clone(), login.human_id.clone()).await?;
                Ok::<_, ApiError>((origin, login.human_id, rows))
            })
            .await;
            let (origin, owner, rows) = match active {
                Ok(Ok((origin, owner, rows))) => (origin, Some(owner), rows),
                _ => (String::new(), None, Vec::new()),
            };
            let mut collectors = self.collectors.lock().await;
            let stale: Vec<_> = collectors
                .keys()
                .filter(|(o, h, p)| {
                    o != agmen_client::LOCAL_ORIGIN
                        && (*o != origin
                            || Some(h) != owner.as_ref()
                            || (!rows.iter().any(|row| row.project == *p)
                                && collectors
                                    .get(&((*o).clone(), (*h).clone(), (*p).clone()))
                                    .is_none_or(|collector| collector.tx.receiver_count() == 0)))
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
                .filter(|(o, id)| {
                    o != agmen_client::LOCAL_ORIGIN
                        && (*o != origin || !rows.iter().any(|row| &row.identity_id == id))
                })
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
            if let Ok(local) = self.origins.person(crate::origin::Origin::Local).await {
                self.flush_queue_full_alerts(agmen_client::LOCAL_ORIGIN, &local.human_id)
                    .await;
            }
            if let Some(owner) = &owner {
                self.flush_queue_full_alerts(&origin, owner).await;
            }
            for row in rows {
                // A finished collector stays offline until an explicit call.
                let key = (origin.clone(), row.owner_id.clone(), row.project.clone());
                if !self.collectors.lock().await.contains_key(&key)
                    && let Err(error) = self.project_channel(&row.project).await
                {
                    tracing::warn!(code = ?error.code,"starting collector failed");
                }
            }
        }
    }

    async fn flush_queue_full_alerts(&self, origin: &str, owner: &IdentityId) {
        let origin = origin.to_owned();
        let owner = owner.clone();
        // The existing envelope builder reads random bytes synchronously.
        let alerts = store_work(self.store.clone(), move |store| {
            let mut alerts = Vec::new();
            for identity in store.pending_queue_full_alerts(&origin, &owner)? {
                let proposed = crate::listener::Listeners::queue_full_alert(&identity)
                    .map_err(|_| crate::state::StoreError::Invalid("full queue ALERT envelope"))?;
                if let Some(alert) =
                    store.prepare_queue_full_alert(&origin, &identity.identity_id, &proposed)?
                {
                    alerts.push((identity, alert));
                }
            }
            Ok(alerts)
        })
        .await;
        let alerts = match alerts {
            Ok(alerts) => alerts,
            Err(error) => {
                tracing::warn!(code=?error.code, "reading full queue ALERTs failed");
                return;
            }
        };
        for (identity, alert) in alerts.into_iter().take(MAX_MANAGED) {
            // As with the listener cap alert, retry the same id so an ambiguous
            // enqueue outcome cannot publish a second notification for the fill.
            for _ in 0..MAX_QUEUE_ALERT_ATTEMPTS {
                match self.post(&identity, alert.clone()).await {
                    Ok(_) => {
                        let origin = agmen_client::origin_key(&identity.hub_url);
                        let id = identity.identity_id.clone();
                        let message = alert.id.clone();
                        if let Err(error) = store_work(self.store.clone(), move |store| {
                            store.acknowledge_queue_full_alert(&origin, &id, &message)
                        })
                        .await
                        {
                            tracing::warn!(identity=%identity.identity_id, code=?error.code, "acknowledging full queue ALERT failed");
                        }
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(identity=%identity.identity_id, code=?error.code, "full queue ALERT could not be queued")
                    }
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
        self.store.shutdown().await;
        self.origins.shutdown().await;
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
        let watch_params: ProjectParams = params(raw)?;
        let project = watch_params.project.clone();
        let origin = self.origins.of(&project).await?;
        let login = self.origins.person(origin).await?;
        let _presence = match (origin, caller) {
            (crate::origin::Origin::Local, Caller::Identity(who)) => Some(
                self.origins
                    .local()
                    .presence(&project, &who.identity_id)
                    .await?,
            ),
            _ => None,
        };
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
        if origin == crate::origin::Origin::Local {
            return self
                .watch_local(caller, id, watch_params, &me, reader, writer)
                .await;
        }
        if watch_params.after.is_some() {
            return Err(bad_request(
                "watch after is only supported for local projects",
            ));
        }
        let tx = self.project_channel(&project).await?;
        let mut events = tx.subscribe();
        write_event(writer, id, &ClientEvent::Me(me.clone())).await?;
        let mut disconnected = [0; 1];
        let mut authority = tokio::time::interval(AUTHORITY_RECHECK);
        loop {
            let (target, event) = tokio::select! {
                _ = reader.read(&mut disconnected) => return Ok(()),
                event = events.recv() => event.map_err(|_|hub_error("watch queue overflowed or closed"))?,
                _ = authority.tick() => {
                    let valid = tokio::time::timeout(DEADLINE,async {
                        match caller {
                            Caller::Identity(identity) => still_managed(&self.origins,self.store.clone(),identity).await,
                            Caller::Human => self.origins.person_for(&login.hub_url).await.is_ok_and(|current| current.human_id == me.identity_id && current.hub_url == login.hub_url),
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
        if agmen_client::is_local(&caller.hub_url) {
            let reply = tokio::time::timeout(
                DEADLINE,
                self.origins
                    .local()
                    .ingest(&caller.project, &caller.identity_id, &envelope),
            )
            .await
            .map_err(|_| hub_error("identity post timed out"))??;
            return match reply {
                agmen_proto::DeliveryReply::Ack { stream, seq, .. } => {
                    Ok(json!({"stream": stream, "seq": seq}))
                }
                agmen_proto::DeliveryReply::Nack { reason, .. } => Err(hub_error(&format!(
                    "hub rejected identity message: {reason}"
                ))),
            };
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
            agmen_client::origin_key(&caller.hub_url),
            caller.identity_id.clone(),
        );
        let slot = {
            let mut runtimes = self.runtimes.lock().await;
            if !runtimes.contains_key(&key) && runtimes.len() >= MAX_MANAGED {
                return Err(hub_error(&format!(
                    "{MAX_MANAGED} identity sessions are already running"
                )));
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
            agmen_client::origin_key(&caller.hub_url),
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
        if !still_managed(&self.origins, self.store.clone(), caller).await {
            return Err(unauthorized());
        }
        let tx = self.project_channel(&caller.project).await?;
        *runtime = Some(Runtime::start(
            session.clone(),
            events,
            self.origins.clone(),
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
        origins: Arc<crate::origin::Origins>,
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
                                let origin = agmen_client::origin_key(&identity.hub_url);
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
                        let valid = tokio::time::timeout(DEADLINE, still_managed(&origins, store.clone(), &identity)).await;
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
    write_watch_frame(writer, &response).await
}

async fn write_watch_frame(
    writer: &mut OwnedWriteHalf,
    response: &agmen_local::Response,
) -> Result<(), ApiError> {
    tokio::time::timeout(DEADLINE, agmen_local::frame::write_async(writer, response))
        .await
        .map_err(|_| hub_error("watch write timed out"))?
        .map_err(|_| hub_error("watch write failed"))
}

pub(crate) async fn store_work<T: Send + 'static>(
    database: Arc<Database>,
    work: impl FnOnce(&mut Store) -> Result<T, crate::state::StoreError> + Send + 'static,
) -> Result<T, ApiError> {
    let store = database.store.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = Cancellation(cancelled.clone());
    let (send, receive) = oneshot::channel();
    let mut jobs = database.jobs.lock().await;
    while jobs.try_join_next().is_some() {}
    if jobs.len() >= MAX_DATABASE_JOBS {
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
    origins: Arc<crate::origin::Origins>,
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
        let hub_url = if self.origin == agmen_client::LOCAL_ORIGIN {
            agmen_client::local_hub_url()
        } else {
            Url::parse(&self.origin).map_err(|_| unauthorized())?
        };
        let login = self.origins.person_for(&hub_url).await?;
        if login.human_id != self.owner || agmen_client::origin_key(&login.hub_url) != self.origin {
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
        if self.origin == agmen_client::LOCAL_ORIGIN {
            if let Err(error) = self.run_local().await {
                self.send(Err(error));
            }
            return;
        }
        let mut warned_limit = false;
        for attempt in 0..=MAX_HUB_RECONNECTS {
            if attempt > 0 {
                tokio::time::sleep(
                    Duration::from_secs(1_u64 << (attempt - 1).min(HUB_BACKOFF_SHIFT))
                        .min(HUB_MAX_BACKOFF),
                )
                .await;
            }
            let positions = match self.positions().await {
                Ok(p) => p,
                Err(error) => {
                    self.send(Err(error));
                    return;
                }
            };
            if let Err(error) = self.hub_pass(positions, &mut warned_limit).await {
                self.send(Err(error));
                return;
            }
            self.send(Ok(ClientEvent::Connection { connected: false }));
        }
        self.send(Err(hub_error("collector offline after 10 reconnects")));
    }

    async fn hub_pass(
        &self,
        positions: BTreeMap<Stream, u64>,
        warned_limit: &mut bool,
    ) -> Result<(), ApiError> {
        let mut chunks = vec![BTreeMap::new()];
        for (n, (stream, seq)) in positions.iter().take(MAX_STREAM_POSITIONS).enumerate() {
            let index = n / MAX_HUB_AFTER;
            if index == chunks.len() {
                chunks.push(BTreeMap::new());
            }
            if let Some(chunk) = chunks.get_mut(index) {
                chunk.insert(stream.clone(), *seq);
            }
        }
        let mut responses = Vec::with_capacity(MAX_HUB_CONNECTIONS);
        for chunk in chunks.iter().take(MAX_HUB_CONNECTIONS) {
            // The hub replays from seq 0 any stream `after` omits. Name other
            // streams too, up to its limit, so a short chunk does not make the
            // hub resend their whole history on every reconnect.
            let mut after = chunk.clone();
            for (stream, seq) in positions.iter().take(MAX_STREAM_POSITIONS) {
                if after.len() >= MAX_HUB_AFTER {
                    break;
                }
                after.entry(stream.clone()).or_insert(*seq);
            }
            match tokio::time::timeout(DEADLINE, self.hub.events(&self.project, &after, None)).await
            {
                Ok(Ok(response)) => responses.push(response),
                Ok(Err(error)) if error.is_final() => return Err(error.into()),
                Ok(Err(crate::hub::HubError::Refused { status, code, .. })) => {
                    if !*warned_limit
                        && (status == 429 || code == "too_many_streams" || code == "hub_busy")
                    {
                        // The hub's error text is its own: log the status only.
                        tracing::warn!(project = %self.project, status,
                            "hub events connection refused; hub limit is 8 streams per person (128 total); retrying");
                        *warned_limit = true;
                    }
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }
        let (tx, mut rx) = mpsc::channel(HUB_EVENT_QUEUE);
        let mut readers = tokio::task::JoinSet::new();
        // The hub sends all readable streams on every connection, including
        // ones absent from `after`. Assign each stream to one reader. The first
        // reader also covers streams created since the last reconnect.
        let secondary: BTreeSet<_> = chunks
            .iter()
            .skip(1)
            .flat_map(|chunk| chunk.keys().cloned())
            .collect();
        for (index, response) in responses.into_iter().enumerate().take(MAX_HUB_CONNECTIONS) {
            let tx = tx.clone();
            let assigned = if index == 0 {
                secondary.clone()
            } else {
                chunks
                    .get(index)
                    .map(|chunk| chunk.keys().cloned().collect())
                    .unwrap_or_default()
            };
            readers.spawn(read_hub_events(response, tx, assigned, index == 0));
        }
        drop(tx);
        self.send(Ok(ClientEvent::Connection { connected: true }));
        // Readers only parse; one consumer performs position checks and ingest
        // transactions, preserving order and suppressing duplicates on replay.
        loop {
            tokio::select! {
                event = rx.recv() => match event {
                    Some(message) => self.accept(message).await?,
                    None => return Ok(()),
                },
                ended = readers.join_next() => return match ended {
                    Some(Ok(result)) => result,
                    Some(Err(_)) => Err(hub_error("hub events reader failed")),
                    None => Ok(()),
                },
            }
        }
    }

    async fn run_local(&self) -> Result<(), ApiError> {
        let local = self.origins.local();
        let mut stored = local.watch_stored(&self.project).await?;
        let mut streams = VecDeque::new();
        self.send(Ok(ClientEvent::Connection { connected: true }));
        loop {
            // Subscribe before paging so commits during replay remain visible.
            stored.borrow_and_update();
            self.local_wake(&mut streams, MAX_COLLECTION_PAGES).await?;
            if let Ok(Err(_)) = tokio::time::timeout(LOCAL_RECHECK, stored.changed()).await {
                return Err(hub_error("local record notification channel closed"));
            }
        }
    }

    async fn local_wake(
        &self,
        streams: &mut VecDeque<Stream>,
        max_pages: usize,
    ) -> Result<(), ApiError> {
        let origin = self.origin.clone();
        let owner = self.owner.clone();
        let identities = store_work(self.store.clone(), move |store| {
            store.identities(&origin, &owner)
        })
        .await?;
        let mut active = Vec::with_capacity(MAX_STREAM_POSITIONS);
        active.extend([Stream::Feed, Stream::Inbox(self.owner.clone())]);
        active.extend(
            identities
                .into_iter()
                .take(MAX_MANAGED)
                .filter(|identity| identity.project == self.project)
                .map(|identity| Stream::Inbox(identity.identity_id)),
        );
        // Keep unfinished streams first across wakes, even when there are more
        // inboxes than the page budget. Refresh registration on every wake.
        streams.retain(|stream| active.contains(stream));
        for stream in active.into_iter().take(MAX_STREAM_POSITIONS) {
            if !streams.contains(&stream) {
                streams.push_back(stream);
            }
        }
        let mut positions = self.positions().await?;
        for _ in 0..max_pages.min(MAX_COLLECTION_PAGES) {
            let Some(stream) = streams.pop_front() else {
                break;
            };
            let after = positions.get(&stream).copied().unwrap_or(0);
            let page = self
                .origins
                .local()
                .page(&self.project, &stream, after, COLLECTION_PAGE_SIZE)
                .await?;
            for row in page
                .messages
                .into_iter()
                .take(COLLECTION_PAGE_SIZE as usize)
            {
                let expected = positions
                    .get(&stream)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1);
                if row.seq.0 != expected || row.stream != stream {
                    return Err(hub_error("noncontiguous local replay"));
                }
                let seq = row.seq.0;
                self.commit(row).await?;
                positions.insert(stream.clone(), seq);
            }
            if page.next_after.is_some() {
                streams.push_back(stream);
            }
        }
        Ok(())
    }
}

async fn read_hub_events(
    mut response: reqwest::Response,
    tx: mpsc::Sender<StampedEnvelope>,
    assigned: BTreeSet<Stream>,
    first: bool,
) -> Result<(), ApiError> {
    let mut parser = crate::sse::SseParser::default();
    while let Ok(Ok(Some(chunk))) = tokio::time::timeout(HUB_READ_WAIT, response.chunk()).await {
        for part in chunk.chunks(HUB_PARSE_BYTES) {
            let events = parser
                .push(part)
                .map_err(|_| hub_error("invalid SSE stream"))?;
            for event in events {
                let message: StampedEnvelope = serde_json::from_str(&event.data)
                    .map_err(|_| hub_error("invalid SSE envelope"))?;
                if assigned.contains(&message.stream) != first
                    && !matches!(
                        tokio::time::timeout(DEADLINE, tx.send(message)).await,
                        Ok(Ok(()))
                    )
                {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Whether `identity` is still managed. A read that fails counts as not
/// managed, so authority is not retained after the daemon can no longer verify
/// its managed identity.
async fn still_managed(
    origins: &crate::origin::Origins,
    store: Arc<Database>,
    identity: &ManagedIdentity,
) -> bool {
    matches!(check_managed(origins, store, identity).await, Ok(true))
}

/// Whether `identity` is still managed: `Ok(false)` when it is revoked or gone.
/// A read that failed says nothing against it, and is returned as it failed;
/// `Identities::read_failed` gives the answer a local client hears.
async fn check_managed(
    origins: &crate::origin::Origins,
    store: Arc<Database>,
    identity: &ManagedIdentity,
) -> Result<bool, ApiError> {
    let login = match origins.person_for(&identity.hub_url).await {
        Ok(login) => login,
        Err(error) => return unconfirmed(error),
    };
    let origin = agmen_client::origin_key(&identity.hub_url);
    if login.human_id != identity.owner_id || agmen_client::origin_key(&login.hub_url) != origin {
        return Ok(false);
    }
    let expected = identity.clone();
    store_work(store, move |s| {
        s.identities(&origin, &expected.owner_id)
            .map(|rows| rows.contains(&expected))
    })
    .await
}

/// What a failed read of the person means for a recheck. A person who is not on
/// this machine is gone. Anything else is a read that failed, returned as it
/// failed (see `check_managed`).
fn unconfirmed(error: ApiError) -> Result<bool, ApiError> {
    match error.code {
        ErrorCode::NotFound => Ok(false),
        _ => Err(error),
    }
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
mod local_collection_tests {
    use super::*;
    use agmen_proto::{IdentityKind, Payload, Timestamp};

    // Exercise the production wake with two pages, keeping the real 500-row page size.
    const TEST_COLLECTION_PAGES: usize = 2;
    const TEST_DM_COUNT: usize = COLLECTION_PAGE_SIZE as usize + 1;
    const TEST_DRAIN_WAKES: usize = 4;
    const TEST_WAIT: Duration = Duration::from_secs(30);
    const COLLECT_WAIT: Duration = Duration::from_secs(5);
    const COLLECT_POLL: Duration = Duration::from_millis(20);
    const SUPERVISE_PROBE: Duration = Duration::from_millis(300);
    const AGENTS: [&str; 2] = ["first", "second"];

    #[derive(Clone, Default)]
    pub(super) struct LogBytes(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogBytes {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBytes {
        type Writer = Self;
        fn make_writer(&'a self) -> Self {
            self.clone()
        }
    }

    impl LogBytes {
        /// Captures this thread's output from debug up until the guard drops.
        pub(super) fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
            let logs = Self::default();
            let guard = tracing::subscriber::set_default(
                tracing_subscriber::fmt()
                    .without_time()
                    .with_ansi(false)
                    .with_max_level(tracing::Level::DEBUG)
                    .with_writer(logs.clone())
                    .finish(),
            );
            (logs, guard)
        }

        /// Everything written so far.
        pub(super) fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        /// Each listener catch-up line as "LEVEL Code", in the order written.
        fn catch_ups(&self) -> Vec<String> {
            self.text()
                .lines()
                .filter(|line| line.contains("listener collector catch-up failed"))
                .map(|line| {
                    let level = line.split_whitespace().next().unwrap();
                    let code = line.rsplit("code=").next().unwrap();
                    format!("{level} {code}")
                })
                .collect()
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        daemon: crate::Agmend,
        collection: Collection,
        agents: Vec<IdentityId>,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let daemon = crate::Agmend::bind(crate::Config::new(
                dir.path().join("agmend.sock"),
                dir.path().join("login.json"),
                agmen_client::TokenStore::Dir(dir.path().join("tokens")),
            ))
            .unwrap();
            let local = daemon.core.origins.local();
            let topics = local.create_project("collection", "andy").await.unwrap();
            let person = local.person().await.unwrap().unwrap();
            let mut agents = Vec::new();
            for name in AGENTS {
                let minted = local
                    .mint(crate::record::admin::LocalMint {
                        project: topics.project().clone(),
                        name: name.into(),
                        kind: IdentityKind::Agent,
                        spec: None,
                        agent: None,
                        delegated_by: None,
                    })
                    .await
                    .unwrap();
                let identity = ManagedIdentity {
                    hub_url: agmen_client::local_hub_url(),
                    broker_url: agmen_client::LOCAL_BROKER_URL.parse().unwrap(),
                    tenant: topics.tenant().clone(),
                    project: topics.project().clone(),
                    identity_id: minted.id.clone(),
                    owner_id: person.id.clone(),
                    handle: minted.handle,
                    kind: IdentityKind::Agent,
                };
                store_work(daemon.core.identities.store.clone(), move |store| {
                    store.register(&identity)
                })
                .await
                .unwrap();
                agents.push(minted.id);
            }
            // Use the same stream order as Store::identities, regardless of minted ids.
            agents.sort();
            let collection = Collection {
                hub: daemon.core.origins.hub().clone(),
                origins: daemon.core.origins.clone(),
                store: daemon.core.identities.store.clone(),
                origin: agmen_client::LOCAL_ORIGIN.into(),
                owner: person.id,
                project: topics.project().clone(),
                tx: broadcast::channel(WATCH_CAPACITY).0,
            };
            Self {
                _dir: dir,
                daemon,
                collection,
                agents,
            }
        }

        async fn post(&self, to: Option<&IdentityId>) -> ClientEnvelope {
            let now = Timestamp::now();
            let mut envelope: ClientEnvelope = serde_json::from_value(json!({
                "v":1, "id":agmen_core::new_message_id(now).unwrap(),
                "verb":"FYI", "data":{}, "body":"collection test", "sent_at":now
            }))
            .unwrap();
            if let Some(to) = to {
                envelope.payload = Payload::Dm {};
                envelope.to.push(to.clone());
            }
            self.collection
                .origins
                .local()
                .post_as_person(&self.collection.project, &envelope)
                .await
                .unwrap();
            envelope
        }

        async fn drain(&self, id: &IdentityId) -> Vec<StampedEnvelope> {
            let id = id.clone();
            store_work(self.collection.store.clone(), move |store| {
                store.drain(
                    agmen_client::LOCAL_ORIGIN,
                    &id,
                    COLLECTION_PAGE_SIZE,
                    agmen_local::MAX_FRAME as usize - 4096,
                )
            })
            .await
            .unwrap()
        }

        /// The registered row a pending call authorizes as.
        async fn caller(&self, id: &IdentityId) -> ManagedIdentity {
            self.daemon
                .core
                .identities
                .managed(
                    agmen_client::LOCAL_ORIGIN.into(),
                    self.collection.owner.clone(),
                )
                .await
                .unwrap()
                .into_iter()
                .find(|row| row.identity_id == *id)
                .unwrap()
        }

        /// A listener drain of one row, which errs only when the store does.
        async fn listener_pending(&self, caller: &ManagedIdentity) -> Vec<StampedEnvelope> {
            self.daemon
                .core
                .identities
                .listener_pending(caller, 1, agmen_local::MAX_FRAME as usize - 4096)
                .await
                .unwrap()
        }

        async fn execute_state(&self, sql: String) {
            let path = self._dir.path().join("agmend.db");
            tokio::task::spawn_blocking(move || {
                rusqlite::Connection::open(path)
                    .unwrap()
                    .execute_batch(&sql)
                    .unwrap();
            })
            .await
            .unwrap();
        }

        async fn fill_dm_history(&self) {
            let path = self._dir.path().join("local.db");
            let project = self.collection.project.clone();
            let streams = [
                Stream::Inbox(self.collection.owner.clone()),
                Stream::Inbox(self.agents[0].clone()),
            ];
            // Extend real DM copies directly so history setup does not exercise
            // the sender's 10 messages per second rate limit.
            tokio::task::spawn_blocking(move || {
                let mut conn = rusqlite::Connection::open(path).unwrap();
                let tx = conn.transaction().unwrap();
                let raw: String = tx
                    .query_row(
                        "SELECT envelope FROM messages WHERE project_id=?1 AND stream=?2 AND seq=1",
                        rusqlite::params![project.as_str(), streams[0].to_string()],
                        |row| row.get(0),
                    )
                    .unwrap();
                let mut message: StampedEnvelope = serde_json::from_str(&raw).unwrap();
                for seq in 2..=TEST_DM_COUNT {
                    message.client.id = agmen_core::new_message_id(Timestamp::now()).unwrap();
                    message.seq = agmen_proto::Seq(seq as u64);
                    for stream in &streams {
                        message.stream = stream.clone();
                        tx.execute(
                            "INSERT INTO messages
                             SELECT project_id,stream,?3,?4,from_id,payload_hash,?5,hub_at,ack_row
                             FROM messages WHERE project_id=?1 AND stream=?2 AND seq=1",
                            rusqlite::params![
                                project.as_str(),
                                stream.to_string(),
                                seq as i64,
                                message.client.id.as_str(),
                                serde_json::to_string(&message).unwrap()
                            ],
                        )
                        .unwrap();
                    }
                }
                for stream in streams {
                    tx.execute(
                        "UPDATE stream_heads SET seq=?2 WHERE stream=?1",
                        rusqlite::params![format!("{project}/{stream}"), TEST_DM_COUNT as i64],
                    )
                    .unwrap();
                }
                tx.commit().unwrap();
            })
            .await
            .unwrap();
        }

        async fn stop(self) {
            self.daemon.core.identities.shutdown().await;
            self.daemon.core.origins.local().shutdown().await;
        }
    }

    #[tokio::test]
    async fn local_collection_shares_page_budget_and_resumes_every_stream() {
        tokio::time::timeout(TEST_WAIT, async {
            let f = Fixture::new().await;
            f.post(None).await;
            f.post(Some(&f.agents[0])).await;
            f.fill_dm_history().await;
            f.post(Some(&f.agents[1])).await;
            let owner = Stream::Inbox(f.collection.owner.clone());
            let first = Stream::Inbox(f.agents[0].clone());
            let second = Stream::Inbox(f.agents[1].clone());
            let mut streams = VecDeque::new();
            f.collection
                .local_wake(&mut streams, TEST_COLLECTION_PAGES)
                .await
                .unwrap();
            let positions = f.collection.positions().await.unwrap();
            assert_eq!(positions.len(), TEST_COLLECTION_PAGES);
            assert_eq!(positions[&Stream::Feed], 1);
            assert_eq!(positions[&owner], u64::from(COLLECTION_PAGE_SIZE));
            assert!(!positions.contains_key(&first));
            assert!(!positions.contains_key(&second));

            f.collection
                .local_wake(&mut streams, TEST_COLLECTION_PAGES)
                .await
                .unwrap();
            let positions = f.collection.positions().await.unwrap();
            assert_eq!(positions[&first], u64::from(COLLECTION_PAGE_SIZE));
            assert_eq!(positions[&second], 1);
            assert_eq!(positions[&owner], u64::from(COLLECTION_PAGE_SIZE));
            assert_eq!(positions[&Stream::Feed], 1);

            for _ in 0..TEST_DRAIN_WAKES {
                f.collection
                    .local_wake(&mut streams, TEST_COLLECTION_PAGES)
                    .await
                    .unwrap();
            }
            let positions = f.collection.positions().await.unwrap();
            assert_eq!(positions[&owner], (TEST_DM_COUNT + 1) as u64);
            assert_eq!(positions[&first], TEST_DM_COUNT as u64);
            assert_eq!(positions[&second], 1);
            let mut rows = f.drain(&f.agents[0]).await;
            rows.extend(f.drain(&f.agents[0]).await);
            assert_eq!(rows.len(), TEST_DM_COUNT);
            assert_eq!(rows.first().unwrap().seq.0, 1);
            assert_eq!(rows.last().unwrap().seq.0, TEST_DM_COUNT as u64);
            assert!(f.drain(&f.agents[0]).await.is_empty());
            assert_eq!(f.drain(&f.agents[1]).await.len(), 1);
            f.stop().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn local_inbox_position_and_pending_commit_together() {
        tokio::time::timeout(TEST_WAIT, async {
            let f = Fixture::new().await;
            let dm = f.post(Some(&f.agents[0])).await;
            let inbox = Stream::Inbox(f.agents[0].clone());
            f.execute_state(format!(
                "CREATE TRIGGER refuse_position BEFORE INSERT ON positions
                 WHEN NEW.stream='{inbox}'
                 BEGIN SELECT RAISE(ABORT, 'test position failure'); END;"
            ))
            .await;
            let mut streams = VecDeque::new();
            let error = f
                .collection
                .local_wake(&mut streams, MAX_COLLECTION_PAGES)
                .await
                .unwrap_err();
            assert_eq!(error, hub_error("daemon state operation failed"));
            assert!(f.drain(&f.agents[0]).await.is_empty());
            assert!(!f.collection.positions().await.unwrap().contains_key(&inbox));

            f.execute_state("DROP TRIGGER refuse_position".into()).await;
            f.collection
                .local_wake(&mut streams, MAX_COLLECTION_PAGES)
                .await
                .unwrap();
            assert_eq!(f.collection.positions().await.unwrap()[&inbox], 1);
            let rows = f.drain(&f.agents[0]).await;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].client.id, dm.id);
            f.collection
                .local_wake(&mut streams, MAX_COLLECTION_PAGES)
                .await
                .unwrap();
            assert!(f.drain(&f.agents[0]).await.is_empty());
            f.stop().await;
        })
        .await
        .unwrap();
    }

    // The next three run no supervise task and no collector: a recorded row reaches
    // the caller only if the call itself finishes the catch-up before it drains.
    #[tokio::test]
    async fn pending_of_a_local_identity_collects_recorded_rows_before_it_drains() {
        tokio::time::timeout(TEST_WAIT, async {
            let f = Fixture::new().await;
            let dm = f.post(Some(&f.agents[0])).await;
            let caller = f.caller(&f.agents[0]).await;
            let rows = f.daemon.core.identities.pending(&caller).await.unwrap();
            assert_eq!(rows.as_array().unwrap().len(), 1);
            assert_eq!(rows[0]["id"], json!(dm.id));
            f.stop().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn launch_pending_of_a_local_identity_collects_recorded_rows_before_it_drains() {
        tokio::time::timeout(TEST_WAIT, async {
            let f = Fixture::new().await;
            let dm = f.post(Some(&f.agents[0])).await;
            let caller = f.caller(&f.agents[0]).await;
            let rows = f
                .daemon
                .core
                .identities
                .launch_pending(&caller)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].client.id, dm.id);
            f.stop().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn listener_pending_of_a_local_identity_collects_recorded_rows_before_it_drains() {
        tokio::time::timeout(TEST_WAIT, async {
            let f = Fixture::new().await;
            let dm = f.post(Some(&f.agents[0])).await;
            let caller = f.caller(&f.agents[0]).await;
            let rows = f
                .daemon
                .core
                .identities
                .listener_pending(&caller, 1, agmen_local::MAX_FRAME as usize - 4096)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].client.id, dm.id);
            f.stop().await;
        })
        .await
        .unwrap();
    }

    // The pump marks a listener failed when listener_pending errors, so a catch-up
    // that fails (here the local origin has stopped) must not block the drain.
    #[tokio::test]
    async fn listener_pending_drains_rows_already_pending_when_the_catch_up_fails() {
        tokio::time::timeout(TEST_WAIT, async {
            let f = Fixture::new().await;
            let dm = f.post(Some(&f.agents[0])).await;
            f.collection
                .local_wake(&mut VecDeque::new(), MAX_COLLECTION_PAGES)
                .await
                .unwrap();
            f.daemon.core.origins.local().shutdown().await;
            let caller = f.caller(&f.agents[0]).await;
            let rows = f
                .daemon
                .core
                .identities
                .listener_pending(&caller, 1, agmen_local::MAX_FRAME as usize - 4096)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].client.id, dm.id);
            f.stop().await;
        })
        .await
        .unwrap();
    }

    // The pump drains once a second, so a permanent failure would warn forever.
    #[tokio::test]
    async fn listener_pending_warns_once_while_the_catch_up_keeps_failing() {
        let (logs, _capture) = LogBytes::capture();
        tokio::time::timeout(TEST_WAIT, async {
            let f = Fixture::new().await;
            let caller = f.caller(&f.agents[0]).await;
            f.daemon.core.origins.local().shutdown().await;
            for _ in 0..3 {
                f.listener_pending(&caller).await;
            }
            assert_eq!(logs.catch_ups(), ["WARN Hub", "DEBUG Hub", "DEBUG Hub"]);
            f.stop().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn listener_pending_warns_again_when_the_catch_up_fails_with_a_new_code() {
        let (logs, _capture) = LogBytes::capture();
        tokio::time::timeout(TEST_WAIT, async {
            let f = Fixture::new().await;
            // No local.db record holds this project, so the catch-up reports NotFound.
            // Once agmend.db registers an identity there, local.db counts as unreadable
            // and the catch-up reports Version.
            let stray = ManagedIdentity {
                project: "ghostproject".parse().unwrap(),
                identity_id: "stray0001".parse().unwrap(),
                handle: "andy/stray".into(),
                ..f.caller(&f.agents[0]).await
            };
            for _ in 0..2 {
                f.listener_pending(&stray).await;
            }
            let row = stray.clone();
            store_work(f.daemon.core.identities.store.clone(), move |store| {
                store.register(&row)
            })
            .await
            .unwrap();
            for _ in 0..2 {
                f.listener_pending(&stray).await;
            }
            assert_eq!(
                logs.catch_ups(),
                [
                    "WARN NotFound",
                    "DEBUG NotFound",
                    "WARN Version",
                    "DEBUG Version"
                ]
            );
            f.stop().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn listener_pending_warns_again_after_a_catch_up_succeeds() {
        let (logs, _capture) = LogBytes::capture();
        tokio::time::timeout(TEST_WAIT, async {
            let f = Fixture::new().await;
            let dm = f.post(Some(&f.agents[0])).await;
            let caller = f.caller(&f.agents[0]).await;
            let inbox = Stream::Inbox(f.agents[0].clone());
            f.execute_state(format!(
                "CREATE TRIGGER refuse_position BEFORE INSERT ON positions
                 WHEN NEW.stream='{inbox}'
                 BEGIN SELECT RAISE(ABORT, 'test position failure'); END;"
            ))
            .await;
            assert!(f.listener_pending(&caller).await.is_empty());
            assert!(f.listener_pending(&caller).await.is_empty());
            f.execute_state("DROP TRIGGER refuse_position".into()).await;
            let rows = f.listener_pending(&caller).await;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].client.id, dm.id);
            // Hub again, as before the success, so only the success can make this warn.
            f.daemon.core.origins.local().shutdown().await;
            assert!(f.listener_pending(&caller).await.is_empty());
            assert_eq!(logs.catch_ups(), ["WARN Hub", "DEBUG Hub", "WARN Hub"]);
            f.stop().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn supervise_collects_recorded_local_rows_without_a_pending_call() {
        tokio::time::timeout(TEST_WAIT, async {
            let f = Fixture::new().await;
            f.post(Some(&f.agents[0])).await;
            let identities = f.daemon.core.identities.clone();
            let task = tokio::spawn(async move { identities.supervise().await });
            let inbox = Stream::Inbox(f.agents[0].clone());
            let collected = tokio::time::timeout(COLLECT_WAIT, async {
                while !f.collection.positions().await.unwrap().contains_key(&inbox) {
                    tokio::time::sleep(COLLECT_POLL).await;
                }
            })
            .await;
            task.abort();
            let _aborted = task.await;
            assert!(
                collected.is_ok(),
                "supervise did not collect the recorded row"
            );
            assert_eq!(f.drain(&f.agents[0]).await.len(), 1);
            f.stop().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn supervise_without_a_local_record_logs_no_warning_and_creates_none() {
        let logs = LogBytes::default();
        let _capture = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(logs.clone())
                .finish(),
        );
        let dir = tempfile::tempdir().unwrap();
        let daemon = crate::Agmend::bind(crate::Config::new(
            dir.path().join("agmend.sock"),
            dir.path().join("login.json"),
            agmen_client::TokenStore::Dir(dir.path().join("tokens")),
        ))
        .unwrap();
        // supervise never returns; its first pass runs well inside the probe.
        let ran = tokio::time::timeout(SUPERVISE_PROBE, daemon.core.identities.supervise()).await;
        assert!(ran.is_err());
        assert!(!dir.path().join("local.db").exists());
        let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(!logs.contains("reading local projects failed"), "{logs}");
        daemon.core.identities.shutdown().await;
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
        fn local_key(&self) -> TokenKey {
            TokenKey::new(
                &"https://agmend.invalid".parse().unwrap(),
                &self.who.identity_id,
            )
            .unwrap()
        }

        fn local_path(&self) -> PathBuf {
            let name: String = self
                .local_key()
                .as_str()
                .bytes()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            self._dir.path().join("tokens").join(format!("tok-{name}"))
        }

        fn unregistered() -> Self {
            let fixture = Self::new(None);
            fixture.tokens.delete(&fixture.local_key()).unwrap();
            fixture
                .daemon
                .core
                .identities
                .store
                .store
                .lock()
                .unwrap()
                .unregister(
                    &fixture.who.hub_url.origin().ascii_serialization(),
                    &fixture.who.identity_id,
                )
                .unwrap();
            fixture
        }

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
    async fn local_token_issued_on_register() {
        let fixture = Fixture::unregistered();
        let identities = &fixture.daemon.core.identities;
        identities.register(fixture.who.clone()).await.unwrap();
        let token = identities
            .local_token(&fixture.who.identity_id)
            .await
            .unwrap()
            .unwrap();
        assert!(valid_token(token.expose()));
        let caller = identities
            .authorize(Some(&fixture.who.identity_id), Some(token.expose()))
            .await
            .unwrap();
        assert!(matches!(caller, Caller::Identity(row) if row == fixture.who));
    }

    #[tokio::test]
    async fn wrong_or_missing_token_is_refused() {
        let fixture = Fixture::unregistered();
        let identities = &fixture.daemon.core.identities;
        identities.register(fixture.who.clone()).await.unwrap();
        let token = identities
            .local_token(&fixture.who.identity_id)
            .await
            .unwrap()
            .unwrap();
        let wrong = if token.expose().starts_with('a') {
            "b".repeat(64)
        } else {
            "a".repeat(64)
        };
        for supplied in [Some(wrong.as_str()), None] {
            let refusal = identities
                .authorize(Some(&fixture.who.identity_id), supplied)
                .await
                .unwrap_err();
            assert_eq!(refusal, unauthorized());
        }
    }

    #[tokio::test]
    async fn register_twice_keeps_the_token() {
        let fixture = Fixture::unregistered();
        let identities = &fixture.daemon.core.identities;
        identities.register(fixture.who.clone()).await.unwrap();
        let first = identities
            .local_token(&fixture.who.identity_id)
            .await
            .unwrap()
            .unwrap();
        identities.register(fixture.who.clone()).await.unwrap();
        let second = identities
            .local_token(&fixture.who.identity_id)
            .await
            .unwrap()
            .unwrap();
        assert!(first.expose() == second.expose());
    }

    #[tokio::test]
    async fn forget_removes_the_token() {
        let fixture = Fixture::unregistered();
        let identities = &fixture.daemon.core.identities;
        identities.register(fixture.who.clone()).await.unwrap();
        let token = identities
            .local_token(&fixture.who.identity_id)
            .await
            .unwrap()
            .unwrap();
        identities.forget(&fixture.who).await.unwrap();
        assert!(
            identities
                .local_token(&fixture.who.identity_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(fixture.tokens.get(&fixture.local_key()).unwrap().is_none());
        assert_eq!(
            identities
                .authorize(Some(&fixture.who.identity_id), Some(token.expose()))
                .await
                .unwrap_err(),
            unauthorized()
        );
        // Forgetting an identity with no credential is also harmless.
        identities.forget(&fixture.who).await.unwrap();
    }

    #[tokio::test]
    async fn local_token_debug_is_redacted() {
        let fixture = Fixture::new(None);
        let token = fixture
            .daemon
            .core
            .identities
            .local_token(&fixture.who.identity_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(format!("{token:?}"), "LocalToken(<redacted>)");
    }

    #[tokio::test]
    async fn failed_store_rolls_back_register() {
        let fixture = Fixture::unregistered();
        let identities = &fixture.daemon.core.identities;
        // The read finds no credential, but the file store cannot open its
        // temporary write path because a directory occupies it.
        let mut temporary = fixture.local_path().into_os_string();
        temporary.push(".tmp");
        std::fs::create_dir(PathBuf::from(temporary)).unwrap();
        assert!(identities.register(fixture.who.clone()).await.is_err());
        let rows = identities
            .managed(
                fixture.who.hub_url.origin().ascii_serialization(),
                fixture.who.owner_id.clone(),
            )
            .await
            .unwrap();
        assert!(rows.is_empty());
        assert!(identities.launched.lock().await.is_empty());
        assert!(fixture.tokens.get(&fixture.local_key()).unwrap().is_none());
    }

    #[tokio::test]
    async fn failed_store_keeps_a_row_that_was_already_registered() {
        let fixture = Fixture::new(None);
        let identities = &fixture.daemon.core.identities;
        // A row from an earlier run whose credential is gone: issuing a new
        // one fails, and the failure must not erase the row it did not add.
        fixture.tokens.delete(&fixture.local_key()).unwrap();
        let mut temporary = fixture.local_path().into_os_string();
        temporary.push(".tmp");
        std::fs::create_dir(PathBuf::from(temporary)).unwrap();
        assert!(identities.register(fixture.who.clone()).await.is_err());
        let rows = identities
            .managed(
                fixture.who.hub_url.origin().ascii_serialization(),
                fixture.who.owner_id.clone(),
            )
            .await
            .unwrap();
        assert_eq!(rows, vec![fixture.who.clone()]);
    }

    #[tokio::test]
    async fn forgotten_identity_leftover_local_token_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = Fixture::new(None);
        let identities = &fixture.daemon.core.identities;
        let token = identities
            .local_token(&fixture.who.identity_id)
            .await
            .unwrap()
            .unwrap();
        let directory = fixture._dir.path().join("tokens");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        let forgotten = identities.forget(&fixture.who).await;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        forgotten.unwrap();
        assert!(fixture.tokens.get(&fixture.local_key()).unwrap().is_some());
        assert!(
            identities
                .local_token(&fixture.who.identity_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            identities
                .authorize(Some(&fixture.who.identity_id), Some(token.expose()))
                .await
                .unwrap_err(),
            unauthorized()
        );
    }

    #[tokio::test]
    async fn managed_agent_methods_require_a_human_caller() {
        let fixture = Fixture::new(None);
        for method in [
            "agent_launch",
            "agent_stop",
            "agent_list",
            "agent_start",
            "agent_attach",
            "agent_detach",
            "agent_remove",
        ] {
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
    async fn identity_sessions_stop_at_the_managed_bound() {
        async fn refusal(identities: &Identities, who: &ManagedIdentity) -> String {
            let envelope = serde_json::from_value(envelope()).unwrap();
            identities.post(who, envelope).await.unwrap_err().message
        }
        let fixture = Fixture::new(None);
        let identities = &fixture.daemon.core.identities;
        for n in 0..MAX_MANAGED - 1 {
            identities.runtimes.lock().await.insert(
                (
                    "https://hub.example".into(),
                    format!("held{n:04}").parse().unwrap(),
                ),
                Arc::new(AsyncMutex::new(None)),
            );
        }
        // The 256th session gets as far as looking for its hub token.
        let no_token = "managed identity has no hub token";
        assert_eq!(refusal(identities, &fixture.who).await, no_token);
        let mut sibling = fixture.who.clone();
        sibling.identity_id = "agent002".parse().unwrap();
        assert_eq!(
            refusal(identities, &sibling).await,
            format!("{MAX_MANAGED} identity sessions are already running")
        );
        // A session that holds one of the 256 places keeps it.
        assert_eq!(refusal(identities, &fixture.who).await, no_token);
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
