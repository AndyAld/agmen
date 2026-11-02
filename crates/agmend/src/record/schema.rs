//! Private local record file and its independent schema version.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};

use super::{ACQUIRE_WAIT, Clock, SqliteStore};

pub(crate) const RECORD_DB: &str = "local.db";
pub(crate) const RECORD_SCHEMA: i64 = 1;
const BUSY_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub(crate) enum RecordError {
    #[error("local.db file operation failed")]
    Io(#[from] std::io::Error),
    #[error("local.db SQLite operation failed")]
    Sql(#[from] rusqlite::Error),
    #[error("local.db schema version {0} is unsupported")]
    Version(i64),
    #[error("invalid local.db: {0}")]
    Invalid(&'static str),
}

/// Synchronous opener; async callers must use the blocking pool.
pub(crate) fn open(path: &Path, clock: Clock) -> Result<SqliteStore, RecordError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(RecordError::Invalid("database path has no parent"))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    // SQLite inherits these permissions for the WAL and SHM sidecars.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(BUSY_WAIT)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !(0..=RECORD_SCHEMA).contains(&version) {
        return Err(RecordError::Version(version));
    }
    let mode: String = tx.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    let convert = !mode.eq_ignore_ascii_case("wal");
    if convert {
        tx.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
    }
    if version == 0 {
        tx.execute_batch(SCHEMA)?;
        tx.pragma_update(None, "user_version", RECORD_SCHEMA)?;
    }
    tx.commit()?;
    if convert {
        // Retain the migration lock until journal conversion is complete.
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(RecordError::Invalid("WAL unavailable"));
        }
        drop(conn);
        conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_WAIT)?;
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    Ok(SqliteStore {
        conn: Arc::new(tokio::sync::Mutex::new(conn)),
        clock,
        acquire_wait: ACQUIRE_WAIT,
    })
}

