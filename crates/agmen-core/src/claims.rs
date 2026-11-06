//! Claims, renewals and releases in the caller's message transaction.

use std::time::Duration;

use crate::{Core, HubPost, Lapsed, PostError, ReleaseBy, Store, StoreError, StoreTx, post_in};
use agmen_proto::{
    Body, ClientEnvelope, DenyReason, IdentityId, IdentityKind, ItemKey, LaneName, NackReason,
    Payload, ProjectId, ProtoError, Stream, Timestamp, Topics, Verb, validate,
};

const LEASE_EXPIRED: &str = "lease_expired";

/// A ledger verb ingest has just recorded, with what the ledger needs of
/// its sender.
#[derive(Debug, Clone)]
pub struct LedgerMessage {
    /// The envelope as the client sent it.
    pub env: ClientEnvelope,
    /// The stamped sender.
    pub from: IdentityId,
    /// The sender's handle.
    pub from_handle: String,
    /// The sender's kind.
    pub from_kind: IdentityKind,
    /// The sender's owning human; `None` for a human.
    pub from_owner: Option<IdentityId>,
    /// The hub's clock when the message arrived.
    pub hub_at: Timestamp,
}

impl LedgerMessage {
    /// Whether the ledger acts on `verb` once it is recorded.
    pub const fn handles(verb: Verb) -> bool {
        matches!(
            verb,
            Verb::Claim | Verb::Heartbeat | Verb::Release | Verb::Assign | Verb::Found | Verb::Done
        )
    }
}

