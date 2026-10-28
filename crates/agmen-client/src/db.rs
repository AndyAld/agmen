//! The identity's SQLite file, which holds the outbox and the cursors. The
//! MCP server, `agmen watch` and one-shot CLI commands of one identity open
//! it at the same time, each with its own connections.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::{Connection, TransactionBehavior};

use crate::error::ClientError;

/// How long a connection waits for another process's write lock before
/// it fails with `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The schema version this build writes, kept in `PRAGMA user_version`.
const SCHEMA_VERSION: i64 = 2;

/// Inspects an existing store without creating it, migrating it or changing its journal mode.
///
/// # Errors
///
/// Returns an I/O/format error if the schema cannot be inspected without writes.
/// A future version is returned unchanged so callers can assess rollback safety.
pub fn store_schema(path: &Path, timeout: Duration) -> Result<Option<i64>, ClientError> {
    let deadline = Instant::now() + timeout.min(BUSY_TIMEOUT);
    // At most 501 snapshots, and at most the caller's remaining five-second budget.
    for _ in 0..501 {
        match inspect_schema(path, deadline) {
            Err(ClientError::StoreChanged) if Instant::now() < deadline => {
                std::thread::sleep(
                    Duration::from_millis(10)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            result => return result,
        }
    }
    Err(ClientError::StoreChanged)
}

// SQLite itself may create a WAL/SHM even on a read-only connection. Inspection
// uses only File::open and validates committed WAL frames instead:
// https://sqlite.org/fileformat2.html#walformat
fn inspect_schema(path: &Path, deadline: Instant) -> Result<Option<i64>, ClientError> {
    use std::io::Read as _;
    let absolute = match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ClientError::StoreInspect(error)),
    };
    let header = || -> Result<Vec<u8>, ClientError> {
        let mut bytes = Vec::with_capacity(100);
        open_readonly(&absolute)?
            .ok_or(ClientError::StoreChanged)?
            .take(100)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let sidecar = |suffix: &str| {
        let mut path = absolute.as_os_str().to_owned();
        path.push(suffix);
        std::path::PathBuf::from(path)
    };
    let before = header()?;
    // A hot rollback journal requires recovery, which inspection must not perform.
    if let Some(journal) = open_readonly(&sidecar("-journal"))? {
        let mut magic = Vec::with_capacity(8);
        journal.take(8).read_to_end(&mut magic)?;
        if magic == [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7] {
            return Err(ClientError::StoreJournal);
        }
    }
    let main_version = header_version(&before)?;
    let wal_path = sidecar("-wal");
    let wal = open_readonly(&wal_path)?;
    let version = if let Some(file) = wal {
        let stamp = file_stamp(&file.metadata()?)?;
        let version = if stamp.len == 0 {
            main_version
        } else {
            wal_version(file, main_version, deadline)?
        };
        let after = std::fs::metadata(&wal_path).map_err(|_| ClientError::StoreChanged)?;
        if file_stamp(&after)? != stamp {
            return Err(ClientError::StoreChanged);
        }
        version
    } else {
        if let Some(file) = open_readonly(&wal_path)?
            && file.metadata()?.len() > 0
        {
            return Err(ClientError::StoreChanged);
        }
        main_version
    };
    if before != header()? {
        return Err(ClientError::StoreChanged);
    }
    Ok(Some(version))
}

fn open_readonly(path: &Path) -> Result<Option<std::fs::File>, ClientError> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ClientError::StoreInspect(error)),
    };
    if !metadata.is_file() {
        return Err(invalid_schema("store or journal is not a regular file"));
    }
    match std::fs::File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(ClientError::StoreChanged)
        }
        Err(error) => Err(ClientError::StoreInspect(error)),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity: (u64, u64),
}

fn file_stamp(metadata: &std::fs::Metadata) -> Result<FileStamp, ClientError> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;
    Ok(FileStamp {
        len: metadata.len(),
        modified: metadata.modified()?,
        #[cfg(unix)]
        identity: (metadata.dev(), metadata.ino()),
    })
}

