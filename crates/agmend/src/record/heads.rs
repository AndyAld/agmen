//! Gapless numbering held by the connection's immediate transaction.

use agmen_core::StoreError;
use agmen_proto::Seq;
use rusqlite::Connection;

use super::backend;

pub(super) fn lock_head(conn: &Connection, _now: i64, key: String) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO stream_heads(stream, seq) VALUES (?1, 0) ON CONFLICT(stream) DO NOTHING",
        [key],
    )
    .map(drop)
    .map_err(backend)
}

pub(super) fn next_seq(conn: &Connection, _now: i64, key: String) -> Result<Seq, StoreError> {
    let seq: i64 = conn
        .query_row(
            "INSERT INTO stream_heads(stream, seq) VALUES (?1, 1)
             ON CONFLICT(stream) DO UPDATE SET seq=stream_heads.seq+1 RETURNING seq",
            [key],
            |row| row.get(0),
        )
        .map_err(backend)?;
    let seq = u64::try_from(seq).map_err(|err| StoreError::Stored(err.to_string()))?;
    Ok(Seq(seq))
}
