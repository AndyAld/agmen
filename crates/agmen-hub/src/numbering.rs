//! Gapless per-stream numbering (protocol 5.2, spike S5).

use sqlx::{Postgres, Transaction};

/// Why a stream could not be numbered.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NumberingError {
    /// The upsert on `stream_heads` failed.
    #[error("numbering stream {stream:?} failed")]
    Database {
        /// The stream being numbered.
        stream: String,
        /// The database error.
        #[source]
        source: sqlx::Error,
    },
}

/// Hands out the next seq on `stream` inside `tx`, starting at 1 for a new
/// stream.
///
/// The upsert locks the stream's `stream_heads` row until `tx` commits or
/// rolls back, so a second transaction on the same stream waits, seqs commit
/// in the order they were handed out, and a rollback returns its seq. Keep
/// the transaction short: it serializes every writer on that stream.
///
/// # Errors
///
/// [`NumberingError::Database`] when the upsert fails, for example because
/// the connection dropped or the migrations have not run.
///
/// # Examples
///
/// ```no_run
/// # async fn example(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
/// let mut tx = pool.begin().await?;
/// let seq = agmen_hub::next_seq(&mut tx, "feed").await?;
/// // Insert the message with `seq` here, in the same transaction.
/// tx.commit().await?;
/// assert!(seq >= 1);
/// # Ok(())
/// # }
/// ```
pub async fn next_seq(
    tx: &mut Transaction<'_, Postgres>,
    stream: &str,
) -> Result<i64, NumberingError> {
    sqlx::query_scalar(
        "INSERT INTO stream_heads (stream, seq) VALUES ($1, 1)
         ON CONFLICT (stream) DO UPDATE SET seq = stream_heads.seq + 1
         RETURNING seq",
    )
    .bind(stream)
    .fetch_one(&mut **tx)
    .await
    .map_err(|source| NumberingError::Database {
        stream: stream.to_owned(),
        source,
    })
}