/// Why the ledger could not act on a message.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LedgerError {
    /// A ledger storage statement failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Posting the hub's reply failed.
    #[error("posting the hub's answer failed")]
    Post(#[from] PostError),
    /// Building the reply failed.
    #[error("building the hub's answer failed")]
    Proto(#[from] ProtoError),
    /// A stored value is invalid.
    #[error("a stored value is invalid: {0}")]
    Stored(String),
}

fn stored(what: impl std::fmt::Display) -> LedgerError {
    LedgerError::Stored(what.to_string())
}

/// Acts on a ledger verb ingest has just written in `tx`, posting the
/// hub's answer in `tx` too, so the message, its effects and the answer
/// commit together or not at all.
///
/// # Errors
/// Returns [`LedgerError`] when a statement or reply fails.
pub async fn apply_ledger<T: StoreTx>(
    tx: &mut T,
    topics: &Topics,
    msg: &LedgerMessage,
    lease: Duration,
) -> Result<(), LedgerError> {
    let project = topics.project();
    match &msg.env.payload {
        Payload::Claim { item } => claim(tx, topics, msg, item, lease).await,
        Payload::Heartbeat { items } => renew(tx, project, &msg.from, items, lease).await,
        Payload::Release { item, .. } => release(tx, project, msg, item).await,
        Payload::Assign { item, to } => crate::items::assign(tx, project, item, to).await,
        Payload::Found {
            title,
            lane,
            severity,
        } => crate::items::found(tx, topics, msg, title, lane.as_ref(), *severity).await,
        Payload::Done { item, changes, .. } => {
            crate::lanes::check_done(tx, topics, msg, item, changes).await
        }
        _ => Ok(()),
    }
}

/// The RELEASE `lease_expired` for `item`, mentioning the holder.
///
/// # Errors
/// Returns [`ProtoError`] if the release body cannot be built.
pub(crate) fn lease_expired(
    item: &ItemKey,
    lane: Option<LaneName>,
    lapsed: &Lapsed,
) -> Result<HubPost, ProtoError> {
    tracing::info!(%item, holder = %lapsed.holder, "lease expired");
    Ok(HubPost {
        payload: Payload::Release {
            item: item.clone(),
            reason: Some(LEASE_EXPIRED.to_owned()),
        },
        body: Body::try_from(format!(
            "{item} released: {}'s lease ran out",
            lapsed.handle
        ))?,
        reply_to: None,
        items: vec![item.clone()],
        lane,
        mentioned: vec![lapsed.holder.clone()],
        streams: vec![Stream::Feed],
    })
}

/// Answers a CLAIM with GRANT or DENY in the claimant's inbox and the feed
/// (protocol 4). A CLAIM past its `valid_until` by more than the skew
/// allowance, judged by the hub's clock, gets DENY `expired`.
async fn claim<T: StoreTx>(
    tx: &mut T,
    topics: &Topics,
    msg: &LedgerMessage,
    item: &ItemKey,
    lease: Duration,
) -> Result<(), LedgerError> {
    // Ingest admitted the CLAIM, so validate passes or finds it late.
    let late = validate(&msg.env, msg.hub_at) == Err(NackReason::Expired);
    let decided = if late {
        Decided::denied(DenyReason::Expired, None)
    } else {
        decide(tx, topics.project(), msg, item, lease).await?
    };
    if let Some(lapsed) = &decided.lapsed {
        post_in(
            tx,
            topics,
            lease_expired(item, decided.lane.clone(), lapsed)?,
        )
        .await?;
    }
    let claimant = &msg.from_handle;
    let (payload, body) = match decided.outcome {
        Outcome::Granted { lease_until } => (
            Payload::Grant {
                item: item.clone(),
                lease_until,
            },
            format!("{item} granted to {claimant} until {lease_until}"),
        ),
        Outcome::Denied { reason, holder } => {
            let why = match (reason, holder) {
                (DenyReason::HeldBy, Some(holder)) => format!("held by {holder}"),
                (DenyReason::HeldBy, None) => "held by another identity".to_owned(),
                (DenyReason::Expired, _) => "the CLAIM arrived after its valid_until".to_owned(),
                (DenyReason::NotInLane, _) => "its lane belongs to another identity".to_owned(),
                (DenyReason::Closed, _) => format!("no open item {item}"),
            };
            (
                Payload::Deny {
                    item: item.clone(),
                    reason,
                },
                format!("{item} denied to {claimant}: {why}"),
            )
        }
    };
    post_in(
        tx,
        topics,
        HubPost {
            payload,
            body: Body::try_from(body)?,
            reply_to: Some(msg.env.id.clone()),
            items: vec![item.clone()],
            lane: decided.lane,
            mentioned: Vec::new(),
            streams: vec![Stream::Feed, Stream::Inbox(msg.from.clone())],
        },
    )
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Granted {
        lease_until: Timestamp,
    },
    Denied {
        reason: DenyReason,
        /// The holder's handle, for `held_by`.
        holder: Option<String>,
    },
}

/// What a CLAIM came to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Decided {
    outcome: Outcome,
    /// The item's lane.
    lane: Option<LaneName>,
    /// The claim released because its lease had run out.
    lapsed: Option<Lapsed>,
}

impl Decided {
    const fn denied(reason: DenyReason, lane: Option<LaneName>) -> Self {
        Self {
            outcome: Outcome::Denied {
                reason,
                holder: None,
            },
            lane,
            lapsed: None,
        }
    }
}

