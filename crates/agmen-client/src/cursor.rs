//! Cursors (protocol 5.3 and ruling R45): per stream, the last seq
//! `agmen watch` delivered, which drops duplicates and finds gaps, and the
//! last seq the inbox listed. They live in the identity's SQLite file next
//! to the outbox.

use std::path::Path;

use agmen_proto::Stream;
use rusqlite::{OptionalExtension as _, TransactionBehavior, params};

use crate::db::{Db, from_sql_int, to_sql_seq};
use crate::error::ClientError;

/// What [`Cursors::advance`] made of a seq.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Advance {
    /// The next seq: the cursor moved to it, so deliver the message.
    Next,
    /// At or before the cursor: already delivered, drop it.
    Duplicate,
    /// Seqs `from` to `to` (inclusive) are missing before it. The cursor
    /// did not move; replay the gap, then deliver.
    Gap {
        /// The first missing seq.
        from: u64,
        /// The last missing seq.
        to: u64,
    },
}

/// The cursors in the identity's SQLite file. Cheap to clone; clones share
/// one connection.
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use agmen_client::{Advance, Cursors};
/// use agmen_proto::Stream;
///
/// let dir = tempfile::tempdir()?;
/// let cursors = Cursors::open(dir.path().join("identity.db"))?;
/// assert_eq!(cursors.advance(&Stream::Feed, 1)?, Advance::Next);
/// assert_eq!(cursors.advance(&Stream::Feed, 1)?, Advance::Duplicate);
/// assert_eq!(cursors.advance(&Stream::Feed, 4)?, Advance::Gap { from: 2, to: 3 });
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
pub struct Cursors {
    db: Db,
}

impl Cursors {
    /// Opens the cursors in the SQLite file at `path`, creating the file and
    /// its tables if needed.
    ///
    /// # Errors
    ///
    /// [`ClientError`] when the file cannot be opened or switched to WAL.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        Ok(Self {
            db: Db::open(path.as_ref())?,
        })
    }

    /// The last seq delivered from `stream`; 0 before the first.
    ///
    /// # Errors
    ///
    /// [`ClientError`] when the read fails.
    pub fn last(&self, stream: &Stream) -> Result<u64, ClientError> {
        let conn = self.db.lock();
        column(&conn, "delivered", stream)
    }

    /// Moves the delivery cursor of `stream` to `seq` when `seq` is the
    /// next one, and says what `seq` is.
    ///
    /// # Errors
    ///
    /// [`ClientError`] when the read or write fails, or `seq` is too large
    /// to store.
    pub fn advance(&self, stream: &Stream, seq: u64) -> Result<Advance, ClientError> {
        let mut conn = self.db.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let last = column(&tx, "delivered", stream)?;
        let advance = if seq <= last {
            Advance::Duplicate
        } else if seq == last + 1 {
            tx.execute(
                "INSERT INTO cursors (stream, delivered) VALUES (?1, ?2)
                 ON CONFLICT (stream) DO UPDATE SET delivered = excluded.delivered",
                params![stream.to_string(), to_sql_seq(seq)?],
            )?;
            Advance::Next
        } else {
            Advance::Gap {
                from: last + 1,
                to: seq - 1,
            }
        };
        tx.commit()?;
        Ok(advance)
    }

    /// Raises the delivery cursor of `stream` to `seq`; it never moves back.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let dir = tempfile::tempdir()?;
    /// let cursors = agmen_client::Cursors::open(dir.path().join("identity.db"))?;
    /// cursors.skip_to(&agmen_proto::Stream::Feed, 6)?;
    /// assert_eq!(cursors.last(&agmen_proto::Stream::Feed)?, 6);
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// [`ClientError`] when the write fails or `seq` is too large to store.
    pub fn skip_to(&self, stream: &Stream, seq: u64) -> Result<(), ClientError> {
        self.db.lock().execute(
            "INSERT INTO cursors (stream, delivered) VALUES (?1, ?2)
             ON CONFLICT (stream) DO UPDATE SET delivered = max(delivered, excluded.delivered)",
            params![stream.to_string(), to_sql_seq(seq)?],
        )?;
        Ok(())
    }

    /// The last seq of `stream` the inbox listed; 0 before the first.
    ///
    /// # Errors
    ///
    /// [`ClientError`] when the read fails.
    pub fn read_cursor(&self, stream: &Stream) -> Result<u64, ClientError> {
        let conn = self.db.lock();
        column(&conn, "read", stream)
    }

    /// Moves the read cursor of `stream` forward to `seq`; it never moves
    /// back.
    ///
    /// # Errors
    ///
    /// [`ClientError`] when the write fails or `seq` is too large to store.
    pub fn mark_read(&self, stream: &Stream, seq: u64) -> Result<(), ClientError> {
        self.db.lock().execute(
            "INSERT INTO cursors (stream, read) VALUES (?1, ?2)
             ON CONFLICT (stream) DO UPDATE SET read = max(read, excluded.read)",
            params![stream.to_string(), to_sql_seq(seq)?],
        )?;
        Ok(())
    }
}

/// Cursor `name` (`delivered` or `read`) of `stream`, 0 without a row.
fn column(
    conn: &rusqlite::Connection,
    name: &'static str,
    stream: &Stream,
) -> Result<u64, ClientError> {
    // `name` is one of two fixed column names, never input.
    let sql = format!("SELECT {name} FROM cursors WHERE stream = ?1");
    let value: Option<i64> = conn
        .query_row(&sql, params![stream.to_string()], |row| row.get(0))
        .optional()?;
    value.map_or(Ok(0), from_sql_int)
}
