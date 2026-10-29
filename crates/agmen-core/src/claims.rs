//! Claims, renewals and releases in the caller's message transaction.

use std::time::Duration;

use crate::{HubPost, Lapsed, PostError, ReleaseBy, StoreError, StoreTx, post_in};
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
// shortcut: public bridge for the hub sweeper until Task 8 moves expiry.
#[doc(hidden)]
pub fn lease_expired(
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

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration;

    use agmen_proto::{
        ClientEnvelope, DenyReason, IdentityKind, Payload, Stream, Timestamp, Topics,
    };

    use crate::{
        AckCopy, Draft, LedgerError, LedgerMessage, MemStore, NewItem, Reads, ResolvedLane, Store,
        StoreTx, apply_ledger, envelope_hash, new_message_id, write,
    };

    pub(crate) const LEASE: Duration = Duration::from_secs(60);

    pub(crate) async fn setup() -> (MemStore, Topics) {
        let store = MemStore::default();
        let topics = Topics::new("tenant01".parse().unwrap(), "project01".parse().unwrap());
        for (id, handle, kind) in [
            ("aaaaaaaa", "a", IdentityKind::Agent),
            ("bbbbbbbb", "b", IdentityKind::Agent),
            ("oooooooo", "o", IdentityKind::Agent),
            ("cccccccc", "c", IdentityKind::Coordinator),
        ] {
            store
                .seed_live_identity(topics.project(), &id.parse().unwrap(), handle, kind)
                .await
                .unwrap();
        }
        (store, topics)
    }

    pub(crate) fn message(from: &str, handle: &str, payload: Payload) -> LedgerMessage {
        let now = Timestamp::now();
        LedgerMessage {
            env: ClientEnvelope {
                v: 1,
                id: new_message_id(now).unwrap(),
                reply_to: None,
                to: Vec::new(),
                mentions: Vec::new(),
                items: Vec::new(),
                lane: None,
                body: "ledger test".try_into().unwrap(),
                valid_until: Some(now + LEASE),
                sent_at: now,
                payload,
            },
            from: from.parse().unwrap(),
            from_handle: handle.into(),
            from_kind: IdentityKind::Agent,
            from_owner: None,
            hub_at: now,
        }
    }

    pub(crate) async fn item(store: &MemStore, topics: &Topics, owned: bool) {
        let mut tx = store.begin().await.unwrap();
        tx.replace_lanes(
            topics.project(),
            &[ResolvedLane {
                name: "L-UI".parse().unwrap(),
                globs: vec!["ui/**".into()],
                owner: owned.then(|| "oooooooo".parse().unwrap()),
            }],
        )
        .await
        .unwrap();
        tx.insert_item(&NewItem {
            project: topics.project(),
            key: &"B-1".parse().unwrap(),
            title: "bug",
            lane: Some(&"L-UI".parse().unwrap()),
            severity: None,
            created_by: &"aaaaaaaa".parse().unwrap(),
        })
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    pub(crate) async fn apply(store: &MemStore, topics: &Topics, msg: &LedgerMessage) {
        let mut tx = store.begin().await.unwrap();
        apply_ledger(&mut tx, topics, msg, LEASE).await.unwrap();
        tx.commit().await.unwrap();
    }

    pub(crate) fn claim(from: &str, handle: &str) -> LedgerMessage {
        message(
            from,
            handle,
            Payload::Claim {
                item: "B-1".parse().unwrap(),
            },
        )
    }

    #[tokio::test]
    async fn claim_grants_then_denies_held_by() {
        let (store, topics) = setup().await;
        item(&store, &topics, false).await;
        let now = Timestamp::from_millisecond(Timestamp::now().as_millisecond()).unwrap();
        store.set_now(now);
        apply(&store, &topics, &claim("aaaaaaaa", "a")).await;
        apply(&store, &topics, &claim("bbbbbbbb", "b")).await;
        store.set_now(now + Duration::from_secs(1));
        apply(&store, &topics, &claim("aaaaaaaa", "a")).await;
        let rows = store.recorded_messages().await.unwrap();
        let feed: Vec<_> = rows
            .iter()
            .filter(|row| row.stream == Stream::Feed)
            .collect();
        assert_eq!(feed.len(), 3);
        assert_eq!(
            feed[0].client.payload,
            Payload::Grant {
                item: "B-1".parse().unwrap(),
                lease_until: now + LEASE,
            }
        );
        assert_eq!(
            feed[1].client.payload,
            Payload::Deny {
                item: "B-1".parse().unwrap(),
                reason: DenyReason::HeldBy,
            }
        );
        assert_eq!(feed[1].client.body.as_str(), "B-1 denied to b: held by a");
        assert_eq!(
            feed[2].client.payload,
            Payload::Grant {
                item: "B-1".parse().unwrap(),
                lease_until: now + Duration::from_secs(1) + LEASE,
            }
        );
        assert_eq!(rows.len(), 6);
        for pair in rows.as_chunks::<2>().0 {
            assert_eq!(pair[0].client.payload, pair[1].client.payload);
            assert!(matches!(pair[1].stream, Stream::Inbox(_)));
        }
    }

    #[tokio::test]
    async fn claim_in_owned_lane_needs_owner_coordinator_or_assignee() {
        let (store, topics) = setup().await;
        item(&store, &topics, true).await;
        apply(&store, &topics, &claim("aaaaaaaa", "a")).await;
        let mut assigned = message(
            "cccccccc",
            "c",
            Payload::Assign {
                item: "B-1".parse().unwrap(),
                to: "aaaaaaaa".parse().unwrap(),
            },
        );
        assigned.from_kind = IdentityKind::Coordinator;
        apply(&store, &topics, &assigned).await;
        apply(&store, &topics, &claim("aaaaaaaa", "a")).await;
        let rows = store.recorded_messages().await.unwrap();
        assert!(matches!(
            rows[0].client.payload,
            Payload::Deny {
                reason: DenyReason::NotInLane,
                ..
            }
        ));
        assert!(matches!(rows[2].client.payload, Payload::Grant { .. }));
        for (id, handle, kind) in [
            ("oooooooo", "o", IdentityKind::Agent),
            ("cccccccc", "c", IdentityKind::Coordinator),
        ] {
            let (store, topics) = setup().await;
            item(&store, &topics, true).await;
            let mut msg = claim(id, handle);
            msg.from_kind = kind;
            apply(&store, &topics, &msg).await;
            assert!(matches!(
                store.recorded_messages().await.unwrap()[0].client.payload,
                Payload::Grant { .. }
            ));
        }
    }

    #[tokio::test]
    async fn lapsed_claim_is_released_before_granting() {
        let (store, topics) = setup().await;
        item(&store, &topics, false).await;
        let now = Timestamp::now();
        store.set_now(now);
        apply(&store, &topics, &claim("aaaaaaaa", "a")).await;
        store.set_now(now + LEASE);
        apply(&store, &topics, &claim("bbbbbbbb", "b")).await;
        let rows = store.recorded_messages().await.unwrap();
        let feed: Vec<_> = rows
            .iter()
            .filter(|row| row.stream == Stream::Feed)
            .collect();
        assert_eq!(feed.len(), 3);
        assert_eq!(
            feed[1].client.payload,
            Payload::Release {
                item: "B-1".parse().unwrap(),
                reason: Some("lease_expired".into()),
            }
        );
        assert_eq!(feed[1].mentioned, vec!["aaaaaaaa".parse().unwrap()]);
        assert_eq!(
            feed[1].client.body.as_str(),
            "B-1 released: a's lease ran out"
        );
        assert!(matches!(feed[2].client.payload, Payload::Grant { .. }));
        assert!(feed[1].seq < feed[2].seq);
    }

    #[tokio::test]
    async fn ledger_failure_records_nothing() {
        let (store, topics) = setup().await;
        item(&store, &topics, false).await;
        let msg = claim("aaaaaaaa", "a");
        store.fail_next("insert_claim");
        let mut tx = store.begin().await.unwrap();
        tx.lock_head(&format!("{}/feed", topics.project()))
            .await
            .unwrap();
        write(
            &mut tx,
            Draft {
                topics: &topics,
                client: msg.env.clone(),
                from: msg.from.clone(),
                from_handle: msg.from_handle.clone(),
                from_kind: msg.from_kind,
                mentioned: Vec::new(),
                refs: Vec::new(),
                streams: [Stream::Feed].into_iter().collect(),
                hash: envelope_hash(&msg.env).unwrap(),
                hub_at: msg.hub_at,
                ack: AckCopy::Queue,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            apply_ledger(&mut tx, &topics, &msg, LEASE).await,
            Err(LedgerError::Store(_))
        ));
        drop(tx);
        assert!(store.recorded_messages().await.unwrap().is_empty());
        assert!(store.queued_publishes().await.unwrap().is_empty());
        assert!(
            store
                .reader()
                .lane_holders(topics.project())
                .await
                .unwrap()
                .is_empty()
        );
        let mut tx = store.begin().await.unwrap();
        assert!(
            tx.active_claim(topics.project(), &"B-1".parse().unwrap())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            tx.next_seq(&format!("{}/feed", topics.project()))
                .await
                .unwrap(),
            agmen_proto::Seq(1)
        );
    }
}
