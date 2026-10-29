//! Shared coordination state and settings.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::RateLimiter;

/// Coordination logic backed by a store and a configured claim lease.
#[derive(Debug, Clone)]
pub struct Core<S> {
    store: S,
    claim_lease: Duration,
    pub(crate) limiter: Arc<Mutex<RateLimiter>>,
}

impl<S> Core<S> {
    /// Builds a core over `store`, using `claim_lease` for new claims.
    #[must_use]
    pub fn new(store: S, claim_lease: Duration) -> Self {
        Self {
            store,
            claim_lease,
            limiter: Arc::new(Mutex::new(RateLimiter::default())),
        }
    }

    /// The storage backend used by this core.
    #[must_use]
    pub const fn store(&self) -> &S {
        &self.store
    }

    /// How long a new claim remains live without renewal.
    #[must_use]
    pub const fn claim_lease(&self) -> Duration {
        self.claim_lease
    }
}

/// Why a core operation could not finish.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// The store failed.
    #[error(transparent)]
    Store(#[from] crate::StoreError),
    /// A stored value is not one the core can use.
    #[error("a stored value is invalid: {0}")]
    Stored(String),
}