fn invalid_schema(reason: &'static str) -> ClientError {
    ClientError::StoreInspect(std::io::Error::new(std::io::ErrorKind::InvalidData, reason))
}

fn header_version(header: &[u8]) -> Result<i64, ClientError> {
    if header.is_empty() {
        return Ok(0);
    }
    if header.len() != 100 || &header[..16] != b"SQLite format 3\0" {
        return Err(invalid_schema("invalid SQLite database header"));
    }
    Ok(i64::from(i32::from_be_bytes([
        header[60], header[61], header[62], header[63],
    ])))
}

fn be_word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn wal_checksum(bytes: &[u8], little: bool, mut sum: [u32; 2]) -> [u32; 2] {
    // Inputs are fixed WAL header fields or a validated page of at most 64 KiB.
    let (pairs, rest) = bytes.as_chunks::<8>();
    assert!(
        rest.is_empty(),
        "WAL checksum input is not a multiple of eight"
    );
    for pair in pairs {
        let first = if little {
            u32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]])
        } else {
            be_word(pair, 0)
        };
        let second = if little {
            u32::from_le_bytes([pair[4], pair[5], pair[6], pair[7]])
        } else {
            be_word(pair, 4)
        };
        sum[0] = sum[0].wrapping_add(first).wrapping_add(sum[1]);
        sum[1] = sum[1].wrapping_add(second).wrapping_add(sum[0]);
    }
    sum
}

fn wal_version(file: std::fs::File, main: i64, deadline: Instant) -> Result<i64, ClientError> {
    use std::io::Read as _;
    let length = file.metadata()?.len();
    if length < 32 {
        return Err(ClientError::StoreChanged);
    }
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut header = [0_u8; 32];
    reader.read_exact(&mut header)?;
    let little = match be_word(&header, 0) {
        0x377f_0682 => true,
        0x377f_0683 => false,
        _ => return Err(invalid_schema("invalid SQLite WAL magic")),
    };
    if be_word(&header, 4) != 3_007_000 {
        return Err(invalid_schema("unsupported SQLite WAL format"));
    }
    let size = be_word(&header, 8);
    if !(512..=65536).contains(&size) || !size.is_power_of_two() {
        return Err(invalid_schema("invalid SQLite WAL page size"));
    }
    let mut sum = wal_checksum(&header[..24], little, [0, 0]);
    if sum != [be_word(&header, 24), be_word(&header, 28)] {
        return Err(invalid_schema("invalid SQLite WAL header checksum"));
    }
    let frames = (length - 32) / (24 + u64::from(size));
    if frames > 1_000_000 {
        return Err(invalid_schema(
            "schema inspection exceeds one million WAL frames",
        ));
    }
    let mut page = vec![0_u8; size as usize];
    let mut committed = main;
    let mut have_commit = false;
    let mut pending = None;
    for _ in 0..frames {
        if Instant::now() >= deadline {
            return Err(ClientError::StoreChanged);
        }
        let mut frame = [0_u8; 24];
        reader.read_exact(&mut frame)?;
        // Old frames after a WAL reset are outside the current generation.
        if frame[8..16] != header[16..24] {
            break;
        }
        reader.read_exact(&mut page)?;
        sum = wal_checksum(&frame[..8], little, sum);
        sum = wal_checksum(&page, little, sum);
        if sum != [be_word(&frame, 16), be_word(&frame, 20)] {
            // Rollback can leave same-salt frames beyond a shorter commit.
            if have_commit {
                break;
            }
            return Err(invalid_schema("invalid SQLite WAL frame checksum"));
        }
        let number = be_word(&frame, 0);
        if number == 0 {
            return Err(invalid_schema("invalid SQLite WAL page number"));
        }
        if number == 1 {
            pending = Some(header_version(&page[..100])?);
        }
        if be_word(&frame, 4) != 0 {
            have_commit = true;
            if let Some(version) = pending.take() {
                committed = version;
            }
        }
    }
    // Detect reset/replacement while reading, even when the file length is unchanged.
    use std::io::{Seek as _, SeekFrom};
    let mut after = [0_u8; 32];
    reader.seek(SeekFrom::Start(0))?;
    reader.read_exact(&mut after)?;
    if after != header {
        return Err(ClientError::StoreChanged);
    }
    Ok(committed)
}

