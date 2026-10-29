//! Bounded, serializable test storage with rollback on drop.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;

use agmen_proto::{Seq, Timestamp};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::{Reads, Store, StoreError, StoreTx};

/// The maximum rows in each in-memory collection.
pub const MEM_MAX_ROWS: usize = 100_000;

/// A forgotten transaction must fail a waiting test instead of hanging it.
const MEM_TX_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default)]
struct MemState {
    heads: BTreeMap<String, Seq>,
}

/// In-memory storage for backend-independent coordination tests.
///
/// Clones share committed state, the clock and a single pending fault.
/// Transactions serialize on one mutex and edit a working copy, so dropping
/// a transaction discards all its writes.
#[derive(Debug, Clone)]
pub struct MemStore {
    state: Arc<Mutex<MemState>>,
    clock: Arc<StdMutex<Timestamp>>,
    failure: Arc<StdMutex<Option<&'static str>>>,
}

impl Default for MemStore {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(MemState::default())),
            clock: Arc::new(StdMutex::new(Timestamp::now())),
            failure: Arc::new(StdMutex::new(None)),
        }
    }
}

impl MemStore {
    /// Sets the test clock, initially the time this store was created.
    pub fn set_now(&self, at: Timestamp) {
        *self.clock.lock().unwrap_or_else(PoisonError::into_inner) = at;
    }

    /// Fails the next call to method `op` with [`StoreError::Backend`].
    ///
    /// Replaces any pending fault. Other methods do not consume it, and a
    /// rollback does not rearm a consumed fault.
    pub fn fail_next(&self, op: &'static str) {
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(op);
    }
}

impl Reads for &MemStore {}

impl Store for MemStore {
    type Reader<'a> = &'a Self;
    type Tx<'a> = MemTx;

    fn reader(&self) -> Self::Reader<'_> {
        self
    }

    async fn begin(&self) -> Result<Self::Tx<'_>, StoreError> {
        fail(&self.failure, "begin")?;
        let guard = tokio::time::timeout(MEM_TX_WAIT, Arc::clone(&self.state).lock_owned())
            .await
            .map_err(|source| StoreError::Backend(Box::new(source)))?;
        let working = guard.clone();
        Ok(MemTx {
            guard,
            working,
            failure: Arc::clone(&self.failure),
        })
    }
}

/// One in-memory transaction, rolled back unless committed.
#[derive(Debug)]
pub struct MemTx {
    guard: OwnedMutexGuard<MemState>,
    working: MemState,
    failure: Arc<StdMutex<Option<&'static str>>>,
}

impl MemTx {
    fn head(&mut self, key: &str) -> Result<&mut Seq, StoreError> {
        if self.working.heads.len() >= MEM_MAX_ROWS && !self.working.heads.contains_key(key) {
            return Err(StoreError::Stored("mem store full".into()));
        }
        Ok(self.working.heads.entry(key.to_owned()).or_default())
    }
}

impl Reads for MemTx {}

impl StoreTx for MemTx {
    async fn lock_head(&mut self, key: &str) -> Result<(), StoreError> {
        fail(&self.failure, "lock_head")?;
        self.head(key)?;
        Ok(())
    }

    async fn next_seq(&mut self, key: &str) -> Result<Seq, StoreError> {
        fail(&self.failure, "next_seq")?;
        let head = self.head(key)?;
        head.0 = head
            .0
            .checked_add(1)
            .ok_or_else(|| StoreError::Stored("sequence exhausted".into()))?;
        Ok(*head)
    }

    async fn commit(mut self) -> Result<(), StoreError> {
        fail(&self.failure, "commit")?;
        *self.guard = self.working;
        Ok(())
    }
}

fn fail(failure: &StdMutex<Option<&'static str>>, op: &'static str) -> Result<(), StoreError> {
    let mut failure = failure.lock().unwrap_or_else(PoisonError::into_inner);
    if *failure == Some(op) {
        *failure = None;
        return Err(StoreError::Backend(Box::new(std::io::Error::other(
            format!("injected {op} failure"),
        ))));
    }
    Ok(())
}
