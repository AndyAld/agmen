//! This machine's lazy coordination record and its owned lease sweeper.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agmen_client::{Login, Page, local_hub_url};
use agmen_core::{
    Core, Gate, ItemList, LaneList, LaneSpec, MAX_CHECK_PATHS, MAX_PAGE, PathCheck, Status,
};
use agmen_local::{ApiError, ErrorCode, console::RosterEntry};
use agmen_proto::{
    ClientEnvelope, DeliveryReply, IdentityId, IdentityKind, ProjectId, Seq, Stream, Timestamp,
    Topics, Verb,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

use crate::console::bad_request;
use crate::hub::HubError;
use crate::record::{
    self, SqliteStore,
    admin::{LocalMint, LocalPerson, MintError, Minted, clean_person_handle},
};

pub(crate) const DEFAULT_CLAIM_LEASE: Duration = Duration::from_secs(60);
const OPEN_WAIT: Duration = Duration::from_secs(10);
const MAX_NAME_CHARS: usize = 64;
const SWEEPER_RETRY: Duration = Duration::from_secs(1);
const MAX_SWEEPER_BACKOFF: Duration = Duration::from_secs(30);
const MAX_SWEEPER_RUNS: u32 = 10;
const MAX_PRESENCES: usize = 256;
const MAX_PRESENCE_CONNECTIONS: usize = 64;

type PresenceCounts = Arc<std::sync::Mutex<BTreeMap<(ProjectId, IdentityId), usize>>>;

#[derive(Debug, Clone)]
struct Record {
    core: Arc<Core<SqliteStore>>,
    person: LocalPerson,
    presences: PresenceCounts,
}

#[derive(Debug)]
struct Running {
    record: Record,
    sweeper: JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.sweeper.abort();
    }
}

#[derive(Debug)]
pub(crate) struct LocalOrigin {
    path: PathBuf,
    claim_lease: Duration,
    running: Mutex<Option<Running>>,
    stopped: AtomicBool,
}

impl LocalOrigin {
    pub(crate) fn new(state_dir: &Path, claim_lease: Duration) -> Arc<Self> {
        Arc::new(Self {
            path: state_dir.join(record::schema::RECORD_DB),
            claim_lease,
            running: Mutex::new(None),
            stopped: AtomicBool::new(false),
        })
    }

    pub(crate) async fn start(&self) {
        if let Err(error) = self.open(None).await
            && error.code != ErrorCode::NotFound
        {
            tracing::warn!(message = %error.message, "local origin unavailable; hub origins remain available");
        }
    }

    async fn open(&self, handle_hint: Option<&str>) -> Result<Record, ApiError> {
        let mut running = tokio::time::timeout(OPEN_WAIT, self.running.lock())
            .await
            .map_err(|_| busy())?;
        if self.stopped.load(Ordering::Acquire) {
            return Err(stopped());
        }
        if let Some(running) = running.as_ref() {
            return Ok(running.record.clone());
        }
        let path = self.path.clone();
        let create = handle_hint.is_some();
        let store = tokio::task::spawn_blocking(move || {
            if !create {
                match std::fs::metadata(&path) {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(failed(error)),
                }
            }
            // Inspect committed WAL frames before SQLite can recover or checkpoint them.
            if let Some(version) =
                agmen_client::preflight_schema(&path, OPEN_WAIT).map_err(failed)?
                && !(0..=record::schema::RECORD_SCHEMA).contains(&version)
            {
                return Err(record_error(record::schema::RecordError::Version(version)));
            }
            record::schema::open(&path, Arc::new(Timestamp::now))
                .map(Some)
                .map_err(record_error)
        })
        .await
        .map_err(failed)??
        .ok_or_else(|| missing("no local origin on this machine"))?;
        let person = match handle_hint {
            Some(hint) => store
                .create_person(&clean_person_handle(hint))
                .await
                .map_err(failed)?,
            None => store
                .person()
                .await
                .map_err(failed)?
                .ok_or_else(|| failed("no local person on this machine"))?,
        };
        let core = Arc::new(Core::new(store, self.claim_lease));
        let sweep_core = core.clone();
        let sweeper = tokio::spawn(async move {
            supervise_sweeper(|| sweep_core.run_lease_sweeper(), SWEEPER_RETRY).await;
        });
        let record = Record {
            core,
            person,
            presences: Arc::default(),
        };
        *running = Some(Running {
            record: record.clone(),
            sweeper,
        });
        Ok(record)
    }

    async fn for_project(&self, project: &ProjectId) -> Result<Record, ApiError> {
        let record = self.open(None).await?;
        if record
            .core
            .store()
            .topics(project)
            .await
            .map_err(failed)?
            .is_none()
        {
            return Err(missing("no such project on this machine"));
        }
        Ok(record)
    }