pub(super) const SCHEMA: &str = "
CREATE TABLE tenants (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE projects (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(id, tenant_id),
    UNIQUE(tenant_id, name)
) STRICT;
CREATE TABLE humans (
    id TEXT PRIMARY KEY NOT NULL,
    handle TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE local_origin (
    id INTEGER PRIMARY KEY CHECK(id=1),
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    person_id TEXT NOT NULL REFERENCES humans(id),
    created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE memberships (
    human_id TEXT NOT NULL REFERENCES humans(id),
    project_id TEXT NOT NULL REFERENCES projects(id),
    role TEXT NOT NULL CHECK(role IN ('admin','coordinator','member')),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(human_id, project_id)
) STRICT;
CREATE TABLE identities (
    id TEXT PRIMARY KEY NOT NULL,
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('agent','coordinator','listener')),
    handle TEXT NOT NULL,
    owner_human_id TEXT NOT NULL REFERENCES humans(id),
    created_at INTEGER NOT NULL,
    revoked_at INTEGER,
    role TEXT,
    verbs TEXT CHECK(verbs IS NULL OR (json_valid(verbs) AND json_type(verbs)='array')),
    edits TEXT CHECK(edits IS NULL OR json_valid(edits)),
    role_sha256 TEXT,
    agent TEXT,
    delegated_by TEXT REFERENCES identities(id),
    FOREIGN KEY(project_id, tenant_id) REFERENCES projects(id, tenant_id),
    CHECK((role IS NULL AND verbs IS NULL AND edits IS NULL AND role_sha256 IS NULL)
       OR (role IS NOT NULL AND verbs IS NOT NULL AND edits IS NOT NULL AND role_sha256 IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX identities_live_handle ON identities(project_id, handle)
    WHERE revoked_at IS NULL;
CREATE INDEX identities_owner ON identities(owner_human_id);
CREATE UNIQUE INDEX identities_one_listener ON identities(owner_human_id, project_id)
    WHERE kind='listener' AND revoked_at IS NULL;
CREATE TABLE stream_heads (
    stream TEXT PRIMARY KEY NOT NULL,
    seq INTEGER NOT NULL CHECK(seq>=0),
    pruned_through INTEGER NOT NULL DEFAULT 0 CHECK(pruned_through=0)
) STRICT;
CREATE TABLE messages (
    project_id TEXT NOT NULL REFERENCES projects(id),
    stream TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK(seq>=1),
    id TEXT NOT NULL,
    from_id TEXT NOT NULL,
    payload_hash BLOB NOT NULL,
    envelope TEXT NOT NULL CHECK(json_valid(envelope)),
    hub_at INTEGER NOT NULL,
    ack_row INTEGER NOT NULL CHECK(ack_row IN (0,1)),
    PRIMARY KEY(project_id, stream, seq),
    UNIQUE(project_id, stream, from_id, id)
) STRICT;
CREATE UNIQUE INDEX messages_ack_row ON messages(from_id, id) WHERE ack_row=1;
CREATE INDEX messages_by_id ON messages(project_id, id);
CREATE INDEX messages_latest_hello ON messages(project_id, from_id, seq DESC)
    WHERE stream='feed' AND json_extract(envelope,'$.verb')='HELLO';
CREATE TABLE lanes (
    project_id TEXT NOT NULL REFERENCES projects(id),
    name TEXT NOT NULL,
    globs TEXT NOT NULL CHECK(json_valid(globs) AND json_type(globs)='array' AND json_array_length(globs)>=1),
    owner_identity_id TEXT REFERENCES identities(id),
    v INTEGER NOT NULL CHECK(v>=1),
    PRIMARY KEY(project_id, name)
) STRICT;
CREATE TABLE items (
    project_id TEXT NOT NULL REFERENCES projects(id),
    key TEXT NOT NULL,
    title TEXT NOT NULL,
    lane TEXT,
    severity TEXT CHECK(severity IN ('low','med','high','security')),
    state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','done')),
    created_by TEXT NOT NULL,
    assigned_to TEXT REFERENCES identities(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(project_id, key)
) STRICT;
CREATE TABLE item_counters (
    project_id TEXT NOT NULL REFERENCES projects(id),
    prefix TEXT NOT NULL,
    n INTEGER NOT NULL CHECK(n BETWEEN 1 AND 999999),
    PRIMARY KEY(project_id, prefix)
) STRICT;
CREATE TABLE claims (
    id INTEGER PRIMARY KEY,
    project_id TEXT NOT NULL,
    item_key TEXT NOT NULL,
    identity_id TEXT NOT NULL REFERENCES identities(id),
    granted_at INTEGER NOT NULL,
    lease_until INTEGER NOT NULL,
    released_at INTEGER,
    FOREIGN KEY(project_id, item_key) REFERENCES items(project_id, key),
    CHECK(lease_until>=granted_at),
    CHECK(released_at IS NULL OR released_at>=granted_at)
) STRICT;
CREATE UNIQUE INDEX claims_active ON claims(project_id, item_key) WHERE released_at IS NULL;
CREATE INDEX claims_expiring ON claims(lease_until) WHERE released_at IS NULL;
CREATE INDEX claims_holder ON claims(identity_id) WHERE released_at IS NULL;
CREATE TABLE lane_lends (
    project_id TEXT NOT NULL,
    lane TEXT NOT NULL,
    message_id TEXT NOT NULL,
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL REFERENCES identities(id),
    paths TEXT NOT NULL CHECK(json_valid(paths) AND json_type(paths)='array'),
    until_at INTEGER NOT NULL,
    PRIMARY KEY(project_id, from_id, message_id),
    FOREIGN KEY(project_id, lane) REFERENCES lanes(project_id, name) ON DELETE CASCADE
) STRICT;
CREATE INDEX lane_lends_recipient ON lane_lends(project_id, to_id, until_at);
CREATE TABLE tenant_usage (
    tenant_id TEXT NOT NULL REFERENCES tenants(id),
    day TEXT NOT NULL,
    messages INTEGER NOT NULL CHECK(messages>=0),
    warned INTEGER NOT NULL DEFAULT 0 CHECK(warned IN (0,1)),
    PRIMARY KEY(tenant_id, day)
) STRICT;
";
