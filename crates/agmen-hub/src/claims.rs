//! Claims (design 6.5): CLAIM answered with GRANT or DENY, HEARTBEAT
//! renewing leases, RELEASE, and the lease sweeper that releases claims
//! whose lease ran out. Also the hand-off from ingest: each recorded
//! ledger verb goes to its handler here, in `items` or in `lanes`.
//!
//! Exactly one claim per item is active: a CLAIM locks the item's row, and
//! a partial unique index backs that up.

use std::time::Duration;

use agmen_proto::{
    Body, ClientEnvelope, DenyReason, IdentityId, IdentityKind, ItemKey, LaneName, NackReason,
    Payload, ProtoError, Stream, Timestamp, Topics, Verb, validate,
};
use sqlx::{PgConnection, PgExecutor, Postgres, Transaction};

use crate::http::{Hub, error_chain};
use crate::ingest::{HubPost, PostError, post_in};

/// Expired claims one sweep releases at most.
const SWEEP_BATCH: i64 = 100;
/// Failed sweeps in a row before the lease sweeper gives up.
const MAX_FAILURES: u32 = 60;
/// The pause after a failed sweep.
const RETRY_DELAY: Duration = Duration::from_secs(1);
/// What a lease sweep's RELEASE says.
const LEASE_EXPIRED: &str = "lease_expired";

/// A ledger verb ingest has just recorded, with what the ledger needs of
/// its sender.
#[derive(Debug, Clone)]
pub(crate) struct LedgerMessage {
    /// The envelope as the client sent it.
    pub(crate) env: ClientEnvelope,
    pub(crate) from: IdentityId,
    pub(crate) from_handle: String,
    pub(crate) from_kind: IdentityKind,
    /// The sender's owning human; `None` for a human.
    pub(crate) from_owner: Option<IdentityId>,
    /// The hub's clock when the message arrived.
    pub(crate) hub_at: Timestamp,
}

