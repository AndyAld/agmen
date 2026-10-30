//! Lease expiry. The sweep runs in the core. This module keeps the hub's
//! error type and the task [`Hub::start_messaging`] spawns.

use agmen_core::StoreError;

use crate::http::Hub;
use crate::ingest::PostError;

/// Why the ledger could not act on a message.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LedgerError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("posting the hub's answer failed")]
    Post(#[from] PostError),
    #[error("building the hub's answer failed")]
    Proto(#[from] agmen_proto::ProtoError),
    #[error("a stored value is invalid: {0}")]
    Stored(String),
}

/// Why [`Hub::run_lease_sweeper`] stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LeaseSweepError {
    /// Sweeps failed this many times in a row.
    #[error("the lease sweeper failed {failures} times in a row")]
    GaveUp {
        /// Sweeps that failed in a row.
        failures: u32,
        /// The last failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl Hub {
    /// Releases claims whose lease ran out and posts RELEASE
    /// `lease_expired` to the feed for each, checking every quarter lease
    /// (at least every 30 s). [`Hub::start_messaging`] runs it; it returns
    /// only when 60 sweeps in a row fail.
    ///
    /// # Errors
    ///
    /// [`LeaseSweepError::GaveUp`] with the last failure.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(hub: agmen_hub::Hub) -> Result<(), agmen_hub::LeaseSweepError> {
    /// tokio::spawn(async move { hub.run_lease_sweeper().await });
    /// # Ok(()) }
    /// ```
    pub async fn run_lease_sweeper(&self) -> Result<(), LeaseSweepError> {
        // Each RELEASE wakes the outbox sweeper and the project's events
        // streams, as every post through `Hub::post_as_hub` does.
        self.core()
            .run_lease_sweeper(|project| self.ingest_state().stored(project))
            .await
            .map_err(|gave_up| LeaseSweepError::GaveUp {
                failures: gave_up.failures,
                source: Box::new(sweep_source(gave_up.last)),
            })
    }
}

fn sweep_source(err: agmen_core::LedgerError) -> LedgerError {
    match err {
        agmen_core::LedgerError::Store(source) => match source {
            StoreError::Backend(source) => LedgerError::Database(decode_sqlx(source)),
            StoreError::Stored(text) => LedgerError::Stored(text),
            other => LedgerError::Database(sqlx::Error::Decode(Box::new(other))),
        },
        agmen_core::LedgerError::Post(err) => LedgerError::Post(err),
        agmen_core::LedgerError::Proto(err) => LedgerError::Proto(err),
        agmen_core::LedgerError::Stored(text) => LedgerError::Stored(text),
        other => LedgerError::Database(sqlx::Error::Decode(Box::new(other))),
    }
}

fn decode_sqlx(source: Box<dyn std::error::Error + Send + Sync>) -> sqlx::Error {
    match source.downcast::<sqlx::Error>() {
        Ok(err) => *err,
        Err(source) => sqlx::Error::Decode(source),
    }
}
