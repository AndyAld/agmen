//! Durable daemon metadata, pending notifications and receive positions.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::{Duration, Instant};

use agmen_client::Broker;
use agmen_proto::{
    IdentityId, IdentityKind, Payload, ProjectId, StampedEnvelope, Stream, TenantId,
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
        Ok(Self { conn })
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "identity registration lands in B T3")
    )]
    pub(crate) fn register(&mut self, identity: &ManagedIdentity) -> Result<(), StoreError> {
        let origin = identity.hub_url.origin().ascii_serialization();
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
            if count >= 64 {
                return Err(StoreError::Full("64 managed identities"));
            }
            let projects: i64 = tx.query_row(
                "SELECT count(*) FROM (SELECT DISTINCT origin, project FROM identities UNION SELECT ?1, ?2)",
                params![origin, identity.project.as_str()], |r| r.get(0))?;
            if projects > 64 {
                return Err(StoreError::Full("64 managed projects"));
            }
        }
        tx.execute("INSERT INTO identities VALUES (?1,?2,?3,?4,?5) ON CONFLICT(origin,identity) DO UPDATE SET metadata=excluded.metadata",
            params![origin, identity.identity_id.as_str(), identity.owner_id.as_str(), identity.project.as_str(), serde_json::to_string(identity)?])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn identities(
        &self,
        origin: &str,
        owner: &IdentityId,
    ) -> Result<Vec<ManagedIdentity>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT metadata FROM identities WHERE origin=?1 AND owner=?2 ORDER BY identity LIMIT 64")?;
        let mut result = Vec::new();
        for row in stmt.query_map(params![origin, owner.as_str()], |r| r.get::<_, String>(0))? {
            result.push(serde_json::from_str(&row?)?);
        }
        Ok(result)
    }

    pub(crate) fn positions(
        &self,
        origin: &str,
        project: &ProjectId,
    ) -> Result<BTreeMap<Stream, u64>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT stream, seq FROM positions WHERE origin=?1 AND project=?2 ORDER BY stream LIMIT 256")?;
        let mut result = BTreeMap::new();
        for row in stmt.query_map(params![origin, project.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
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

    #[cfg(test)]
    pub(crate) fn ingest(
        &mut self,
        origin: &str,
        project: &ProjectId,
        message: &StampedEnvelope,
    ) -> Result<(), StoreError> {
        self.ingest_for_owner(origin, project, message, None)
    }

    pub(crate) fn ingest_for_owner(
        &mut self,
        origin: &str,
        project: &ProjectId,
        message: &StampedEnvelope,
        owner: Option<&IdentityId>,
    ) -> Result<(), StoreError> {
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
            return Ok(());
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
            if count >= 256 {
                return Err(StoreError::Full("256 stream positions"));
            }
        }
        let mut stmt = tx.prepare("SELECT identity FROM identities WHERE origin=?1 AND project=?2 AND (?3 IS NULL OR owner=?3) ORDER BY identity LIMIT 64")?;
        let identities = stmt.query_map(
            params![origin, project.as_str(), owner.map(IdentityId::as_str)],
            |r| r.get::<_, String>(0),
        )?;
        for row in identities {
            let id = row?
                .parse::<IdentityId>()
                .map_err(|_| StoreError::Invalid("stored identity"))?;
            if message.from == id
                || matches!(&message.stream, Stream::Inbox(recipient) if recipient != &id)
            {
                continue;
            }
            let qualifies = message.mentioned.contains(&id)
                || match &message.client.payload {
                    Payload::Dm {} => message.client.to.contains(&id),
                    Payload::Assign { to, .. } => to == &id,
                    _ => false,
                };
            if !qualifies {
                continue;
            }
            let needs_receipt =
                matches!(&message.client.payload, Payload::Assign { to, .. } if to == &id);
            let seen: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pending WHERE origin=?1 AND project=?2 AND identity=?3 AND sender=?4 AND id=?5) OR (?6 AND EXISTS(SELECT 1 FROM receipts WHERE origin=?1 AND project=?2 AND identity=?3 AND sender=?4 AND id=?5))",
                params![origin, project.as_str(), id.as_str(), message.from.as_str(), message.client.id.as_str(), needs_receipt], |r| r.get(0))?;
            if !seen {
                if raw.len() > crate::sse::MAX_EVENT {
                    return Err(StoreError::Full("message envelope bytes"));
                }
                let (pending, receipts): (i64, i64) = tx.query_row(
                    "SELECT (SELECT count(*) FROM pending WHERE origin=?1 AND identity=?2), (SELECT count(*) FROM receipts WHERE origin=?1 AND identity=?2 AND sender!='')",
                    params![origin, id.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?;
                if pending >= 10_000 {
                    return Err(StoreError::Full("10000 pending rows"));
                }
                if needs_receipt && receipts >= 20_000 {
                    return Err(StoreError::Full(
                        "20000 receipts; awaiting other stream copies",
                    ));
                }
                tx.execute("INSERT INTO pending(origin, project, identity, sender, id, message) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT DO NOTHING",
                    params![origin, project.as_str(), id.as_str(), message.from.as_str(), message.client.id.as_str(), raw])?;
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
            }
        }
        drop(stmt);
        tx.execute("INSERT INTO positions VALUES (?1,?2,?3,?4) ON CONFLICT(origin,project,stream) DO UPDATE SET seq=excluded.seq",
            params![position_origin, project.as_str(), stream, seq])?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn drain(
        &mut self,
        origin: &str,
        identity: &IdentityId,
        limit: u32,
        byte_budget: usize,
    ) -> Result<Vec<StampedEnvelope>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut stmt = tx.prepare(
            "SELECT n, message FROM pending WHERE origin=?1 AND identity=?2 ORDER BY n LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![origin, identity.as_str(), limit.min(500)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        let budget = byte_budget.min(agmen_local::MAX_FRAME as usize - 4096);
        let mut bytes: usize = 2; // JSON array brackets.
        let mut selected = Vec::new();
        let mut result = Vec::new();
        for row in rows {
            let (n, raw) = row?;
            if raw.len() > crate::sse::MAX_EVENT {
                return Err(StoreError::Invalid("stored envelope exceeds cap"));
            }
            let message: StampedEnvelope = serde_json::from_str(&raw)?;
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
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
