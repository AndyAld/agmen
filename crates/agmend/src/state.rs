//! Durable daemon metadata, pending notifications and receive positions.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::{Duration, Instant};

use agmen_client::Broker;
use agmen_local::safeguards::MAX_MANAGED;
use agmen_proto::{
    ClientEnvelope, IdentityId, IdentityKind, MessageId, Payload, ProjectId, StampedEnvelope,
    Stream, TenantId,
};
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use url::Url;

const SCHEMA: &str = "
CREATE TABLE identities (
 origin TEXT NOT NULL, identity TEXT NOT NULL, owner TEXT NOT NULL,
 project TEXT NOT NULL, metadata TEXT NOT NULL, PRIMARY KEY(origin, identity)
) STRICT;
CREATE TABLE positions (
 origin TEXT NOT NULL, project TEXT NOT NULL, stream TEXT NOT NULL,
 seq INTEGER NOT NULL CHECK(seq >= 0), PRIMARY KEY(origin, project, stream)
) STRICT;
CREATE TABLE pending (
 n INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, project TEXT NOT NULL,
 identity TEXT NOT NULL, sender TEXT NOT NULL, id TEXT NOT NULL, message TEXT NOT NULL,
 UNIQUE(origin, project, identity, sender, id)
) STRICT;
CREATE INDEX pending_identity ON pending(origin, identity, n);
CREATE TABLE receipts (
 origin TEXT NOT NULL, project TEXT NOT NULL, identity TEXT NOT NULL, sender TEXT NOT NULL, id TEXT NOT NULL,
 feed INTEGER, inbox INTEGER, PRIMARY KEY(origin, project, identity, sender, id)
) STRICT;
CREATE INDEX receipts_identity ON receipts(origin, identity);
";

/// Stream positions per project and origin: Feed, the person's inbox and one
/// inbox per managed identity. Hub reconnects split these into bounded requests.
pub(crate) const MAX_STREAM_POSITIONS: usize = MAX_MANAGED + 2;
pub(crate) const MAX_PENDING: i64 = 10_000;
const MAX_RECEIPTS: i64 = 20_000;
// Notifications use a scalar count, bounded by SQLite's sequence range.
const MAX_QUEUE_ALERTS: i64 = i64::MAX;

/// The cap that caused a durable owner notification.
#[derive(Debug, Clone, Copy)]
pub(crate) enum QueueLimit {
    Pending,
    Receipts,
}

impl QueueLimit {
    fn counter(self) -> &'static str {
        match self {
            Self::Pending => "$.queue_full.pending",
            Self::Receipts => "$.queue_full.receipts",
        }
    }

    pub(crate) fn text(self, identity: &ManagedIdentity) -> String {
        let cap = match self {
            Self::Receipts => format!("{MAX_RECEIPTS} receipts"),
            _ => format!("{MAX_PENDING} waiting"),
        };
        format!(
            "{}'s queue is full ({cap}); new messages to it are recorded in the project but not queued for it until it is started or removed.",
            identity.handle
        )
    }
}

/// What one collector may deliver for a project.
pub(crate) struct CollectionState {
    pub(crate) positions: BTreeMap<Stream, u64>,
    /// The streams it delivers: the feed, the person's inbox and live identities'.
    pub(crate) streams: BTreeSet<Stream>,
    /// The project has room for one more position.
    pub(crate) room: bool,
}