    #[cfg(test)]
    pub(crate) async fn has_project(&self, project: &ProjectId) -> bool {
        self.topics(project).await.is_ok()
    }

    pub(crate) async fn person(&self) -> Result<Option<LocalPerson>, ApiError> {
        match self.open(None).await {
            Ok(record) => Ok(Some(record.person)),
            Err(error) if error.code == ErrorCode::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn login(&self) -> Result<Option<Login>, ApiError> {
        Ok(self.person().await?.map(|person| Login {
            hub_url: local_hub_url(),
            human_id: person.id,
            handle: person.handle,
        }))
    }

    pub(crate) async fn create_project(
        &self,
        name: &str,
        handle_hint: &str,
    ) -> Result<Topics, ApiError> {
        if !is_display_name(name) {
            return Err(bad_request(
                "name must be 1 to 64 characters, with no control characters or leading or trailing whitespace",
            ));
        }
        self.open(Some(handle_hint))
            .await?
            .core
            .store()
            .create_project(name)
            .await
            .map_err(|error| match error {
                agmen_core::StoreError::Duplicate => {
                    bad_request("a local project already has that name")
                }
                error => failed(error),
            })
    }

    pub(crate) async fn projects(&self) -> Result<Vec<(ProjectId, String)>, ApiError> {
        self.open(None)
            .await?
            .core
            .store()
            .projects()
            .await
            .map_err(failed)
    }

    pub(crate) async fn topics(&self, project: &ProjectId) -> Result<Topics, ApiError> {
        self.open(None)
            .await?
            .core
            .store()
            .topics(project)
            .await
            .map_err(failed)?
            .ok_or_else(|| missing("no such project on this machine"))
    }

    pub(crate) async fn head(&self, project: &ProjectId) -> Result<u64, ApiError> {
        Ok(self
            .for_project(project)
            .await?
            .core
            .page(project, &Stream::Feed, 0, 1)
            .await
            .map_err(failed)?
            .head)
    }

    pub(crate) async fn page(
        &self,
        project: &ProjectId,
        stream: &Stream,
        after: u64,
        limit: u32,
    ) -> Result<Page, ApiError> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(bad_request(format!("limit must be 1 to {MAX_PAGE}")));
        }
        i64::try_from(after).map_err(|_| bad_request("after exceeds the storage range"))?;
        convert(
            self.for_project(project)
                .await?
                .core
                .page(project, stream, after, limit)
                .await
                .map_err(failed)?,
        )
    }

    pub(crate) async fn roster(&self, project: &ProjectId) -> Result<Vec<RosterEntry>, ApiError> {
        let record = self.for_project(project).await?;
        let topics = Topics::new(record.person.tenant, project.clone());
        convert(record.core.roster(&topics).await.map_err(failed)?)
    }

    pub(crate) async fn state(&self, project: &ProjectId, key: &str) -> Result<Value, ApiError> {
        if !matches!(key, "head" | "roster") {
            return Err(bad_request("key must be roster or head"));
        }
        let head = self.head(project).await?;
        let value = if key == "head" {
            json!({"feed_seq": head, "rules_v": 0, "lanes_v": 0, "items_v": 0, "hub_at": Timestamp::now()})
        } else {
            serde_json::to_value(self.roster(project).await?).map_err(failed)?
        };
        Ok(json!({"key": key, "as_of_seq": head, "value": value}))
    }

    pub(crate) async fn ingest(
        &self,
        project: &ProjectId,
        from: &IdentityId,
        envelope: &ClientEnvelope,
    ) -> Result<DeliveryReply, ApiError> {
        let record = self.for_project(project).await?;
        let topics = Topics::new(record.person.tenant, project.clone());
        let raw = serde_json::to_vec(envelope).map_err(failed)?;
        let handled = record
            .core
            .ingest(&topics, from, &raw, Gate::Open)
            .await
            .map_err(failed)?;
        if let Some(meta) = handled.recorded {
            match meta.verb {
                Verb::Bye => {
                    record
                        .core
                        .statuses()
                        .set(project, &meta.from, Status::Offline);
                    record.core.notifier().stored(project);
                }
                Verb::Rollcall => {
                    if let Err(error) = record
                        .core
                        .answer_rollcall(&topics, &meta.id, &meta.from, meta.from_kind)
                        .await
                    {
                        tracing::warn!(%error, "answering a local ROLLCALL failed");
                    }
                }
                _ => {}
            }
        }
        Ok(handled.reply)
    }

