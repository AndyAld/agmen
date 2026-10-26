//! The identity's SQLite file, which holds the outbox and the cursors. The
//! MCP server, `agmen watch` and one-shot CLI commands of one identity open
//! it at the same time, each with its own connections.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};

use crate::error::ClientError;

/// How long a connection waits for another process's write lock before
/// it fails with `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The schema version this build writes, kept in `PRAGMA user_version`.
const SCHEMA_VERSION: i64 = 2;

/// Version 1. `outbox.n` keeps enqueue order. A pending row is due once
/// `next_at_ms` (Unix milliseconds) has passed; `recorded` rows carry
/// where the hub recorded them and `rejected` rows the final NACK reason.
/// `cursors` holds, per stream, the last seq `agmen watch` delivered and the
/// last seq the inbox listed (ruling R45).
const SCHEMA_V1: &str = "
CREATE TABLE outbox (
    n INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    envelope TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'recorded', 'rejected')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_at_ms INTEGER NOT NULL,
    stream TEXT,
    seq INTEGER CHECK (seq > 0),
    reason TEXT,
    CHECK ((state = 'recorded') = (stream IS NOT NULL AND seq IS NOT NULL)),
    CHECK ((state = 'rejected') = (reason IS NOT NULL))
) STRICT;
CREATE INDEX outbox_due ON outbox (next_at_ms) WHERE state = 'pending';
CREATE TABLE cursors (
    stream TEXT PRIMARY KEY,
    delivered INTEGER NOT NULL DEFAULT 0 CHECK (delivered >= 0),
    read INTEGER NOT NULL DEFAULT 0 CHECK (read >= 0)
) STRICT;
";

/// Version 2, applied on top of version 1. `hold_until_ms` is when the
/// delay the hub named in the last retryable NACK (`retry_after_ms`) runs
/// out, in Unix milliseconds, or NULL when the hub named none. A reconnect
/// does not resend a row before its hold runs out.
const SCHEMA_V2: &str = "ALTER TABLE outbox ADD COLUMN hold_until_ms INTEGER;";

/// One connection to the identity's file, shared by the clones of an
/// `Outbox` or `Cursors`.
#[derive(Debug, Clone)]
pub(crate) struct Db(Arc<Mutex<Connection>>);

impl Db {
    /// Opens (creating it if needed) the file at `path` in WAL mode with
    /// `synchronous=FULL` and a 5 s busy timeout, and creates the tables or
    /// brings an older file's tables up to [`SCHEMA_VERSION`].
    pub(crate) fn open(path: &Path) -> Result<Self, ClientError> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let mode = wal_mode(&conn)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(ClientError::StoreMode(mode));
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        // IMMEDIATE takes the write lock first, so two processes opening a
        // new file one after the other do not both create the tables.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        match version {
            0 => {
                tx.execute_batch(SCHEMA_V1)?;
                tx.execute_batch(SCHEMA_V2)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            1 => {
                tx.execute_batch(SCHEMA_V2)?;
                tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            SCHEMA_VERSION => {}
            newer => return Err(ClientError::StoreVersion(newer)),
        }
        tx.commit()?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    /// The connection. A panic while it was held leaves it usable: any
    /// open transaction rolled back when its guard dropped.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Switches the file to WAL and returns the journal mode it is in. Turning
/// a new file into WAL needs an exclusive lock, and SQLite answers BUSY at
/// once, without its busy handler, while another process opens the same
/// new file; so this retries for up to [`BUSY_TIMEOUT`].
fn wal_mode(conn: &Connection) -> Result<String, ClientError> {
    const PAUSE: Duration = Duration::from_millis(10);
    let tries = BUSY_TIMEOUT.as_millis() / PAUSE.as_millis();
    let mut tried = 0;
    loop {
        match conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0)) {
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::DatabaseBusy && tried < tries =>
            {
                tried += 1;
                std::thread::sleep(PAUSE);
            }
            result => return Ok(result?),
        }
    }
}

/// `seq` as SQLite's signed integer.
pub(crate) fn to_sql_seq(seq: u64) -> Result<i64, ClientError> {
    i64::try_from(seq).map_err(|_| ClientError::SeqOutOfRange(seq))
}

/// A stored seq or count, which the schema keeps non-negative.
pub(crate) fn from_sql_int(value: i64) -> Result<u64, ClientError> {
    u64::try_from(value).map_err(|_| ClientError::Stored(format!("negative value {value}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_1_file_gains_the_hold_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO outbox (id, envelope, next_at_ms) VALUES ('a', '{}', 7)",
                [],
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let conn = db.lock();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let row: (i64, Option<i64>) = conn
            .query_row(
                "SELECT next_at_ms, hold_until_ms FROM outbox WHERE id = 'a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (7, None), "the old row is kept, with no hold");
    }

    #[test]
    fn a_newer_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.db");
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(
            Db::open(&path),
            Err(ClientError::StoreVersion(v)) if v == SCHEMA_VERSION + 1
        ));
    }
}