/// Metadata only. Credentials stay in TokenStore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedIdentity {
    pub(crate) hub_url: Url,
    pub(crate) broker_url: Broker,
    pub(crate) tenant: TenantId,
    pub(crate) project: ProjectId,
    pub(crate) identity_id: IdentityId,
    pub(crate) owner_id: IdentityId,
    pub(crate) handle: String,
    pub(crate) kind: IdentityKind,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    #[error("daemon database I/O failed")]
    Io(#[from] std::io::Error),
    #[error("daemon database SQLite operation failed")]
    Sql(#[from] rusqlite::Error),
    #[error("daemon database JSON is invalid")]
    Json(#[from] serde_json::Error),
    #[error("daemon database schema {0} is unsupported (expected 2)")]
    Version(i64),
    #[error("daemon database invariant failed: {0}")]
    Invalid(&'static str),
    #[error("identity store failed: {0}")]
    Client(#[from] agmen_client::ClientError),
    #[error("identity store {} failed: {source}", path.display())]
    Identity {
        path: std::path::PathBuf,
        #[source]
        source: Box<StoreError>,
    },
    #[error("daemon database limit reached: {0}")]
    Full(&'static str),
    #[error("stream gap: expected {expected}, received {received}")]
    Gap { expected: u64, received: u64 },
}

#[derive(Debug)]
pub(crate) struct Store {
    conn: Connection,
    own_ids: BTreeMap<(String, IdentityId), VecDeque<MessageId>>,
}

/// How a schema check reads a store: strictly, or as the preflight of a
/// caller that will open it with SQLite and recheck under the migration lock.
type Inspect = fn(&Path, Duration) -> Result<Option<i64>, agmen_client::ClientError>;

fn schema(path: &Path, deadline: Instant, inspect: Inspect) -> Result<Option<i64>, StoreError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or(StoreError::Full("store inspection deadline"))?;
    let version = inspect(path, remaining)?;
    if let Some(version) = version
        && !(0..=2).contains(&version)
    {
        return Err(StoreError::Version(version));
    }
    Ok(version)
}

/// Each identity store path under `dir`, whether or not the file exists yet.
fn identity_stores(dir: &Path) -> Result<Vec<std::path::PathBuf>, StoreError> {
    let entries = match fs::read_dir(dir.join("identities")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut stores = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err(StoreError::Full("4096 identity stores"));
        }
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            return Err(StoreError::Invalid("identity store directory is a symlink"));
        }
        if entry.file_type()?.is_dir() {
            stores.push(entry.path().join(agmen_client::STATE_DB));
        }
    }
    Ok(stores)
}

fn identity_schema(
    path: &Path,
    deadline: Instant,
    inspect: Inspect,
) -> Result<Option<i64>, StoreError> {
    schema(path, deadline, inspect).map_err(|source| StoreError::Identity {
        path: path.to_owned(),
        source: Box::new(source),
    })
}

/// Read-only preflight of every daemon-owned SQLite store, including lazy sessions.
pub(crate) fn schemas(dir: &Path, deadline: Instant) -> Result<BTreeMap<String, i64>, StoreError> {
    let inspect: Inspect = agmen_client::store_schema;
    let mut versions = BTreeMap::new();
    if let Some(version) = schema(&dir.join("agmend.db"), deadline, inspect)? {
        versions.insert("agmend.db".to_owned(), version);
    }
    let record_path = dir.join(crate::record::schema::RECORD_DB);
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or(StoreError::Full("store inspection deadline"))?;
    // A newer local.db is reported as found, not refused: local calls refuse it
    // by name, and hub projects must keep working.
    if let Some(version) = agmen_client::store_schema(&record_path, remaining)? {
        versions.insert(crate::record::schema::RECORD_DB.to_owned(), version);
    }
    for path in identity_stores(dir)? {
        if let Some(version) = identity_schema(&path, deadline, inspect)? {
            let relative = path
                .strip_prefix(dir)
                .map_err(|_| StoreError::Invalid("store outside state directory"))?;
            let relative = relative
                .to_str()
                .ok_or(StoreError::Invalid("store path is not UTF-8"))?;
            versions.insert(relative.to_owned(), version);
        }
    }
    Ok(versions)
}

/// A strict snapshot for rollback: every discovered file must be readable, including future schemas.
pub(crate) fn snapshot(dir: &Path) -> Result<Vec<(std::path::PathBuf, i64)>, StoreError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut paths = identity_stores(dir)?;
    paths.push(dir.join("agmend.db"));
    include_record(dir, &mut paths)?;
    paths.sort();
    let mut result = Vec::with_capacity(paths.len());
    for path in &paths {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(StoreError::Full("store inspection deadline"))?;
        let version = agmen_client::store_schema(path, remaining)
            .map_err(|source| StoreError::Identity {
                path: path.clone(),
                source: Box::new(StoreError::Client(source)),
            })?
            .ok_or_else(|| StoreError::Identity {
                path: path.clone(),
                source: Box::new(StoreError::Invalid(
                    "store snapshot is incomplete: store is missing",
                )),
            })?;
        result.push((path.clone(), version));
    }
    let mut after = identity_stores(dir)?;
    after.push(dir.join("agmend.db"));
    include_record(dir, &mut after)?;
    after.sort();
    if paths != after {
        return Err(StoreError::Invalid(
            "store discovery changed during snapshot",
        ));
    }
    Ok(result)
}

fn include_record(dir: &Path, paths: &mut Vec<std::path::PathBuf>) -> Result<(), StoreError> {
    let path = dir.join(crate::record::schema::RECORD_DB);
    match fs::symlink_metadata(&path) {
        Ok(_) => paths.push(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn open_stores(path: &Path) -> Result<Store, StoreError> {
    let dir = path
        .parent()
        .ok_or(StoreError::Invalid("database has no parent"))?;
    // Refuse all newer stores before migrating even the first older store.
    let deadline = Instant::now() + Duration::from_secs(10);
    let inspect: Inspect = agmen_client::preflight_schema;
    schema(path, deadline, inspect)?;
    let identities = identity_stores(dir)?;
    for identity in &identities {
        identity_schema(identity, deadline, inspect)?;
    }
    let store = Store::open(path)?;
    for identity in identities {
        if Instant::now() >= deadline {
            return Err(StoreError::Full("store initialization deadline"));
        }
        // An absent file stays absent: sessions create their store lazily.
        if fs::symlink_metadata(&identity).is_err() {
            continue;
        }
        drop(
            agmen_client::Outbox::open(&identity).map_err(|source| StoreError::Identity {
                path: identity,
                source: Box::new(StoreError::Client(source)),
            })?,
        );
    }
    Ok(store)
}

// Canonical URL origins contain no fragment and IdentityId contains only
// lowercase ASCII letters and digits. This key never leaves the database.
fn position_namespace(origin: &str, owner: Option<&IdentityId>) -> String {
    owner.map_or_else(
        || origin.to_owned(),
        |owner| format!("{origin}#owner={owner}"),
    )
}

impl Store {
    pub(crate) fn open(path: &Path) -> Result<Self, StoreError> {
        schema(
            path,
            Instant::now() + Duration::from_secs(5),
            agmen_client::preflight_schema,
        )?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or(StoreError::Invalid("database path has no parent"))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        // SQLite inherits the database mode for WAL and SHM, so restrict it first.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if !(0..=2).contains(&version) {
            return Err(StoreError::Version(version));
        }
        let mode: String = tx.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        let convert = !mode.eq_ignore_ascii_case("wal");
        if convert {
            tx.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        }
        match version {
            0 => {
                tx.execute_batch(SCHEMA)?;
                tx.pragma_update(None, "user_version", 2)?;
            }
            1 => {
                // A drained v1 receipt has no recoverable sender. Preserve it
                // with an empty sender, without deduplicating another sender
                // against it or charging it to the active receipt quota.
                tx.execute_batch("ALTER TABLE pending RENAME TO pending_v1;
                    ALTER TABLE receipts RENAME TO receipts_v1;
                    DROP INDEX pending_identity; DROP INDEX receipts_identity;
                    CREATE TABLE pending (
                        n INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, project TEXT NOT NULL,
                        identity TEXT NOT NULL, sender TEXT NOT NULL, id TEXT NOT NULL, message TEXT NOT NULL,
                        UNIQUE(origin,project,identity,sender,id)) STRICT;
                    CREATE INDEX pending_identity ON pending(origin,identity,n);
                    INSERT INTO pending SELECT n,origin,project,identity,json_extract(message,'$.from'),id,message FROM pending_v1;
                    CREATE TABLE receipts (
                        origin TEXT NOT NULL, project TEXT NOT NULL, identity TEXT NOT NULL,
                        sender TEXT NOT NULL, id TEXT NOT NULL, feed INTEGER, inbox INTEGER,
                        PRIMARY KEY(origin,project,identity,sender,id)) STRICT;
                    CREATE INDEX receipts_identity ON receipts(origin,identity);
                    INSERT INTO receipts SELECT r.origin,r.project,r.identity,coalesce(p.sender,''),r.id,r.feed,r.inbox
                        FROM receipts_v1 r LEFT JOIN pending p ON p.origin=r.origin AND p.project=r.project AND p.identity=r.identity AND p.id=r.id;
                    DROP TABLE pending_v1; DROP TABLE receipts_v1;
                    PRAGMA user_version=2;")?;
            }
            2 => {}
            other => return Err(StoreError::Version(other)),
        }
        // Additive to v2: old readers still use the same tables and columns.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS stream_alerts (
            origin TEXT NOT NULL, owner TEXT NOT NULL, project TEXT NOT NULL,
            full INTEGER NOT NULL, alerts INTEGER NOT NULL, envelope TEXT,
            PRIMARY KEY(origin,owner,project)) STRICT;",
        )?;
        tx.commit()?;
        if convert {
            // Keep the exclusive migration lock until journal conversion completes.
            let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
            if !mode.eq_ignore_ascii_case("wal") {
                return Err(StoreError::Invalid("WAL unavailable"));
            }
            drop(conn);
            conn = Connection::open(path)?;
            conn.busy_timeout(Duration::from_secs(5))?;
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        // Initialize WAL sidecars now, while the database and directory are private.
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?;
        Ok(Self {
            conn,
            own_ids: BTreeMap::new(),
        })
    }

    pub(crate) fn register(&mut self, identity: &ManagedIdentity) -> Result<(), StoreError> {
        let origin = agmen_client::origin_key(&identity.hub_url);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT owner, project FROM identities WHERE origin=?1 AND identity=?2",
                params![origin, identity.identity_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((owner, project)) = &existing {
            if owner != identity.owner_id.as_str() || project != identity.project.as_str() {
                return Err(StoreError::Invalid("identity owner or project changed"));
            }
        } else {
            let count: i64 = tx.query_row("SELECT count(*) FROM identities", [], |r| r.get(0))?;
            if count >= MAX_MANAGED as i64 {
                static FULL: std::sync::LazyLock<String> =
                    std::sync::LazyLock::new(|| format!("{MAX_MANAGED} managed identities"));
                return Err(StoreError::Full(&FULL));
            }
            let projects: i64 = tx.query_row(
                "SELECT count(*) FROM (SELECT DISTINCT origin, project FROM identities UNION SELECT ?1, ?2)",
                params![origin, identity.project.as_str()], |r| r.get(0))?;
            if projects > 64 {
                return Err(StoreError::Full("64 managed projects"));
            }
        }
        tx.execute("INSERT INTO identities VALUES (?1,?2,?3,?4,?5) ON CONFLICT(origin,identity) DO UPDATE SET metadata=json_patch(excluded.metadata,json_object('queue_full',json_extract(identities.metadata,'$.queue_full')))",
            params![origin, identity.identity_id.as_str(), identity.owner_id.as_str(), identity.project.as_str(), serde_json::to_string(identity)?])?;
        tx.commit()?;
        Ok(())
    }

    /// Forgets a removed identity: its row, waiting messages and receipts.
    pub(crate) fn unregister(
        &mut self,
        origin: &str,
        identity: &IdentityId,
    ) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for table in ["identities", "pending", "receipts"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE origin=?1 AND identity=?2"),
                params![origin, identity.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn identities(
        &self,
        origin: &str,
        owner: &IdentityId,
    ) -> Result<Vec<ManagedIdentity>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT metadata FROM identities WHERE origin=?1 AND owner=?2 ORDER BY identity LIMIT ?3")?;
        let mut result = Vec::new();
        for row in stmt.query_map(params![origin, owner.as_str(), MAX_MANAGED as i64], |r| {
            r.get::<_, String>(0)
        })? {
            result.push(serde_json::from_str(&row?)?);
        }
        Ok(result)
    }

    /// Pending notifications survive drains independently of the full marker.
    pub(crate) fn pending_queue_full_alerts(
        &self,
        origin: &str,
        owner: &IdentityId,
    ) -> Result<Vec<ManagedIdentity>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT metadata FROM identities WHERE origin=?1 AND owner=?2 AND json_extract(metadata,'$.queue_full.alerts')>0 ORDER BY identity LIMIT ?3")?;
        let mut identities = Vec::new();
        for row in stmt.query_map(params![origin, owner.as_str(), MAX_MANAGED as i64], |row| {
            row.get::<_, String>(0)
        })? {
            identities.push(serde_json::from_str(&row?)?);
        }
        Ok(identities)
    }

    /// Persist the envelope before publishing so every retry uses the same id.
    pub(crate) fn prepare_queue_full_alert(
        &mut self,
        origin: &str,
        identity: &IdentityId,
        proposed: &ClientEnvelope,
    ) -> Result<Option<ClientEnvelope>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<String> = tx.query_row(
            "SELECT metadata FROM identities WHERE origin=?1 AND identity=?2 AND json_extract(metadata,'$.queue_full.alerts')>0",
            params![origin, identity.as_str()], |row| row.get(0),
        ).optional()?;
        let Some(raw) = current else { return Ok(None) };
        let metadata: serde_json::Value = serde_json::from_str(&raw)?;
        let envelope = if let Some(saved) = metadata.pointer("/queue_full/envelope") {
            serde_json::from_value(saved.clone())?
        } else {
            // Old markers contain only the total and mean the pending cap.
            let limit = if metadata
                .pointer("/queue_full/receipts")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                > 0
            {
                QueueLimit::Receipts
            } else {
                QueueLimit::Pending
            };
            let who: ManagedIdentity = serde_json::from_str(&raw)?;
            let text = limit.text(&who);
            let mut envelope = proposed.clone();
            envelope.body = text
                .clone()
                .try_into()
                .map_err(|_| StoreError::Invalid("full queue ALERT body"))?;
            let Payload::Alert { need, .. } = &mut envelope.payload else {
                return Err(StoreError::Invalid("full queue ALERT payload"));
            };
            *need = text;
            tx.execute("UPDATE identities SET metadata=json_set(metadata,'$.queue_full.envelope',json(?3),'$.queue_full.counter',?4) WHERE origin=?1 AND identity=?2", params![origin, identity.as_str(), serde_json::to_string(&envelope)?, limit.counter()])?;
            envelope
        };
        tx.commit()?;
        Ok(Some(envelope))
    }

    pub(crate) fn acknowledge_queue_full_alert(
        &self,
        origin: &str,
        identity: &IdentityId,
        message: &MessageId,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE identities SET metadata=json_remove(json_set(metadata,'$.queue_full.alerts',json_extract(metadata,'$.queue_full.alerts')-1,
             coalesce(json_extract(metadata,'$.queue_full.counter'),'$.queue_full.pending'),max(0,coalesce(json_extract(metadata,coalesce(json_extract(metadata,'$.queue_full.counter'),'$.queue_full.pending')),1)-1)),
             '$.queue_full.envelope','$.queue_full.counter')
             WHERE origin=?1 AND identity=?2 AND json_extract(metadata,'$.queue_full.envelope.id')=?3 AND json_extract(metadata,'$.queue_full.alerts')>0",
            params![origin, identity.as_str(), message.as_str()],
        )?;
        Ok(())
    }

    /// Whether any identity registered under `origin` belongs to `project`.
    pub(crate) fn has_identity(
        &self,
        origin: &str,
        project: &ProjectId,
    ) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM identities WHERE origin=?1 AND project=?2)",
            params![origin, project.as_str()],
            |r| r.get(0),
        )?)
    }

    pub(crate) fn positions(
        &self,
        origin: &str,
        project: &ProjectId,
    ) -> Result<BTreeMap<Stream, u64>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT stream, seq FROM positions WHERE origin=?1 AND project=?2 ORDER BY stream LIMIT ?3")?;
        let mut result = BTreeMap::new();
        for row in stmt.query_map(
            params![origin, project.as_str(), MAX_STREAM_POSITIONS as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )? {
            let (stream, seq) = row?;
            result.insert(
                stream
                    .parse()
                    .map_err(|_| StoreError::Invalid("stored stream"))?,
                u64::try_from(seq).map_err(|_| StoreError::Invalid("negative sequence"))?,
            );
        }
        Ok(result)
    }

    /// The saved position of `stream` if `owner` may deliver it and one exists.
    pub(crate) fn saved_position(
        &self,
        origin: &str,
        owner: &IdentityId,
        project: &ProjectId,
        stream: &Stream,
    ) -> Result<Option<u64>, StoreError> {
        if !self.can_deliver(origin, owner, project, stream)? {
            return Ok(None);
        }
        let saved: Option<i64> = self
            .conn
            .query_row(
                "SELECT seq FROM positions WHERE origin=?1 AND project=?2 AND stream=?3",
                params![
                    position_namespace(origin, Some(owner)),
                    project.as_str(),
                    stream.to_string()
                ],
                |row| row.get(0),
            )
            .optional()?;
        saved
            .map(|seq| u64::try_from(seq).map_err(|_| StoreError::Invalid("negative sequence")))
            .transpose()
    }

    pub(crate) fn positions_for_owner(
        &self,
        origin: &str,
        owner: &IdentityId,
        project: &ProjectId,
    ) -> Result<BTreeMap<Stream, u64>, StoreError> {
        // B T5 is the first production ingester. Unattributed test-era positions
        // must not be inherited by a different login; catch up from zero instead.
        self.positions(&position_namespace(origin, Some(owner)), project)
    }

    /// Only registered inboxes and the person's streams belong to a collector.
    /// Read without a write transaction.
    pub(crate) fn active_streams(
        &self,
        origin: &str,
        owner: &IdentityId,
        project: &ProjectId,
    ) -> Result<BTreeSet<Stream>, StoreError> {
        let mut streams = BTreeSet::from([Stream::Feed, Stream::Inbox(owner.clone())]);
        let mut stmt = self.conn.prepare("SELECT identity FROM identities WHERE origin=?1 AND owner=?2 AND project=?3 ORDER BY identity LIMIT ?4")?;
        for row in stmt.query_map(
            params![origin, owner.as_str(), project.as_str(), MAX_MANAGED as i64],
            |row| row.get::<_, String>(0),
        )? {
            streams.insert(Stream::Inbox(
                row?.parse()
                    .map_err(|_| StoreError::Invalid("stored identity"))?,
            ));
        }
        Ok(streams)
    }

    pub(crate) fn collection_positions(
        &mut self,
        origin: &str,
        owner: &IdentityId,
        project: &ProjectId,
    ) -> Result<CollectionState, StoreError> {
        let streams = self.active_streams(origin, owner, project)?;
        let namespace = position_namespace(origin, Some(owner));
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM positions WHERE (origin=?1 OR origin=?2) AND project=?3 AND stream!='feed' AND stream!=?4
             AND NOT EXISTS(SELECT 1 FROM identities WHERE origin=?2 AND owner=?5 AND project=?3 AND identity=substr(positions.stream,7))",
            params![namespace, origin, project.as_str(), format!("inbox:{owner}"), owner.as_str()],
        )?;
        let room = rearm_stream_alert(&tx, origin, project)?;
        tx.commit()?;
        Ok(CollectionState {
            positions: self.positions_for_owner(origin, owner, project)?,
            streams,
            room,
        })
    }

    pub(crate) fn can_deliver(
        &self,
        origin: &str,
        owner: &IdentityId,
        project: &ProjectId,
        stream: &Stream,
    ) -> Result<bool, StoreError> {
        match stream {
            Stream::Feed => Ok(true),
            Stream::Inbox(id) if id == owner => Ok(true),
            Stream::Inbox(id) => Ok(self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM identities WHERE origin=?1 AND owner=?2 AND project=?3 AND identity=?4)",
                params![origin, owner.as_str(), project.as_str(), id.as_str()], |row| row.get(0),
            )?),
        }
    }

    pub(crate) fn mark_streams_full(
        &mut self,
        origin: &str,
        owner: &IdentityId,
        project: &ProjectId,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO stream_alerts VALUES (?1,?2,?3,1,1,NULL)
             ON CONFLICT(origin,owner,project) DO UPDATE SET full=1,alerts=min(alerts,?4-1)+1 WHERE full=0",
            params![origin, owner.as_str(), project.as_str(), MAX_QUEUE_ALERTS],
        )?;
        Ok(())
    }

    pub(crate) fn pending_stream_alerts(
        &self,
        origin: &str,
        owner: &IdentityId,
    ) -> Result<Vec<ProjectId>, StoreError> {
        const MAX_PROJECT_ALERTS: usize = 64;
        let mut stmt = self.conn.prepare("SELECT project FROM stream_alerts WHERE origin=?1 AND owner=?2 AND alerts>0 ORDER BY project LIMIT ?3")?;
        let mut projects = Vec::new();
        for row in stmt.query_map(
            params![origin, owner.as_str(), MAX_PROJECT_ALERTS as i64],
            |row| row.get::<_, String>(0),
        )? {
            projects.push(
                row?.parse()
                    .map_err(|_| StoreError::Invalid("stored project"))?,
            );
        }
        Ok(projects)
    }

    pub(crate) fn prepare_stream_alert(
        &mut self,
        origin: &str,
        owner: &IdentityId,
        project: &ProjectId,
        proposed: &ClientEnvelope,
    ) -> Result<Option<ClientEnvelope>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<Option<String>> = tx.query_row(
            "SELECT envelope FROM stream_alerts WHERE origin=?1 AND owner=?2 AND project=?3 AND alerts>0",
            params![origin, owner.as_str(), project.as_str()], |row| row.get(0),
        ).optional()?;
        let envelope = match current {
            None => return Ok(None),
            Some(Some(raw)) => serde_json::from_str(&raw)?,
            Some(None) => {
                tx.execute("UPDATE stream_alerts SET envelope=?4 WHERE origin=?1 AND owner=?2 AND project=?3", params![origin, owner.as_str(), project.as_str(), serde_json::to_string(proposed)?])?;
                proposed.clone()
            }
        };
        tx.commit()?;
        Ok(Some(envelope))
    }

    pub(crate) fn acknowledge_stream_alert(
        &self,
        origin: &str,
        owner: &IdentityId,
        project: &ProjectId,
        message: &MessageId,
    ) -> Result<(), StoreError> {
        self.conn.execute("UPDATE stream_alerts SET alerts=alerts-1,envelope=NULL WHERE origin=?1 AND owner=?2 AND project=?3 AND json_extract(envelope,'$.id')=?4 AND alerts>0", params![origin, owner.as_str(), project.as_str(), message.as_str()])?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn ingest(
        &mut self,
        origin: &str,
        project: &ProjectId,
        message: &StampedEnvelope,
    ) -> Result<(), StoreError> {
        self.ingest_for_owner(origin, project, message, None)
    }

    #[cfg(test)]
    pub(crate) fn ingest_for_owner(
        &mut self,
        origin: &str,
        project: &ProjectId,
        message: &StampedEnvelope,
        owner: Option<&IdentityId>,
    ) -> Result<(), StoreError> {
        let (_, remember) = self.ingest_tx(origin, project, message, owner, None)?;
        if let Some(id) = remember {
            self.remember_own(origin, &id, &message.client.id)?;
        }
        Ok(())
    }

    /// Like `ingest_for_owner`, but an identity whose stored metadata does not
    /// parse is skipped instead of failing the message, so the other
    /// recipients still get it and the position advances. `broken` maps the
    /// identities already known to be broken to a hash of their stored text; a
    /// changed text is parsed again. A message with a skipped identity and no
    /// other recipient that parses is refused, so nothing is lost silently.
    /// Returns the identities found broken by this message, which exist only
    /// once the transaction committed, and the error of the step after the
    /// commit, if it failed: the caller must report the former either way.
    pub(crate) fn ingest_skipping_broken(
        &mut self,
        origin: &str,
        project: &ProjectId,
        message: &StampedEnvelope,
        owner: &IdentityId,
        broken: &BTreeMap<IdentityId, u64>,
    ) -> Result<(IdentityFindings, Option<StoreError>), StoreError> {
        let (found, remember) =
            self.ingest_tx(origin, project, message, Some(owner), Some(broken))?;
        let after =
            remember.and_then(|id| self.remember_own(origin, &id, &message.client.id).err());
        Ok((found, after))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "routing, deduplication, quota and position commit in one transaction"
    )]
    fn ingest_tx(
        &mut self,
        origin: &str,
        project: &ProjectId,
        message: &StampedEnvelope,
        owner: Option<&IdentityId>,
        tolerate: Option<&BTreeMap<IdentityId, u64>>,
    ) -> Result<IngestOutcome, StoreError> {
        let mut found = Vec::new();
        let mut skipped_known = false;
        let mut delivered_to = 0_usize;
        let seq = i64::try_from(message.seq.0)
            .map_err(|_| StoreError::Invalid("sequence exceeds SQLite integer"))?;
        let raw = serde_json::to_string(message)?;
        let position_origin = position_namespace(origin, owner);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stream = message.stream.to_string();
        let previous: Option<i64> = tx
            .query_row(
                "SELECT seq FROM positions WHERE origin=?1 AND project=?2 AND stream=?3",
                params![position_origin, project.as_str(), stream],
                |r| r.get(0),
            )
            .optional()?;
        let last = previous.unwrap_or(0);
        if seq <= last {
            return Ok((found, None));
        }
        if seq != last + 1 {
            return Err(StoreError::Gap {
                expected: (last + 1) as u64,
                received: message.seq.0,
            });
        }
        if previous.is_none() {
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM positions WHERE project=?2 AND (origin=?1 OR substr(origin,1,length(?3))=?3)",
                params![origin, project.as_str(),format!("{origin}#owner=")],
                |r| r.get(0),
            )?;
            if count >= MAX_STREAM_POSITIONS as i64 {
                return Err(StoreError::Full("258 stream positions"));
            }
        }
        let mut remember = None;
        let mut stmt = tx.prepare("SELECT identity,owner,metadata FROM identities WHERE origin=?1 AND project=?2 AND (?3 IS NULL OR owner=?3) ORDER BY identity LIMIT ?4")?;
        let owner = owner.map(IdentityId::as_str);
        let identities = stmt.query_map(
            params![origin, project.as_str(), owner, MAX_MANAGED as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )?;
        for row in identities {
            let (id, owner, metadata) = row?;
            // An inbox can route only to its identity or an owning listener.
            // Keep the sender too, since its outgoing ids must be remembered.
            if let Stream::Inbox(recipient) = &message.stream
                && recipient.as_str() != id
                && recipient.as_str() != owner
                && message.from.as_str() != id
            {
                continue;
            }
            let id = id
                .parse::<IdentityId>()
                .map_err(|_| StoreError::Invalid("stored identity"))?;
            let identity: ManagedIdentity = match tolerate {
                None => serde_json::from_str(&metadata)?,
                Some(known) => {
                    let hash = metadata_hash(&metadata);
                    if known.get(&id) == Some(&hash) {
                        skipped_known = true;
                        continue;
                    }
                    match serde_json::from_str(&metadata) {
                        Ok(identity) => identity,
                        Err(_) => {
                            found.push((id, hash));
                            continue;
                        }
                    }
                }
            };
            if message.from == id {
                remember = Some(id);
                continue;
            }
            if !addressed_to(
                message,
                &id,
                &owner,
                identity.kind,
                self.own_ids.get(&(origin.to_owned(), id.clone())),
            )? {
                continue;
            }
            delivered_to += 1;
            let needs_receipt = identity.kind != IdentityKind::Listener
                && matches!(&message.client.payload, Payload::Assign { to, .. } if to == &id);
            let seen: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pending WHERE origin=?1 AND project=?2 AND identity=?3 AND sender=?4 AND id=?5) OR (?6 AND EXISTS(SELECT 1 FROM receipts WHERE origin=?1 AND project=?2 AND identity=?3 AND sender=?4 AND id=?5))",
                params![origin, project.as_str(), id.as_str(), message.from.as_str(), message.client.id.as_str(), needs_receipt], |r| r.get(0))?;
            if !seen {
                if raw.len() > crate::sse::MAX_EVENT {
                    return Err(StoreError::Full("message envelope bytes"));
                }
                let (pending, receipts): (i64, i64) = tx.query_row(
                    "SELECT (SELECT count(*) FROM pending WHERE origin=?1 AND identity=?2), (SELECT count(*) FROM receipts WHERE origin=?1 AND identity=?2 AND sender!='')",
                    params![origin, id.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?;
                if pending >= MAX_PENDING || (needs_receipt && receipts >= MAX_RECEIPTS) {
                    let changed = tx.execute("UPDATE identities SET metadata=json_set(metadata,'$.queue_full.full',1) WHERE origin=?1 AND identity=?2 AND json_extract(metadata,'$.queue_full.full') IS NULL", params![origin, id.as_str()])?;
                    if changed > 0 {
                        let limit = if pending >= MAX_PENDING {
                            QueueLimit::Pending
                        } else {
                            QueueLimit::Receipts
                        };
                        record_full_alert(&tx, origin, id.as_str(), limit)?;
                    }
                    // An ASSIGN also comes as a second stream copy. Record this copy's
                    // receipt when it fits, so the other copy is skipped too and the
                    // ASSIGN is not delivered late once the queue drains.
                    if !needs_receipt || receipts >= MAX_RECEIPTS {
                        continue;
                    }
                } else {
                    tx.execute("INSERT INTO pending(origin, project, identity, sender, id, message) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT DO NOTHING",
                        params![origin, project.as_str(), id.as_str(), message.from.as_str(), message.client.id.as_str(), raw])?;
                }
            }
            if needs_receipt {
                let (feed, inbox) = match message.stream {
                    Stream::Feed => (Some(seq), None),
                    Stream::Inbox(_) => (None, Some(seq)),
                };
                tx.execute("INSERT INTO receipts VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(origin,project,identity,sender,id) DO UPDATE SET feed=coalesce(excluded.feed,feed), inbox=coalesce(excluded.inbox,inbox)",
                    params![origin, project.as_str(), id.as_str(), message.from.as_str(), message.client.id.as_str(), feed, inbox])?;
                // Both copies commit contiguously; positions reject subsequent replays.
                tx.execute("DELETE FROM receipts WHERE origin=?1 AND project=?2 AND identity=?3 AND sender=?4 AND id=?5 AND feed IS NOT NULL AND inbox IS NOT NULL",
                    params![origin, project.as_str(), id.as_str(), message.from.as_str(), message.client.id.as_str()])?;
                rearm_queue_alert(&tx, origin, &id)?;
            }
        }
        drop(stmt);
        // A message nobody can read is not skipped: the position stays, and a
        // repair of any one recipient delivers it.
        if (skipped_known || !found.is_empty()) && delivered_to == 0 {
            return Err(StoreError::Invalid(
                "every recipient's stored state is unreadable",
            ));
        }
        tx.execute("INSERT INTO positions VALUES (?1,?2,?3,?4) ON CONFLICT(origin,project,stream) DO UPDATE SET seq=excluded.seq",
            params![position_origin, project.as_str(), stream, seq])?;
        tx.commit()?;
        Ok((found, remember))
    }

    /// Watches receive delivery IDs even when an outgoing ASK has no feed copy.
    pub(crate) fn remember_own(
        &mut self,
        origin: &str,
        identity: &IdentityId,
        id: &MessageId,
    ) -> Result<(), StoreError> {
        let key = (origin.to_owned(), identity.clone());
        if self.own_ids.len() >= MAX_MANAGED && !self.own_ids.contains_key(&key) {
            static FULL: std::sync::LazyLock<String> =
                std::sync::LazyLock::new(|| format!("{MAX_MANAGED} own-message trackers"));
            return Err(StoreError::Full(&FULL));
        }
        let ids = self.own_ids.entry(key).or_default();
        if !ids.contains(id) {
            if ids.len() == 1000 {
                ids.pop_front();
            }
            ids.push_back(id.clone());
        }
        Ok(())
    }

    pub(crate) fn drain(
        &mut self,
        origin: &str,
        identity: &IdentityId,
        limit: u32,
        byte_budget: usize,
    ) -> Result<Vec<StampedEnvelope>, StoreError> {
        self.drain_batch(origin, identity, limit, byte_budget, None)
    }

    pub(crate) fn drain_launch(
        &mut self,
        origin: &str,
        identity: &ManagedIdentity,
    ) -> Result<Vec<StampedEnvelope>, StoreError> {
        self.drain_batch(
            origin,
            &identity.identity_id,
            32,
            96 * 1024,
            Some(&identity.owner_id),
        )
    }

    fn drain_batch(
        &mut self,
        origin: &str,
        identity: &IdentityId,
        limit: u32,
        byte_budget: usize,
        owner: Option<&IdentityId>,
    ) -> Result<Vec<StampedEnvelope>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let query = if owner.is_some() {
            "SELECT n, message FROM pending WHERE origin=?1 AND identity=?2 ORDER BY CASE WHEN json_extract(message, '$.from')=?4 AND json_extract(message, '$.from_kind')='human' THEN 0 ELSE 1 END, n LIMIT ?3"
        } else {
            "SELECT n, message FROM pending WHERE origin=?1 AND identity=?2 ORDER BY n LIMIT ?3"
        };
        let mut stmt = tx.prepare(query)?;
        let decode = |r: &rusqlite::Row<'_>| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?));
        let rows = if let Some(owner) = owner {
            stmt.query_map(
                params![origin, identity.as_str(), limit.min(500), owner.as_str()],
                decode,
            )?
        } else {
            stmt.query_map(params![origin, identity.as_str(), limit.min(500)], decode)?
        };
        let budget = byte_budget.min(agmen_local::MAX_FRAME as usize - 4096);
        let mut bytes: usize = 2; // JSON array brackets.
        let mut selected = Vec::new();
        let mut result = Vec::new();
        let mut owner_batch = None;
        for row in rows {
            let (n, raw) = row?;
            if raw.len() > crate::sse::MAX_EVENT {
                return Err(StoreError::Invalid("stored envelope exceeds cap"));
            }
            let message: StampedEnvelope = serde_json::from_str(&raw)?;
            if let Some(owner) = owner {
                let is_owner = message.from == *owner && message.from_kind == IdentityKind::Human;
                if *owner_batch.get_or_insert(is_owner) != is_owner {
                    break;
                }
            }
            let size = serde_json::to_vec(&message)?.len() + usize::from(!result.is_empty());
            if size > budget.saturating_sub(bytes) {
                break;
            }
            bytes += size;
            selected.push(n);
            result.push(message);
        }
        drop(stmt);
        for n in selected {
            tx.execute("DELETE FROM pending WHERE n=?1", [n])?;
        }
        rearm_queue_alert(&tx, origin, identity)?;
        tx.commit()?;
        Ok(result)
    }
}

