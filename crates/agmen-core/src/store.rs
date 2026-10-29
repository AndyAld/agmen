//! The backend boundary for coordination reads and atomic writes.

use std::future::Future;

use agmen_proto::Seq;

/// Why a storage operation could not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A record with the same unique key already exists.
    #[error("duplicate record")]
    Duplicate,
    /// Stored data or a backend capacity limit prevents the operation.
    #[error("{0}")]
    Stored(String),
    /// The backend failed, preserving its original error as the source.
    #[error("storage backend failed")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// A backend that supplies readers and transactions.
///
/// The core never holds a reader and a transaction at the same time. A
/// transaction needs to be `Send`, but its state need not be `Sync`.
pub trait Store: Send + Sync + 'static {
    /// Queries outside a write transaction.
    type Reader<'a>: Reads
    where
        Self: 'a;

    /// One atomic write transaction, rolled back when dropped.
    type Tx<'a>: StoreTx
    where
        Self: 'a;

    /// Opens a reader without starting a write transaction.
    fn reader(&self) -> Self::Reader<'_>;

    /// Begins an atomic transaction.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the backend cannot begin a transaction.
    fn begin(&self) -> impl Future<Output = Result<Self::Tx<'_>, StoreError>> + Send;
}

/// Queries shared by a reader and an open transaction.
pub trait Reads: Send {}

/// Reads and writes within one atomic transaction.
///
/// Dropping a transaction without a successful [`commit`](Self::commit)
/// records nothing. Numbering and head locks last until commit or drop.
pub trait StoreTx: Reads {
    /// Locks a key's head without taking a sequence number.
    ///
    /// Creates a missing head at zero, so its next sequence is still one.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the lock or head creation fails.
    fn lock_head(&mut self, key: &str) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Takes the next sequence number, starting at one for each key.
    ///
    /// Holds the key until commit or drop. Dropping the transaction returns
    /// every number it took, so committed numbers have no gaps.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if numbering fails or the sequence is exhausted.
    fn next_seq(&mut self, key: &str) -> impl Future<Output = Result<Seq, StoreError>> + Send;

    /// Makes all writes visible and releases the transaction's locks.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the backend cannot commit the transaction.
    fn commit(self) -> impl Future<Output = Result<(), StoreError>> + Send;
}