/// [`store_schema`] for a caller that is about to open the store with SQLite
/// and recheck the version under its migration lock.
///
/// A rollback journal means a live writer (SQLite's busy handler waits for
/// it) or a crash, which SQLite must recover before anyone can read a
/// consistent version; refusing it would leave the store unopenable for good.
/// Recovery only restores the last committed state, then the caller's
/// locked check refuses a newer schema before it migrates or converts.
///
/// # Errors
///
/// As [`store_schema`], except that a rollback journal yields `Ok(None)`.
pub fn preflight_schema(path: &Path, timeout: Duration) -> Result<Option<i64>, ClientError> {
    match store_schema(path, timeout) {
        Err(ClientError::StoreJournal) => Ok(None),
        result => result,
    }
}

fn check_version(version: i64) -> Result<(), ClientError> {
    if !(0..=SCHEMA_VERSION).contains(&version) {
        return Err(ClientError::StoreVersion(version));
    }
    Ok(())
}

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
        if let Some(version) = preflight_schema(path, BUSY_TIMEOUT)? {
            check_version(version)?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // EXCLUSIVE takes the write lock first, so two processes opening a
        // new file one after the other do not both create the tables.
        // In WAL this is equivalent to IMMEDIATE. In DELETE it also protects
        // the subsequent journal conversion against another schema writer.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        check_version(version)?;
        let mode: String = tx.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        let convert = !mode.eq_ignore_ascii_case("wal");
        if convert {
            tx.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
        }
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
        if convert {
            // Exclusive locking mode keeps the migration lock through journal conversion.
            let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
            if !mode.eq_ignore_ascii_case("wal") {
                return Err(ClientError::StoreMode(mode));
            }
            drop(conn);
            conn = Connection::open(path)?;
            conn.busy_timeout(BUSY_TIMEOUT)?;
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    /// The connection. A panic while it was held leaves it usable: any
    /// open transaction rolled back when its guard dropped.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
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
    fn newer_schema_refused_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.db");
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            Db::open(&path),
            Err(ClientError::StoreVersion(99))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn newer_wal_schema_refused_without_sidecar_writes() {
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("source.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA user_version=99;")
            .unwrap();
        for with_shm in [false, true] {
            let fixture = tempfile::tempdir().unwrap();
            let target = fixture.path().join("store?literal#.db");
            let suffixes = if with_shm {
                vec!["", "-wal", "-shm"]
            } else {
                vec!["", "-wal"]
            };
            let before: Vec<_> = suffixes
                .iter()
                .map(|suffix| {
                    let bytes = std::fs::read(format!("{}{suffix}", path.display())).unwrap();
                    let target = std::path::PathBuf::from(format!("{}{suffix}", target.display()));
                    std::fs::write(&target, &bytes).unwrap();
                    (target, bytes)
                })
                .collect();
            let answer = Db::open(&target);
            assert!(
                matches!(answer, Err(ClientError::StoreVersion(99))),
                "{answer:?}"
            );
            assert_eq!(
                std::fs::read_dir(fixture.path()).unwrap().count(),
                before.len()
            );
            for (file, bytes) in before {
                assert_eq!(std::fs::read(file).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn schema_inspection_leaves_clean_wal_without_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clean.db");
        drop(Db::open(&path).unwrap());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let before = std::fs::read(&path).unwrap();
        assert_eq!(store_schema(&path, BUSY_TIMEOUT).unwrap(), Some(2));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn schema_inspection_follows_database_symlink_to_its_wal() {
        let source = tempfile::tempdir().unwrap();
        let aliases = tempfile::tempdir().unwrap();
        let path = source.path().join("real.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA user_version=99;")
            .unwrap();
        let alias = aliases.path().join("agmen.db");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert_eq!(store_schema(&alias, BUSY_TIMEOUT).unwrap(), Some(99));
        assert!(matches!(
            Db::open(&alias),
            Err(ClientError::StoreVersion(99))
        ));
        assert_eq!(std::fs::read_dir(aliases.path()).unwrap().count(), 1);
    }

    #[test]
    fn schema_inspection_matches_sqlite_commits_and_wal_resets() {
        for size in [512, 4096, 65536] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("versions.db");
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "PRAGMA page_size={size}; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;"
            ))
            .unwrap();
            for version in [1, 99, 2, -1, 3] {
                conn.pragma_update(None, "user_version", version).unwrap();
                assert_eq!(store_schema(&path, BUSY_TIMEOUT).unwrap(), Some(version));
                conn.execute_batch("PRAGMA wal_checkpoint(RESTART)")
                    .unwrap();
                assert_eq!(store_schema(&path, BUSY_TIMEOUT).unwrap(), Some(version));
            }
            drop(conn);
            assert_eq!(store_schema(&path, BUSY_TIMEOUT).unwrap(), Some(3));
        }
    }

    #[test]
    fn schema_inspection_distinguishes_cold_and_hot_rollback_journals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("persist.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=PERSIST; PRAGMA user_version=2;")
            .unwrap();
        drop(conn);
        let journal = std::path::PathBuf::from(format!("{}-journal", path.display()));
        assert!(std::fs::metadata(&journal).unwrap().len() > 0);
        assert_eq!(
            store_schema(&path, Duration::from_millis(20)).unwrap(),
            Some(2)
        );
        let hot = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
        std::fs::write(&journal, hot).unwrap();
        assert!(matches!(
            store_schema(&path, Duration::from_millis(20)),
            Err(ClientError::StoreJournal)
        ));
        assert_eq!(std::fs::read(&journal).unwrap(), hot);
    }

    #[test]
    fn open_recovers_hot_rollback_journal_left_by_a_crash() {
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("crashed.db");
        drop(Db::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA cache_size=1; CREATE TABLE pad(payload BLOB); BEGIN IMMEDIATE;",
        )
        .unwrap();
        for _ in 0..40 {
            conn.execute("INSERT INTO pad VALUES (zeroblob(4096))", [])
                .unwrap();
        }
        conn.pragma_update(None, "user_version", 1).unwrap();
        // Copy the files mid-transaction: the copy is what a crash leaves behind.
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("crashed.db");
        for suffix in ["", "-journal"] {
            std::fs::copy(
                format!("{}{suffix}", path.display()),
                format!("{}{suffix}", target.display()),
            )
            .unwrap();
        }
        conn.execute_batch("ROLLBACK").unwrap();
        let db = Db::open(&target).unwrap();
        let version: i64 = db
            .lock()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn schema_inspection_matches_sqlite_after_rolled_back_wal_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollback.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE data(payload BLOB); PRAGMA user_version=2; PRAGMA cache_size=1; BEGIN IMMEDIATE;").unwrap();
        for _ in 0..40 {
            conn.execute("INSERT INTO data VALUES (zeroblob(4096))", [])
                .unwrap();
        }
        conn.pragma_update(None, "user_version", 99).unwrap();
        conn.execute_batch("ROLLBACK; PRAGMA user_version=3;")
            .unwrap();
        let expected: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(expected, 3);
        assert_eq!(store_schema(&path, BUSY_TIMEOUT).unwrap(), Some(expected));
    }

    #[test]
    fn schema_inspection_validates_wal_and_ignores_uncommitted_page_one() {
        let source = tempfile::tempdir().unwrap();
        let original = source.path().join("source.db");
        let conn = Connection::open(&original).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA user_version=99;")
            .unwrap();
        let wal = std::fs::read(format!("{}-wal", original.display())).unwrap();
        let size = be_word(&wal, 8) as usize;
        let width = 24 + size;
        assert_eq!(wal.len(), 32 + width);
        let little = be_word(&wal, 0) == 0x377f_0682;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("store.db");
        std::fs::copy(&original, &path).unwrap();
        let wal_path = std::path::PathBuf::from(format!("{}-wal", path.display()));
        let mut big = wal.clone();
        big[..4].copy_from_slice(&0x377f_0683_u32.to_be_bytes());
        let sum = wal_checksum(&big[..24], false, [0, 0]);
        big[24..28].copy_from_slice(&sum[0].to_be_bytes());
        big[28..32].copy_from_slice(&sum[1].to_be_bytes());
        let sum = wal_checksum(&big[32..40], false, sum);
        let sum = wal_checksum(&big[56..], false, sum);
        big[48..52].copy_from_slice(&sum[0].to_be_bytes());
        big[52..56].copy_from_slice(&sum[1].to_be_bytes());
        std::fs::write(&wal_path, &big).unwrap();
        assert_eq!(store_schema(&path, BUSY_TIMEOUT).unwrap(), Some(99));
        let mut frame = wal[32..].to_vec();
        frame[4..8].copy_from_slice(&0_u32.to_be_bytes());
        frame[24 + 60..24 + 64].copy_from_slice(&123_i32.to_be_bytes());
        let previous = [be_word(&wal, 32 + 16), be_word(&wal, 32 + 20)];
        let sum = wal_checksum(&frame[..8], little, previous);
        let sum = wal_checksum(&frame[24..], little, sum);
        frame[16..20].copy_from_slice(&sum[0].to_be_bytes());
        frame[20..24].copy_from_slice(&sum[1].to_be_bytes());
        let mut extended = wal.clone();
        extended.extend_from_slice(&frame);
        std::fs::write(&wal_path, &extended).unwrap();
        assert_eq!(store_schema(&path, BUSY_TIMEOUT).unwrap(), Some(99));
        extended[32 + width + 24 + 60] ^= 1;
        std::fs::write(&wal_path, &extended).unwrap();
        assert_eq!(store_schema(&path, BUSY_TIMEOUT).unwrap(), Some(99));
        for offset in [0, 4, 8, 24, 32 + 16, 32 + 24 + 60] {
            let mut bad = wal.clone();
            bad[offset] ^= 1;
            std::fs::write(&wal_path, &bad).unwrap();
            assert!(
                matches!(
                    store_schema(&path, BUSY_TIMEOUT),
                    Err(ClientError::StoreInspect(_))
                ),
                "offset {offset}"
            );
        }
        std::fs::write(&wal_path, &wal[..20]).unwrap();
        assert!(store_schema(&path, Duration::from_millis(20)).is_err());
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 2);
    }

    #[test]
    fn schema_inspection_during_checkpoint_and_wal_removal_creates_no_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.db");
        drop(Db::open(&path).unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let worker_path = path.clone();
        let writer = std::thread::spawn(move || {
            worker_barrier.wait();
            for _ in 0..50 {
                let conn = Connection::open(&worker_path).unwrap();
                conn.pragma_update(None, "user_version", 99).unwrap();
                conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                    .unwrap();
                drop(conn);
            }
        });
        barrier.wait();
        for _ in 0..100 {
            if let Ok(Some(version)) = store_schema(&path, Duration::from_millis(20)) {
                assert!([2, 99].contains(&version));
            }
        }
        writer.join().unwrap();
        let before = std::fs::read(&path).unwrap();
        assert_eq!(store_schema(&path, BUSY_TIMEOUT).unwrap(), Some(99));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

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