fn record_full_alert(
    tx: &rusqlite::Transaction<'_>,
    origin: &str,
    identity: &str,
    limit: QueueLimit,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE identities SET metadata=json_set(metadata,
         '$.queue_full.alerts',min(coalesce(json_extract(metadata,'$.queue_full.alerts'),0),?4-1)+1,
         ?3,min(coalesce(json_extract(metadata,?3),0),?4-1)+1) WHERE origin=?1 AND identity=?2",
        params![origin, identity, limit.counter(), MAX_QUEUE_ALERTS],
    )?;
    Ok(())
}

/// Whether the project has room for one more position, which also re-arms its
/// stream list ALERT.
fn rearm_stream_alert(
    tx: &rusqlite::Transaction<'_>,
    origin: &str,
    project: &ProjectId,
) -> Result<bool, StoreError> {
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM positions WHERE project=?2 AND (origin=?1 OR substr(origin,1,length(?3))=?3)",
        params![origin, project.as_str(), format!("{origin}#owner=")],
        |row| row.get(0),
    )?;
    let room = count < MAX_STREAM_POSITIONS as i64;
    if room {
        tx.execute(
            "UPDATE stream_alerts SET full=0 WHERE origin=?1 AND project=?2 AND full=1",
            params![origin, project.as_str()],
        )?;
    }
    Ok(room)
}