impl LedgerMessage {
    /// Whether the ledger acts on `verb` once it is recorded.
    pub(crate) const fn handles(verb: Verb) -> bool {
        matches!(
            verb,
            Verb::Claim | Verb::Heartbeat | Verb::Release | Verb::Assign | Verb::Found | Verb::Done
        )
    }
}

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
    /// Acts on a ledger verb ingest has just written in `tx`, posting the
    /// hub's answer in `tx` too, so the message, its effects and the answer
    /// commit together or not at all.
    pub(crate) async fn apply_ledger(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        topics: &Topics,
        msg: &LedgerMessage,
    ) -> Result<(), LedgerError> {
        let project = topics.project().as_str();
        match &msg.env.payload {
            Payload::Claim { item } => claim(tx, topics, msg, item, self.claim_lease()).await,
            Payload::Heartbeat { items } => {
                renew(tx, project, &msg.from, items, self.claim_lease()).await
            }
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

/// The RELEASE `lease_expired` for `item`, mentioning the holder.
fn lease_expired(
    item: &ItemKey,
    lane: Option<LaneName>,
    lapsed: &Lapsed,
) -> Result<HubPost, LedgerError> {
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
async fn claim(
    tx: &mut Transaction<'_, Postgres>,
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
        decide(tx, topics.project().as_str(), msg, item, lease).await?
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

/// A claim whose lease ran out, released on the way to a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lapsed {
    holder: IdentityId,
    handle: String,
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

/// `lease` in milliseconds, as SQL takes it.
fn lease_ms(lease: Duration) -> i64 {
    i64::try_from(lease.as_millis()).unwrap_or(i64::MAX)
}

fn timestamp_ms(ms: i64) -> Result<Timestamp, LedgerError> {
    Timestamp::from_millisecond(ms).map_err(stored)
}

/// An item's state, lane, assignee and lane owner.
type ItemLock = (String, Option<String>, Option<String>, Option<String>);

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
async fn decide(
    tx: &mut Transaction<'_, Postgres>,
    project: &str,
    msg: &LedgerMessage,
    item: &ItemKey,
    lease: Duration,
) -> Result<Decided, LedgerError> {
    let row: Option<ItemLock> = sqlx::query_as(
        "SELECT i.state, i.lane, i.assigned_to, l.owner_identity_id
         FROM items i
         LEFT JOIN lanes l ON l.project_id = i.project_id AND l.name = i.lane
         WHERE i.project_id = $1 AND i.key = $2
         FOR UPDATE OF i",
    )
    .bind(project)
    .bind(item.as_str())
    .fetch_optional(&mut **tx)
    .await?;
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
    let lapsed = release_lapsed(tx, project, item).await?;
    let outcome = match take(tx, project, item, &msg.from, lease).await? {
        Taken::Granted(ms) => Outcome::Granted {
            lease_until: timestamp_ms(ms)?,
        },
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

/// What [`take`] came to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Taken {
    /// Granted, or renewed for the holder; the lease end in milliseconds.
    Granted(i64),
    /// Another identity, with this handle, holds the item.
    Held(String),
}

/// Grants `item` to `from`: renews the lease when `from` already holds it,
/// refuses when another identity does, else inserts the claim. The item's
/// row lock makes a concurrent insert impossible; if the unique index still
/// refuses one, the error aborts the CLAIM's transaction and the client
/// resends.
async fn take(
    tx: &mut Transaction<'_, Postgres>,
    project: &str,
    item: &ItemKey,
    from: &IdentityId,
    lease: Duration,
) -> Result<Taken, LedgerError> {
    let holder: Option<(String, String)> = sqlx::query_as(
        "SELECT c.identity_id, h.handle FROM claims c JOIN identities h ON h.id = c.identity_id
         WHERE c.project_id = $1 AND c.item_key = $2 AND c.released_at IS NULL",
    )
    .bind(project)
    .bind(item.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    match holder {
        Some((holder, _)) if holder == from.as_str() => Ok(Taken::Granted(
            sqlx::query_scalar(
                "UPDATE claims SET lease_until = now() + $3 * interval '1 millisecond'
                 WHERE project_id = $1 AND item_key = $2 AND released_at IS NULL
                 RETURNING (extract(epoch FROM lease_until) * 1000)::bigint",
            )
            .bind(project)
            .bind(item.as_str())
            .bind(lease_ms(lease))
            .fetch_one(&mut **tx)
            .await?,
        )),
        Some((_, handle)) => Ok(Taken::Held(handle)),
        None => Ok(Taken::Granted(
            sqlx::query_scalar(
                "INSERT INTO claims (project_id, item_key, identity_id, lease_until)
                 VALUES ($1, $2, $3, now() + $4 * interval '1 millisecond')
                 RETURNING (extract(epoch FROM lease_until) * 1000)::bigint",
            )
            .bind(project)
            .bind(item.as_str())
            .bind(from.as_str())
            .bind(lease_ms(lease))
            .fetch_one(&mut **tx)
            .await?,
        )),
    }
}

/// Releases `item`'s active claim if its lease has run out.
async fn release_lapsed(
    tx: &mut Transaction<'_, Postgres>,
    project: &str,
    item: &ItemKey,
) -> Result<Option<Lapsed>, LedgerError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "UPDATE claims c SET released_at = now() FROM identities h
         WHERE c.project_id = $1 AND c.item_key = $2 AND c.released_at IS NULL
           AND c.lease_until <= now() AND h.id = c.identity_id
         RETURNING c.identity_id, h.handle",
    )
    .bind(project)
    .bind(item.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|(holder, handle)| {
        Ok(Lapsed {
            holder: holder.parse().map_err(stored)?,
            handle,
        })
    })
    .transpose()
}

/// HEARTBEAT: renews the leases `from` holds on `items`. A lease that has
/// already run out stays out; the sweeper releases it.
async fn renew(
    conn: &mut PgConnection,
    project: &str,
    from: &IdentityId,
    items: &[ItemKey],
    lease: Duration,
) -> Result<(), LedgerError> {
    let keys: Vec<&str> = items.iter().map(ItemKey::as_str).collect();
    let renewed = sqlx::query(
        "UPDATE claims SET lease_until = now() + $4 * interval '1 millisecond'
         WHERE project_id = $1 AND identity_id = $2 AND item_key = ANY($3)
           AND released_at IS NULL AND lease_until > now()",
    )
    .bind(project)
    .bind(from.as_str())
    .bind(&keys)
    .bind(lease_ms(lease))
    .execute(conn)
    .await?;
    tracing::debug!(%from, renewed = renewed.rows_affected(), "heartbeat");
    Ok(())
}

/// RELEASE: ends `item`'s active claim when the sender holds it or is a
/// coordinator (protocol 4). From anyone else it changes nothing.
async fn release(
    conn: &mut PgConnection,
    project: &str,
    msg: &LedgerMessage,
    item: &ItemKey,
) -> Result<(), LedgerError> {
    let released = sqlx::query(
        "UPDATE claims SET released_at = now()
         WHERE project_id = $1 AND item_key = $2 AND released_at IS NULL
           AND (identity_id = $3 OR $4)",
    )
    .bind(project)
    .bind(item.as_str())
    .bind(msg.from.as_str())
    .bind(msg.from_kind == IdentityKind::Coordinator)
    .execute(conn)
    .await?;
    if released.rows_affected() == 0 {
        tracing::debug!(%item, from = %msg.from, "RELEASE released nothing");
    }
    Ok(())
}

/// One identity's active claim on an item of a lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaneHolder {
    pub(crate) lane: LaneName,
    pub(crate) identity: IdentityId,
    pub(crate) handle: String,
}

/// Who holds a live claim on an item with a lane in `project`, oldest claim
/// first.
pub(crate) async fn lane_holders(
    db: impl PgExecutor<'_>,
    project: &str,
) -> Result<Vec<LaneHolder>, LedgerError> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT i.lane, c.identity_id, h.handle
         FROM claims c
         JOIN items i ON i.project_id = c.project_id AND i.key = c.item_key
         JOIN identities h ON h.id = c.identity_id
         WHERE c.project_id = $1 AND c.released_at IS NULL AND c.lease_until > now()
           AND i.lane IS NOT NULL
         ORDER BY c.granted_at, c.id",
    )
    .bind(project)
    .fetch_all(db)
    .await?;
    rows.into_iter()
        .map(|(lane, identity, handle)| {
            Ok(LaneHolder {
                lane: lane.parse().map_err(stored)?,
                identity: identity.parse().map_err(stored)?,
                handle,
            })
        })
        .collect()
}