/// Decides a CLAIM of `item` in `tx`, which then holds the item's row lock
/// until it commits, so CLAIMs of one item are decided one at a time:
///
/// - no item, or one that is done: `closed`;
/// - an item in a lane with an owner, from anyone but that owner, a
///   coordinator, or the identity the item was assigned to: `not_in_lane`;
/// - an active claim by another identity: `held_by`. A claim whose lease
///   ran out is released first;
/// - an active claim by the claimant: granted again with a fresh lease;
/// - otherwise granted.
async fn decide<T: StoreTx>(
    tx: &mut T,
    project: &ProjectId,
    msg: &LedgerMessage,
    item: &ItemKey,
    lease: Duration,
) -> Result<Decided, LedgerError> {
    let row = tx.lock_item(project, item).await?;
    let Some((state, lane, assigned_to, owner)) = row else {
        return Ok(Decided::denied(DenyReason::Closed, None));
    };
    let lane: Option<LaneName> = lane
        .as_deref()
        .map(str::parse)
        .transpose()
        .map_err(stored)?;
    if state != "open" {
        return Ok(Decided::denied(DenyReason::Closed, lane));
    }
    let from = msg.from.as_str();
    if let Some(owner) = owner
        && owner != from
        && msg.from_kind != IdentityKind::Coordinator
        && assigned_to.as_deref() != Some(from)
    {
        return Ok(Decided::denied(DenyReason::NotInLane, lane));
    }
    let lapsed = tx.release_lapsed(project, item).await?;
    let outcome = match take(tx, project, item, &msg.from, lease).await? {
        Taken::Granted(lease_until) => Outcome::Granted { lease_until },
        Taken::Held(handle) => Outcome::Denied {
            reason: DenyReason::HeldBy,
            holder: Some(handle),
        },
    };
    Ok(Decided {
        outcome,
        lane,
        lapsed,
    })
}

/// What taking a claim came to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Taken {
    Granted(Timestamp),
    Held(String),
}

/// The item row is already locked before reading or changing its claim.
async fn take<T: StoreTx>(
    tx: &mut T,
    project: &ProjectId,
    item: &ItemKey,
    from: &IdentityId,
    lease: Duration,
) -> Result<Taken, LedgerError> {
    match tx.active_claim(project, item).await? {
        Some((holder, _)) if holder == *from => {
            Ok(Taken::Granted(tx.extend_claim(project, item, lease).await?))
        }
        Some((_, handle)) => Ok(Taken::Held(handle)),
        None => Ok(Taken::Granted(
            tx.insert_claim(project, item, from, lease).await?,
        )),
    }
}

/// HEARTBEAT renews only leases that have not ended.
async fn renew<T: StoreTx>(
    tx: &mut T,
    project: &ProjectId,
    from: &IdentityId,
    items: &[ItemKey],
    lease: Duration,
) -> Result<(), LedgerError> {
    let renewed = tx.renew_claims(project, from, items, lease).await?;
    tracing::debug!(%from, renewed, "heartbeat");
    Ok(())
}

/// RELEASE by a holder or a coordinator; otherwise nothing changes.
async fn release<T: StoreTx>(
    tx: &mut T,
    project: &ProjectId,
    msg: &LedgerMessage,
    item: &ItemKey,
) -> Result<(), LedgerError> {
    let by = if msg.from_kind == IdentityKind::Coordinator {
        ReleaseBy::Coordinator
    } else {
        ReleaseBy::Holder
    };
    let released = tx.release_claim(project, item, &msg.from, by).await?;
    if released == 0 {
        tracing::debug!(%item, from = %msg.from, "RELEASE released nothing");
    }
    Ok(())
}

/// Expired claims one sweep releases at most.
const SWEEP_BATCH: u32 = 100;
/// Failed sweeps in a row before the lease sweeper gives up.
const MAX_FAILURES: u32 = 60;
/// The pause after a failed sweep.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Why [`Core::run_lease_sweeper`] stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
#[error("the lease sweeper failed {failures} times in a row")]
pub struct SweepGaveUp {
    /// Sweeps that failed in a row.
    pub failures: u32,
    /// The last failure.
    #[source]
    pub last: LedgerError,
}