    pub(crate) async fn post_as_person(
        &self,
        project: &ProjectId,
        envelope: &ClientEnvelope,
    ) -> Result<(Stream, Seq), ApiError> {
        let person = self.for_project(project).await?.person;
        match self.ingest(project, &person.id, envelope).await? {
            DeliveryReply::Ack { stream, seq, .. } => Ok((stream, seq)),
            DeliveryReply::Nack {
                reason,
                retry_after_ms,
                ..
            } => Err(HubError::Nack {
                reason,
                retry_after_ms,
            }
            .into()),
        }
    }

    pub(crate) async fn mint(&self, request: LocalMint) -> Result<Minted, HubError> {
        let record = self.open(None).await.map_err(|error| HubError::Refused {
            status: 500,
            code: "server_error".into(),
            description: error.message,
        })?;
        let minted = record
            .core
            .store()
            .mint(request)
            .await
            .map_err(mint_error)?;
        record.core.notifier().stored(minted.topics.project());
        Ok(minted)
    }

    pub(crate) async fn revoke(
        &self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<bool, ApiError> {
        let record = self.for_project(project).await?;
        let revoked = record
            .core
            .store()
            .revoke(project, id)
            .await
            .map_err(failed)?;
        if revoked {
            record.core.statuses().set(project, id, Status::Offline);
            record.core.notifier().stored(project);
        }
        Ok(revoked)
    }

    pub(crate) async fn lanes(&self, project: &ProjectId) -> Result<LaneList, ApiError> {
        self.for_project(project)
            .await?
            .core
            .list_lanes(project)
            .await
            .map_err(failed)
    }

    pub(crate) async fn replace_lanes(
        &self,
        project: &ProjectId,
        lanes: Vec<LaneSpec>,
    ) -> Result<LaneList, ApiError> {
        let record = self.for_project(project).await?;
        let list = record
            .core
            .replace_lanes(project, IdentityKind::Human, lanes)
            .await
            .map_err(|error| match error {
                agmen_core::ReplaceError::Store(error) => failed(error),
                error => bad_request(error.to_string()),
            })?;
        record.core.notifier().stored(project);
        Ok(list)
    }

    pub(crate) async fn check_path(
        &self,
        project: &ProjectId,
        id: &IdentityId,
        paths: Vec<String>,
    ) -> Result<Vec<PathCheck>, ApiError> {
        if paths.len() > MAX_CHECK_PATHS {
            return Err(bad_request(format!(
                "paths must contain at most {MAX_CHECK_PATHS} entries"
            )));
        }
        self.for_project(project)
            .await?
            .core
            .check_path(project, id, paths)
            .await
            .map_err(failed)?
            .ok_or_else(|| missing("no such live identity on this machine"))
    }

    pub(crate) async fn items(&self, project: &ProjectId) -> Result<ItemList, ApiError> {
        self.for_project(project)
            .await?
            .core
            .items(project)
            .await
            .map_err(failed)
    }

    pub(crate) async fn watch_stored(
        &self,
        project: &ProjectId,
    ) -> Result<watch::Receiver<()>, ApiError> {
        Ok(self
            .for_project(project)
            .await?
            .core
            .notifier()
            .watch_stored(project))
    }

    pub(crate) async fn presence(
        &self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<PresenceGuard, ApiError> {
        let record = self.for_project(project).await?;
        let core = record.core;
        let key = (project.clone(), id.clone());
        let mut counts = record.presences.lock().map_err(failed)?;
        if !counts.contains_key(&key) && counts.len() >= MAX_PRESENCES {
            return Err(failed("local presence capacity reached"));
        }
        let count = counts.entry(key).or_default();
        if *count >= MAX_PRESENCE_CONNECTIONS {
            return Err(failed("local presence connection capacity reached"));
        }
        *count += 1;
        core.statuses().set(project, id, Status::Online);
        core.notifier().stored(project);
        drop(counts);
        Ok(PresenceGuard {
            core,
            counts: record.presences,
            project: project.clone(),
            id: id.clone(),
        })
    }

    pub(crate) async fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        if tokio::time::timeout(OPEN_WAIT, async {
            let mut running = self.running.lock().await;
            if let Some(mut running) = running.take() {
                running.sweeper.abort();
                if let Err(error) = (&mut running.sweeper).await
                    && !error.is_cancelled()
                {
                    tracing::warn!(%error, "joining the local lease sweeper failed");
                }
            }
        })
        .await
        .is_err()
        {
            tracing::warn!("stopping the local origin reached its deadline");
        }
    }
}

async fn supervise_sweeper<F, Fut, E>(mut sweep: F, retry: Duration)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let mut backoff = retry.min(MAX_SWEEPER_BACKOFF);
    for attempt in 1..=MAX_SWEEPER_RUNS {
        match sweep().await {
            Ok(()) => return,
            Err(error) if attempt == MAX_SWEEPER_RUNS => {
                tracing::error!(%error, attempt, "local lease sweeper exhausted its attempts");
            }
            Err(error) => {
                tracing::warn!(%error, attempt, ?backoff, "retrying the local lease sweeper");
                tokio::time::sleep(backoff).await;
                backoff = backoff.saturating_mul(2).min(MAX_SWEEPER_BACKOFF);
            }
        }
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const RETRY: Duration = Duration::from_millis(1);
    const TEST_WAIT: Duration = Duration::from_secs(2);

    #[tokio::test]
    async fn sweeper_recovers_after_transient_give_up() {
        let attempts = AtomicUsize::new(0);
        supervise_sweeper(
            || async {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err("60 transient sweep failures")
                } else {
                    Ok(())
                }
            },
            RETRY,
        )
        .await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn sweeper_exhausts_a_bounded_number_of_runs() {
        let attempts = AtomicUsize::new(0);
        tokio::time::timeout(
            TEST_WAIT,
            supervise_sweeper(
                || async {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>("record unavailable")
                },
                RETRY,
            ),
        )
        .await
        .unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), MAX_SWEEPER_RUNS as usize);
    }

    #[tokio::test]
    async fn overlapping_presence_guards_remain_online_until_the_last_closes() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalOrigin::new(dir.path(), DEFAULT_CLAIM_LEASE);
        let topics = local.create_project("presence", "andy").await.unwrap();
        let project = topics.project();
        let id = local.person().await.unwrap().unwrap().id;
        let record = local.for_project(project).await.unwrap();
        let first = local.presence(project, &id).await.unwrap();
        let second = local.presence(project, &id).await.unwrap();
        drop(first);
        assert_eq!(record.core.statuses().status(project, &id), Status::Online);
        drop(second);
        assert_eq!(record.core.statuses().status(project, &id), Status::Offline);
        tokio::time::timeout(TEST_WAIT, local.shutdown())
            .await
            .unwrap();
    }
}

