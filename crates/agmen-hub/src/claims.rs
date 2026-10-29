//! Lease expiry stays in the hub until the presence and expiry extraction.

use std::time::Duration;

use agmen_core::{Lapsed, lease_expired};
use agmen_proto::{ProtoError, Topics};

use crate::http::{Hub, error_chain};
use crate::ingest::PostError;

/// Expired claims one sweep releases at most.
const SWEEP_BATCH: i64 = 100;
/// Failed sweeps in a row before the lease sweeper gives up.
const MAX_FAILURES: u32 = 60;
/// The pause after a failed sweep.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Why the ledger could not act on a message.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LedgerError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("posting the hub's answer failed")]
    Post(#[from] PostError),
    #[error("building the hub's answer failed")]
    Proto(#[from] ProtoError),
    #[error("a stored value is invalid: {0}")]
    Stored(String),
}

fn stored(what: impl std::fmt::Display) -> LedgerError {
    LedgerError::Stored(what.to_string())
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
        let every =
            (self.claim_lease() / 4).clamp(Duration::from_millis(100), Duration::from_secs(30));
        let mut failures = 0_u32;
        loop {
            match self.expire_leases().await {
                Ok(released) => {
                    failures = 0;
                    if released < usize::try_from(SWEEP_BATCH).unwrap_or(usize::MAX) {
                        tokio::time::sleep(every).await;
                    }
                }
                Err(err) => {
                    failures += 1;
                    if failures >= MAX_FAILURES {
                        return Err(LeaseSweepError::GaveUp {
                            failures,
                            source: Box::new(err),
                        });
                    }
                    tracing::warn!(error = %error_chain(&err), failures, "lease sweep failed; retrying");
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
    }

    /// One sweep: releases up to [`SWEEP_BATCH`] expired claims and posts a
    /// RELEASE for each whose item is still open. Returns how many it
    /// released. DONE ends its own claim, so a claim on a done item reaches
    /// here only if it predates that; it is released without a RELEASE.
    ///
    /// shortcut: the release commits before its RELEASE is posted, so a
    /// crash in between loses that RELEASE (the claim is still released);
    /// post in the release's transaction if that matters.
    async fn expire_leases(&self) -> Result<usize, LedgerError> {
        let rows: Vec<ExpiredClaim> = sqlx::query_as(
            "UPDATE claims c SET released_at = now()
             FROM items i, identities h, projects p
             WHERE c.id IN (
                     SELECT id FROM claims
                     WHERE released_at IS NULL AND lease_until <= now()
                     ORDER BY lease_until LIMIT $1
                     FOR UPDATE SKIP LOCKED)
               AND i.project_id = c.project_id AND i.key = c.item_key
               AND h.id = c.identity_id AND p.id = c.project_id
             RETURNING p.tenant_id, c.project_id, c.item_key, i.lane, c.identity_id, h.handle,
                       i.state = 'open'",
        )
        .bind(SWEEP_BATCH)
        .fetch_all(self.pool())
        .await?;
        for (tenant, project, item, lane, holder, handle, open) in &rows {
            if !open {
                continue;
            }
            let topics = Topics::new(
                tenant.parse().map_err(stored)?,
                project.parse().map_err(stored)?,
            );
            let lapsed = Lapsed {
                holder: holder.parse().map_err(stored)?,
                handle: handle.clone(),
            };
            let lane = lane
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(stored)?;
            let item = item.parse().map_err(stored)?;
            self.post_as_hub(&topics, lease_expired(&item, lane, &lapsed)?)
                .await?;
        }
        Ok(rows.len())
    }
}

/// A claim the lease sweep released: tenant, project, item, lane, holder,
/// the holder's handle, and whether the item is still open.
type ExpiredClaim = (String, String, String, Option<String>, String, String, bool);