impl<S: Store> Core<S> {
    /// Releases claims whose lease ran out and posts RELEASE `lease_expired`
    /// to the feed for each open item. Checks every quarter lease, and at
    /// least every 30 s. Returns only when 60 sweeps in a row fail.
    ///
    /// Each committed RELEASE wakes the publisher and the project's readers.
    ///
    /// A full batch does not sleep. A shorter batch sleeps. A failure sleeps
    /// one second, except the 60th, which returns.
    ///
    /// The success path runs until the task is aborted. The failure path
    /// stops after 60 failures in a row.
    ///
    /// # Errors
    ///
    /// [`SweepGaveUp`] with the last failure. It does not return [`Ok`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example<S: agmen_core::Store + 'static>(core: agmen_core::Core<S>) {
    /// tokio::spawn(async move { core.run_lease_sweeper().await });
    /// # }
    /// ```
    pub async fn run_lease_sweeper(&self) -> Result<(), SweepGaveUp> {
        self.sweep_until_gave_up(RETRY_DELAY).await
    }

    /// [`run_lease_sweeper`](Self::run_lease_sweeper) with the pause after a
    /// failed sweep as an argument, so a test need not wait 59 real seconds.
    async fn sweep_until_gave_up(&self, retry_delay: Duration) -> Result<(), SweepGaveUp> {
        let every =
            (self.claim_lease() / 4).clamp(Duration::from_millis(100), Duration::from_secs(30));
        let mut failures = 0_u32;
        loop {
            match self.expire_leases().await {
                Ok(released) => {
                    failures = 0;
                    if released < batch_len() {
                        tokio::time::sleep(every).await;
                    }
                }
                Err(err) => {
                    failures += 1;
                    if failures >= MAX_FAILURES {
                        return Err(SweepGaveUp {
                            failures,
                            last: err,
                        });
                    }
                    tracing::warn!(
                        error = %sweep_error(&err),
                        failures,
                        "lease sweep failed; retrying"
                    );
                    tokio::time::sleep(retry_delay).await;
                }
            }
        }
    }

    /// One sweep: releases up to [`SWEEP_BATCH`] expired claims and posts a
    /// RELEASE for each whose item is still open, waking its project after
    /// each post. Returns how many it released. DONE ends its
    /// own claim, so a claim on a done item reaches here only if it predates
    /// that; it is released without a RELEASE.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError`] when the release or a later post fails.
    ///
    /// shortcut: the release commits before its RELEASE is posted, so a
    /// crash in between loses that RELEASE (the claim is still released);
    /// post in the release's transaction if that matters.
    pub(crate) async fn expire_leases(&self) -> Result<usize, LedgerError> {
        let mut tx = self.store().begin().await?;
        let rows = tx.expire_claims(SWEEP_BATCH).await?;
        tx.commit().await?;
        // Ids are parsed after commit, so a corrupt stored id still leaves
        // the batch released. Done items are skipped before parsing.
        for row in rows.iter().take(batch_len()) {
            if !row.open {
                continue;
            }
            let topics = Topics::new(
                row.tenant.parse().map_err(stored)?,
                row.project.parse().map_err(stored)?,
            );
            let lapsed = Lapsed {
                holder: row.holder.parse().map_err(stored)?,
                handle: row.handle.clone(),
            };
            let lane = row
                .lane
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(stored)?;
            let item = row.item.parse().map_err(stored)?;
            self.post_as_hub(&topics, lease_expired(&item, lane, &lapsed)?)
                .await?;
        }
        Ok(rows.len())
    }
}

fn batch_len() -> usize {
    usize::try_from(SWEEP_BATCH).unwrap_or(usize::MAX)
}

fn sweep_error(err: &LedgerError) -> String {
    match err {
        LedgerError::Store(StoreError::Backend(source)) => {
            format!("database error: {}", error_chain(source.as_ref()))
        }
        other => error_chain(other),
    }
}

/// `err` and up to 16 of its sources, joined with ": ".
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut parts = Vec::with_capacity(17);
    parts.push(err.to_string());
    let mut current = err.source();
    while parts.len() < 17 {
        let Some(inner) = current else {
            break;
        };
        parts.push(inner.to_string());
        current = std::error::Error::source(inner);
    }
    parts.join(": ")
}

#[cfg(test)]
#[path = "claims_tests.rs"]
pub(crate) mod tests;