fn rearm_queue_alert(
    tx: &rusqlite::Transaction<'_>,
    origin: &str,
    identity: &IdentityId,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE identities SET metadata=json_remove(metadata,'$.queue_full.full')
         WHERE origin=?1 AND identity=?2 AND json_extract(metadata,'$.queue_full.full') IS NOT NULL
         AND (SELECT count(*) FROM pending WHERE origin=?1 AND identity=?2) < ?3
         AND (SELECT count(*) FROM receipts WHERE origin=?1 AND identity=?2 AND sender!='') < ?4",
        params![origin, identity.as_str(), MAX_PENDING, MAX_RECEIPTS],
    )?;
    Ok(())
}

/// Reuse the client's R31 routing rules, also implemented by the plugin.
fn addressed_to(
    message: &StampedEnvelope,
    id: &IdentityId,
    owner: &str,
    kind: IdentityKind,
    own_ids: Option<&VecDeque<MessageId>>,
) -> Result<bool, StoreError> {
    // ingest_for_owner skips an identity before decoding it when an inbox is
    // neither its own nor its owner's. Keep that skip in step with the inbox
    // rules below; inbox_ingest_skips_only_identities_the_addressing_rules_reject
    // checks they agree.
    if kind == IdentityKind::Listener {
        // Private fanout includes the owner; public messages have a feed copy.
        // Choose one durable copy even if another arrives after a drain/restart.
        let canonical = match &message.stream {
            Stream::Feed => true,
            Stream::Inbox(recipient) if recipient.as_str() == owner => {
                !matches!(message.client.payload, Payload::Assign { .. })
            }
            Stream::Inbox(recipient) if recipient == id => matches!(
                message.client.payload,
                Payload::Grant { .. } | Payload::Deny { .. }
            ),
            Stream::Inbox(_) => false,
        };
        if !canonical {
            return Ok(false);
        }
    } else if matches!(&message.stream, Stream::Inbox(recipient) if recipient != id) {
        return Ok(false);
    }
    let me = agmen_client::Me {
        identity_id: id.clone(),
        handle: String::new(),
        owner_id: owner
            .parse()
            .map_err(|_| StoreError::Invalid("stored owner identity"))?,
    };
    let addressed = me.is_for_me(message, |reply| {
        own_ids.is_some_and(|own| own.contains(reply))
    });
    // shortcut: L21-DR supplies the listener's delegated worker ids here.
    Ok(addressed
        || (kind == IdentityKind::Listener
            && crate::listener::includes(message, id, &me.owner_id, &[])))
}

/// What `ingest_tx` leaves for after the commit: the identities found broken,
/// and the sender whose own message id is still to be remembered.
type IngestOutcome = (IdentityFindings, Option<IdentityId>);

/// Identities found broken, each with a hash of its stored text.
pub(crate) type IdentityFindings = Vec<(IdentityId, u64)>;

/// Identifies one stored metadata text, so a re-registered identity is parsed again.
fn metadata_hash(metadata: &str) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    metadata.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
