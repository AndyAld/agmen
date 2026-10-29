//! Bounded per-sender message rate limits.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use agmen_proto::IdentityId;

/// Publishes per second each sender may sustain (protocol section 8).
pub const RATE_PER_SEC: f64 = 10.0;
/// Publishes a sender may make at once after a quiet spell.
pub const BURST: f64 = 30.0;
/// The most senders the rate limiter tracks. Past it, senders whose bucket
/// has refilled are forgotten; if every tracked sender is still busy, new
/// senders get `hub_busy` until one calms down.
pub const MAX_TRACKED: usize = 10_000;
/// A token bucket per sender: [`RATE_PER_SEC`] sustained, [`BURST`] at
/// once.
#[derive(Debug, Default)]
pub struct RateLimiter {
    buckets: HashMap<IdentityId, Bucket>,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn refilled(self, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        elapsed.mul_add(RATE_PER_SEC, self.tokens).min(BURST)
    }
}

impl RateLimiter {
    /// Takes one publish from `id`'s bucket. `Err(Some(wait))` says how
    /// long until the next one is allowed; `Err(None)` means the limiter is
    /// full of busy senders.
    ///
    /// # Errors
    ///
    /// Returns the time until the next token, or `None` when all tracked
    /// senders are busy and a new sender cannot fit.
    pub fn take(&mut self, id: &IdentityId, now: Instant) -> Result<(), Option<Duration>> {
        if !self.buckets.contains_key(id) && self.buckets.len() >= MAX_TRACKED {
            // A full bucket is the same as no bucket, so forgetting one
            // changes no answer.
            self.buckets
                .retain(|_, bucket| bucket.refilled(now) < BURST);
            if self.buckets.len() >= MAX_TRACKED {
                return Err(None);
            }
        }
        let bucket = self.buckets.entry(id.clone()).or_insert(Bucket {
            tokens: BURST,
            at: now,
        });
        bucket.tokens = bucket.refilled(now);
        bucket.at = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Some(Duration::from_secs_f64(
                (1.0 - bucket.tokens) / RATE_PER_SEC,
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agmen_proto::IdentityId;
    use std::time::{Duration, Instant};

    fn id(name: &str) -> IdentityId {
        format!("{name:0<8}").parse().unwrap()
    }

    #[test]
    fn rate_limit_allows_a_burst_of_30_then_10_per_second() {
        let mut limiter = RateLimiter::default();
        let sol = id("sol");
        let start = Instant::now();
        for n in 0..30 {
            assert_eq!(limiter.take(&sol, start), Ok(()), "publish {n}");
        }
        let wait = limiter.take(&sol, start).unwrap_err().unwrap();
        assert!(
            wait > Duration::from_millis(90) && wait <= Duration::from_millis(100),
            "{wait:?}"
        );
        let later = start + Duration::from_millis(100);
        assert_eq!(limiter.take(&sol, later), Ok(()));
        assert!(limiter.take(&sol, later).is_err());
        // Another sender has a bucket of its own.
        assert_eq!(limiter.take(&id("w3f"), start), Ok(()));
    }

    #[test]
    fn rate_limiter_stays_bounded() {
        let mut limiter = RateLimiter::default();
        let start = Instant::now();
        for n in 0..MAX_TRACKED {
            let sender: IdentityId = format!("s{n:0>9}").parse().unwrap();
            limiter.take(&sender, start).unwrap();
        }
        // Every bucket is still refilling, so a new sender has no room.
        assert_eq!(limiter.take(&id("late"), start), Err(None));
        // Once the buckets refill they are forgotten and the new sender fits.
        let refilled = start + Duration::from_secs(1);
        assert_eq!(limiter.take(&id("late"), refilled), Ok(()));
        assert_eq!(limiter.buckets.len(), 1);
    }
}
