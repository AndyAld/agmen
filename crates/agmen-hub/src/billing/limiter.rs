//! Per-tenant limit on routes that call Paddle.
//!
//! Six calls a minute, burst 6, at most [`MAX_TRACKED`] tenants. A full map
//! forgets tenants whose bucket has refilled, the same way the ingest limiter
//! forgets quiet senders. A tenant that is still busy is never forgotten, so
//! filling the map cannot reset its bucket; if every tracked tenant is busy, a
//! new one is refused until one refills.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use agmen_proto::TenantId;

/// Calls one tenant may sustain per minute.
const PER_MINUTE: u64 = 6;
/// Calls one tenant may make together after a quiet spell.
const BURST: u32 = 6;
/// Seconds between tokens.
const REFILL_SECS: u64 = 60 / PER_MINUTE;
/// The most tenants the limiter tracks.
pub(crate) const MAX_TRACKED: usize = 10_000;

/// A token bucket per tenant.
// shortcut: the counts live in this process, so N hub replicas allow N times
// the limit; keep them in Postgres if the hub ever runs more than one replica.
#[derive(Debug, Default)]
pub(crate) struct BillingLimiter {
    buckets: HashMap<TenantId, Bucket>,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: u32,
    at: Instant,
}

impl Bucket {
    /// Tokens available at `now`, and the instant those tokens were counted from.
    fn refilled(self, now: Instant) -> (u32, Instant) {
        let elapsed = now.saturating_duration_since(self.at).as_secs();
        let steps = u32::try_from(elapsed / REFILL_SECS).unwrap_or(u32::MAX);
        let tokens = self.tokens.saturating_add(steps).min(BURST);
        let at = if tokens == BURST {
            now
        } else {
            self.at
                .checked_add(Duration::from_secs(
                    REFILL_SECS.saturating_mul(u64::from(steps)),
                ))
                .unwrap_or(now)
        };
        (tokens, at)
    }
}

impl BillingLimiter {
    /// Takes one Paddle call for `tenant`.
    ///
    /// `Err(wait)` is how long until the next call is allowed, including when
    /// every tracked tenant is still busy and a new one cannot fit.
    pub(crate) fn take(&mut self, tenant: &TenantId, now: Instant) -> Result<(), Duration> {
        if !self.buckets.contains_key(tenant) && self.buckets.len() >= MAX_TRACKED {
            self.evict_idle(now);
            if self.buckets.len() >= MAX_TRACKED {
                return Err(self.soonest_idle(now));
            }
        }
        let bucket = self.buckets.entry(tenant.clone()).or_insert(Bucket {
            tokens: BURST,
            at: now,
        });
        let (tokens, at) = bucket.refilled(now);
        if tokens >= 1 {
            bucket.tokens = tokens - 1;
            bucket.at = at;
            Ok(())
        } else {
            bucket.tokens = 0;
            bucket.at = at;
            Err(wait_for_one(at, now))
        }
    }

    /// Drops buckets that have refilled. The map never grows past [`MAX_TRACKED`].
    fn evict_idle(&mut self, now: Instant) {
        let idle: Vec<TenantId> = self
            .buckets
            .iter()
            .take(MAX_TRACKED)
            .filter(|(_, bucket)| bucket.refilled(now).0 >= BURST)
            .map(|(tenant, _)| tenant.clone())
            .collect();
        for tenant in idle.iter().take(MAX_TRACKED) {
            self.buckets.remove(tenant);
        }
    }

    /// How long until some tracked bucket refills and can be forgotten.
    fn soonest_idle(&self, now: Instant) -> Duration {
        let mut soonest = Duration::from_secs(REFILL_SECS.saturating_mul(u64::from(BURST)));
        for bucket in self.buckets.values().take(MAX_TRACKED) {
            let (tokens, at) = bucket.refilled(now);
            if tokens >= BURST {
                return Duration::from_secs(1);
            }
            let missing = u64::from(BURST - tokens);
            let into = now
                .saturating_duration_since(at)
                .as_secs()
                .min(REFILL_SECS - 1);
            let wait = (REFILL_SECS - into).saturating_add(REFILL_SECS.saturating_mul(missing - 1));
            let wait = Duration::from_secs(wait.max(1));
            if wait < soonest {
                soonest = wait;
            }
        }
        soonest
    }
}

fn wait_for_one(at: Instant, now: Instant) -> Duration {
    let into = now
        .saturating_duration_since(at)
        .as_secs()
        .min(REFILL_SECS - 1);
    Duration::from_secs(REFILL_SECS - into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(name: &str) -> TenantId {
        name.parse().unwrap()
    }

    #[test]
    fn six_calls_then_ten_seconds() {
        let mut limiter = BillingLimiter::default();
        let acme = tenant("tenant01");
        let start = Instant::now();
        for n in 0..6 {
            assert_eq!(limiter.take(&acme, start), Ok(()), "call {n}");
        }
        assert_eq!(limiter.take(&acme, start), Err(Duration::from_secs(10)));
        assert_eq!(limiter.take(&acme, start + Duration::from_secs(10)), Ok(()));
        assert_eq!(limiter.take(&tenant("tenant02"), start), Ok(()));
    }

    #[test]
    fn limiter_forgets_refilled_tenants() {
        let mut limiter = BillingLimiter::default();
        let start = Instant::now();
        for n in 0..MAX_TRACKED {
            let id = tenant(&format!("t{n:07}"));
            limiter.take(&id, start).unwrap();
        }
        let late = tenant("late0001");
        assert!(limiter.take(&late, start).is_err());
        assert_eq!(limiter.take(&late, start + Duration::from_secs(10)), Ok(()));
        assert_eq!(limiter.buckets.len(), 1);
    }

    #[test]
    fn eviction_leaves_a_busy_tenant_as_it_was() {
        let mut limiter = BillingLimiter::default();
        let start = Instant::now();
        let busy = tenant("busy0001");
        for n in 0..6 {
            assert_eq!(limiter.take(&busy, start), Ok(()), "call {n}");
        }
        for n in 0..MAX_TRACKED - 1 {
            limiter.take(&tenant(&format!("t{n:07}")), start).unwrap();
        }
        assert_eq!(limiter.buckets.len(), MAX_TRACKED);
        // Every bucket is still refilling, so a new tenant waits for the quickest.
        let late = tenant("late0001");
        assert_eq!(limiter.take(&late, start), Err(Duration::from_secs(10)));
        // Ten seconds on, the one-token buckets are full again and are forgotten.
        // The busy tenant is not: it has one token back, not six.
        let later = start + Duration::from_secs(10);
        assert_eq!(limiter.take(&late, later), Ok(()));
        assert_eq!(limiter.buckets.len(), 2);
        assert_eq!(limiter.take(&busy, later), Ok(()));
        assert_eq!(limiter.take(&busy, later), Err(Duration::from_secs(10)));
    }
}