#[derive(Debug)]
pub(crate) struct PresenceGuard {
    core: Arc<Core<SqliteStore>>,
    counts: PresenceCounts,
    project: ProjectId,
    id: IdentityId,
}

impl Drop for PresenceGuard {
    fn drop(&mut self) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (self.project.clone(), self.id.clone());
        let Some(count) = counts.get_mut(&key) else {
            return;
        };
        *count -= 1;
        if *count > 0 {
            return;
        }
        counts.remove(&key);
        self.core
            .statuses()
            .set(&self.project, &self.id, Status::Offline);
        self.core.notifier().stored(&self.project);
    }
}

// Mirrors agmen_hub::db::is_display_name without depending on the hub crate.
fn is_display_name(name: &str) -> bool {
    (1..=MAX_NAME_CHARS).contains(&name.chars().count())
        && !name.chars().any(char::is_control)
        && name.trim() == name
}

fn missing(message: &str) -> ApiError {
    ApiError {
        code: ErrorCode::NotFound,
        message: message.into(),
    }
}

fn failed(error: impl std::fmt::Display) -> ApiError {
    ApiError {
        code: ErrorCode::Hub,
        message: format!("local.db: {error}"),
    }
}

pub(crate) fn busy() -> ApiError {
    ApiError {
        code: ErrorCode::Hub,
        message: "local.db is busy; try again".into(),
    }
}

fn stopped() -> ApiError {
    ApiError {
        code: ErrorCode::Hub,
        message: "the local origin has stopped".into(),
    }
}

/// Whether `error` says the origin is busy or stopped, which tells routing
/// nothing about the record, rather than that the record cannot be read.
pub(crate) fn is_unavailable(error: &ApiError) -> bool {
    *error == busy() || *error == stopped()
}

fn record_error(error: record::schema::RecordError) -> ApiError {
    ApiError {
        code: if matches!(error, record::schema::RecordError::Version(_)) {
            ErrorCode::Version
        } else {
            ErrorCode::Hub
        },
        message: error.to_string(),
    }
}

fn convert<T: Serialize, U: DeserializeOwned>(value: T) -> Result<U, ApiError> {
    serde_json::from_value(serde_json::to_value(value).map_err(failed)?).map_err(failed)
}

fn mint_error(error: MintError) -> HubError {
    let (status, code) = match error.code {
        "not_a_member" | "coordinator_role_required" => (403, error.code),
        "bad_handle" | "bad_role" | "bad_edits" => (422, error.code),
        "listener_exists" | "handle_taken" => (409, error.code),
        _ => (500, "server_error"),
    };
    HubError::Refused {
        status,
        code: code.into(),
        description: error.message,
    }
}

#[cfg(test)]
#[path = "local_origin_tests.rs"]
mod tests;
