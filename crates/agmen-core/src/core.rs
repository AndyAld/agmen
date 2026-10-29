//! Shared coordination state and settings.

use std::time::Duration;

/// Coordination logic backed by a store and a configured claim lease.
#[derive(Debug, Clone)]
pub struct Core<S> {
    store: S,
    claim_lease: Duration,
}

impl<S> Core<S> {
    /// Builds a core over `store`, using `claim_lease` for new claims.
    #[must_use]
    pub const fn new(store: S, claim_lease: Duration) -> Self {
        Self { store, claim_lease }
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
