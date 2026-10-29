//! Admission, routing and atomic message effects, in protocol 5.2 order.

use std::fmt;
use std::future::Future;
use std::sync::PoisonError;
use std::time::{Duration, Instant};

use agmen_proto::{
    ClientEnvelope, DeliveryReply, IdentityId, IdentityKind, MessageId, NackReason, Payload,
    Stream, Timestamp, Topics, Verb, validate,
};

use crate::record::{backend_context, database_error, head_key};
use crate::secrets_scan::find_secret;
use crate::{
    AckCopy, Core, Draft, LedgerMessage, Reads, Roster, Sender, Store, StoreError, StoreTx,
    candidate_item_keys, envelope_hash, hub_now, may_send, mentioned, message_id, needs_roster,
    route, stamp_refs, write,
};

const BUSY_RETRY_MS: u64 = 1_000;
const ERROR_CHAIN_DEPTH: usize = 17;

/// Whether a tenant may write, with its hosted daily message limit when set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Operator-made tenants and self-host hubs have no quota or counting.
    Open,
    /// Writable hosted tenants count recorded messages against this limit.
    Quota {
        /// The most recorded messages per UTC day.
        limit: u32,
    },
    /// Only RELEASE and BYE may be ingested until the owner pays.
    ReadOnly,
}

/// The result of taking one message from the tenant's daily quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaTaken {
    /// This transaction counted a message and may own today's owner warning.
    Counted {
        /// The number of messages counted today.
        messages: u32,
        /// Whether this transaction owns the day's 80 percent warning.
        warn: bool,
    },
    /// No message remains today; the counter was not changed.
    Exhausted,
}

/// Who sends the reply to a publish on `ack/{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyRoute {
    /// The ACK is in the outbox, behind the message's copies.
    Outbox,
    /// Nothing was queued: the caller publishes the reply itself.
    Caller,
}

/// A newly committed message for the transport's post-commit callbacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMeta {
    /// The recorded verb.
    pub verb: Verb,
    /// The stamped sender.
    pub from: IdentityId,
    /// The stamped sender kind.
    pub from_kind: IdentityKind,
    /// The client message id.
    pub id: MessageId,
}

/// The answer to one publish and its committed effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handled {
    /// The client's delivery reply.
    pub reply: DeliveryReply,
    /// Who publishes the reply.
    pub route: ReplyRoute,
    /// Present only when this call committed a new message.
    pub recorded: Option<RecordedMeta>,
    /// The quota limit when this call owns today's owner warning.
    pub quota_warning: Option<u32>,
}

/// Why a publish could not be answered at all.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IngestError {
    /// The payload has no readable `id`, so no reply can name it.
    #[error("the publish has no readable message id, so it cannot be answered")]
    NoMessageId,
    /// The topic names no project in this store for the tenant.
    #[error("the topic names a project this hub does not have")]
    UnknownProject,
    /// The project lookup failed; other store errors produce a NACK.
    #[error("looking up the project failed")]
    Store(#[source] StoreError),
}

impl<S: Store> Core<S> {
    /// Admits, routes and records a publish atomically, with its ACK and ledger effects.
    ///
    /// # Errors
    /// Returns [`IngestError`] when the publish has no readable id or its project
    /// cannot be found. Admission and transaction failures return a NACK.
    ///
    /// ```no_run
    /// # async fn example<S: agmen_core::Store>(core: agmen_core::Core<S>, topics: agmen_proto::Topics, from: agmen_proto::IdentityId) -> Result<(), agmen_core::IngestError> {
    /// let handled = core.ingest(&topics, &from, b"{}", agmen_core::Gate::Open).await?;
    /// # let _ = handled;
    /// # Ok(()) }
    /// ```
    pub async fn ingest(
        &self,
        topics: &Topics,
        from: &IdentityId,
        raw: &[u8],
        gate: Gate,
    ) -> Result<Handled, IngestError> {
        self.ingest_with_gate(topics, from, raw, std::future::ready(Ok(gate)))
            .await
    }

    /// Loads a transport's gate only after admission and resend detection.
    ///
    /// Hosted plan reads can fail, so awaiting the gate before admission would
    /// change which refusal wins and would prevent resends from getting their ACK.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::ingest`]. Gate failures return `hub_busy`.
    pub async fn ingest_with_gate(
        &self,
        topics: &Topics,
        from: &IdentityId,
        raw: &[u8],
        gate: impl Future<Output = Result<Gate, StoreError>> + Send,
    ) -> Result<Handled, IngestError> {
        self.ingest_at(topics, from, raw, gate, Instant::now).await
    }

    async fn ingest_at(
        &self,
        topics: &Topics,
        from: &IdentityId,
        raw: &[u8],
        gate: impl Future<Output = Result<Gate, StoreError>> + Send,
        instant: impl FnOnce() -> Instant + Send,
    ) -> Result<Handled, IngestError> {
        let id = message_id(raw).ok_or(IngestError::NoMessageId)?;
        if !self
            .store()
            .reader()
            .project_exists(topics)
            .await
            .map_err(IngestError::Store)?
        {
            return Err(IngestError::UnknownProject);
        }
        let hub_at = hub_now();
        match self.accept(topics, from, raw, gate, hub_at, instant).await {
            Ok(handled) => Ok(handled),
            Err(refused) => {
                tracing::debug!(%from, %id, reason = %refused.reason, "refused");
                Ok(Handled {
                    reply: DeliveryReply::Nack {
                        id,
                        retryable: refused.reason.is_retryable(),
                        retry_after_ms: refused.retry_after_ms,
                        reason: refused.reason,
                    },
                    route: ReplyRoute::Caller,
                    recorded: None,
                    quota_warning: None,
                })
            }
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "keep validation, routing and atomic message effects in protocol order"
    )]
    async fn accept(
        &self,
        topics: &Topics,
        from: &IdentityId,
        raw: &[u8],
        gate: impl Future<Output = Result<Gate, StoreError>> + Send,
        hub_at: Timestamp,
        instant: impl FnOnce() -> Instant + Send,
    ) -> Result<Handled, Refused> {
        let admitted = self.admit(topics, from, raw, hub_at, instant).await?;
        if let Some(reply) = recorded_reply(
            &mut self.store().reader(),
            &admitted.sender.id,
            &admitted.env.id,
            &admitted.hash,
        )
        .await?
        {
            return Ok(Handled {
                reply,
                route: ReplyRoute::Caller,
                recorded: None,
                quota_warning: None,
            });
        }
        let Admitted {
            env,
            verb,
            sender,
            hash,
        } = admitted;
        let gate = gate.await.map_err(database_error)?;
        if gate == Gate::ReadOnly && !matches!(verb, Verb::Release | Verb::Bye) {
            return Err(NackReason::ReadOnly.into());
        }
        let roster = if needs_roster(verb, &env) {
            self.store()
                .reader()
                .roster(topics.project())
                .await
                .map_err(database_error)?
        } else {
            Roster::default()
        };
        let parent = match &env.reply_to {
            Some(reply_to) => self
                .store()
                .reader()
                .parent(topics.project(), &sender.id, reply_to)
                .await
                .map_err(database_error)?,
            None => None,
        };
        if verb == Verb::Review && !parent.as_ref().is_some_and(|parent| parent.verb == "DONE") {
            return Err(NackReason::Schema.into());
        }
        if let Payload::Delegated { worker, .. } = &env.payload {
            let valid = self
                .store()
                .reader()
                .is_delegate(topics.project(), worker, &sender.id)
                .await
                .map_err(database_error)?;
            if !valid {
                return Err(NackReason::Schema.into());
            }
        }
        let refs = if matches!(
            verb,
            Verb::Fyi | Verb::Ask | Verb::Answer | Verb::Alert | Verb::Done | Verb::Review
        ) {
            let candidates = candidate_item_keys(env.body.as_str());
            let (lanes, keys) = self
                .store()
                .reader()
                .ref_targets(topics.project(), &candidates)
                .await
                .map_err(database_error)?;
            stamp_refs(env.body.as_str(), &roster, &lanes, &keys)
        } else {
            Vec::new()
        };
        let mentioned = mentioned(verb, &env, &sender, &roster, parent.as_ref())?;
        let streams = route(verb, &env, &sender, &roster, parent.as_ref())?;
        let id = env.id.clone();
        let lend = (verb == Verb::Lend).then(|| env.clone());
        let ledger = LedgerMessage::handles(verb).then(|| LedgerMessage {
            env: env.clone(),
            from: sender.id.clone(),
            from_handle: sender.handle.clone(),
            from_kind: sender.kind,
            from_owner: sender.owner.clone(),
            hub_at,
        });
        // A human posts over HTTP and reads the reply from the response, so
        // no ACK is queued for a topic nobody listens on.
        let (ack, route) = if sender.kind == IdentityKind::Human {
            (AckCopy::None, ReplyRoute::Caller)
        } else {
            (AckCopy::Queue, ReplyRoute::Outbox)
        };
        let draft = Draft {
            topics,
            client: env,
            from: sender.id.clone(),
            from_handle: sender.handle,
            from_kind: sender.kind,
            mentioned,
            refs,
            streams,
            hash: hash.clone(),
            hub_at,
            ack,
        };
        let mut tx = self.store().begin().await.map_err(database_error)?;
        let quota_warning = if let Gate::Quota { limit } = gate
            && !matches!(verb, Verb::Heartbeat | Verb::Release | Verb::Bye)
        {
            match tx
                .take_quota(topics.tenant(), limit)
                .await
                .map_err(database_error)?
            {
                QuotaTaken::Exhausted => {
                    // A copy of this message may have taken the last slot
                    // while this one waited on the counter: answer as for a
                    // resend instead of refusing a recorded message.
                    drop(tx);
                    if let Some(reply) =
                        recorded_reply(&mut self.store().reader(), &sender.id, &id, &hash).await?
                    {
                        return Ok(Handled {
                            reply,
                            route: ReplyRoute::Caller,
                            recorded: None,
                            quota_warning: None,
                        });
                    }
                    let now = Timestamp::now();
                    let millis = next_utc_midnight(now).as_millisecond() - now.as_millisecond();
                    return Err(Refused::retry(
                        NackReason::QuotaExceeded,
                        Duration::from_millis(millis.unsigned_abs()),
                    ));
                }
                QuotaTaken::Counted { messages, warn } => {
                    debug_assert!(messages <= limit);
                    warn.then_some(limit)
                }
            }
        } else {
            None
        };
        if ledger.is_some() {
            // Ledger work locks item and claim rows, then posts to the feed.
            // Taking the feed's head first gives every ledger transaction
            // the lock order of a plain feed message (feed head first), so
            // two of them cannot deadlock.
            tx.lock_head(&head_key(topics, &Stream::Feed))
                .await
                .map_err(|err| backend_context(err, "numbering failed"))?;
        }
        match write(&mut tx, draft).await {
            Ok(recorded) => {
                if let Some(lend) = &lend {
                    crate::lend(&mut tx, topics, &sender.id, sender.kind, lend)
                        .await
                        .map_err(|err| match err {
                            crate::LendError::Refused(reason) => Refused::from(reason),
                            crate::LendError::Store(err) => Refused::internal(err),
                        })?;
                }
                // The ledger runs in the message's transaction: if it fails,
                // nothing is recorded and the NACK `hub_busy` makes the
                // client resend, which runs it again.
                if let Some(ledger) = &ledger {
                    crate::apply_ledger(&mut tx, topics, ledger, self.claim_lease())
                        .await
                        .map_err(|err| {
                            Refused::internal(format!("ledger: {}", error_chain(&err)))
                        })?;
                }
                tx.commit().await.map_err(database_error)?;
                let (stream, seq) = recorded.ack;
                let draft_id = id.clone();
                Ok(Handled {
                    reply: DeliveryReply::Ack { id, stream, seq },
                    route,
                    recorded: Some(RecordedMeta {
                        verb,
                        from: sender.id,
                        from_kind: sender.kind,
                        id: draft_id,
                    }),
                    quota_warning,
                })
            }
            // A copy of the same message committed first: answer as for a
            // resend.
            Err(StoreError::Duplicate) => {
                drop(tx);
                match recorded_reply(&mut self.store().reader(), &sender.id, &id, &hash).await? {
                    Some(reply) => Ok(Handled {
                        reply,
                        route: ReplyRoute::Caller,
                        recorded: None,
                        quota_warning: None,
                    }),
                    None => Err(Refused::internal("duplicate row vanished")),
                }
            }
            Err(err) => Err(err.into()),
        }
    }

    /// The checks that need nothing but the publish and its sender, in
    /// protocol 5.2's order: rate limit, schema and size, `valid_until` on
    /// the hub's clock, the sender, the verb, and secrets.
    async fn admit(
        &self,
        topics: &Topics,
        from: &IdentityId,
        raw: &[u8],
        hub_at: Timestamp,
        instant: impl FnOnce() -> Instant + Send,
    ) -> Result<Admitted, Refused> {
        self.limiter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(from, instant())
            .map_err(|retry| match retry {
                Some(after) => Refused::retry(NackReason::RateLimited, after),
                None => Refused::retry(NackReason::HubBusy, Duration::from_millis(BUSY_RETRY_MS)),
            })?;
        let env = ClientEnvelope::from_json(raw).map_err(|err| NackReason::from(&err))?;
        match validate(&env, hub_at) {
            // A late CLAIM is recorded and answered with DENY expired
            // (protocol 3).
            Err(NackReason::Expired) if matches!(env.payload, Payload::Claim { .. }) => {}
            checked => checked?,
        }
        let verb = env.payload.verb().ok_or(NackReason::Schema)?;
        let sender = self
            .store()
            .reader()
            .sender(topics, from)
            .await
            .map_err(database_error)?
            .ok_or(NackReason::IdentityRevoked)?;
        if !sender.verbs.as_ref().map_or_else(
            || may_send(sender.kind, verb),
            |verbs| verbs.iter().any(|v| v == verb.as_str()),
        ) {
            return Err(NackReason::ForbiddenVerb.into());
        }
        if verb == Verb::PushRequest {
            // Push approval is out of the MVP, so every agent's effective
            // push_approval is `off` (protocol 7): push without asking.
            return Err(NackReason::NotRequired.into());
        }
        let data = serde_json::to_string(&env.payload).map_err(Refused::internal)?;
        if let Some(rule) = find_secret(env.body.as_str()).or_else(|| find_secret(&data)) {
            tracing::info!(%from, id = %env.id, rule, "refused secret-shaped text");
            return Err(NackReason::SecretDetected.into());
        }
        let hash = envelope_hash(&env).map_err(Refused::internal)?;
        Ok(Admitted {
            env,
            verb,
            sender,
            hash,
        })
    }
}

/// A NACK about to be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Refused {
    reason: NackReason,
    retry_after_ms: Option<u64>,
}

impl Refused {
    fn retry(reason: NackReason, after: Duration) -> Self {
        let ms = u64::try_from(after.as_millis())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        Self {
            reason,
            retry_after_ms: Some(ms),
        }
    }

    /// A failure of the hub's own: logged, and answered `hub_busy`.
    fn internal(err: impl fmt::Display) -> Self {
        tracing::error!(error = %err, "ingest failed");
        Self::retry(NackReason::HubBusy, Duration::from_millis(BUSY_RETRY_MS))
    }
}

impl From<NackReason> for Refused {
    fn from(reason: NackReason) -> Self {
        Self {
            reason,
            retry_after_ms: None,
        }
    }
}

impl From<StoreError> for Refused {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::Stored(message) if message == crate::record::TOO_LARGE => {
                NackReason::Schema.into()
            }
            StoreError::Backend(source) => Self::internal(error_chain(source.as_ref())),
            other => Self::internal(error_chain(&other)),
        }
    }
}

/// A publish that passed the checks that need no roster.
#[derive(Debug)]
struct Admitted {
    env: ClientEnvelope,
    verb: Verb,
    sender: Sender,
    hash: Vec<u8>,
}

async fn recorded_reply(
    reader: &mut impl Reads,
    from: &IdentityId,
    id: &MessageId,
    hash: &[u8],
) -> Result<Option<DeliveryReply>, StoreError> {
    let Some((stream, seq, recorded_hash)) =
        reader.recorded(from, id).await.map_err(database_error)?
    else {
        return Ok(None);
    };
    if recorded_hash != hash {
        return Ok(Some(DeliveryReply::Nack {
            id: id.clone(),
            retryable: false,
            retry_after_ms: None,
            reason: NackReason::Conflict,
        }));
    }
    Ok(Some(DeliveryReply::Ack {
        id: id.clone(),
        stream,
        seq,
    }))
}

/// The next UTC midnight strictly after `now`.
///
/// An instant that is already midnight resolves to the following day. Quota
/// counters reset at that instant.
///
/// # Examples
///
/// ```
/// use agmen_proto::Timestamp;
/// use agmen_core::next_utc_midnight;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let almost: Timestamp = "2026-11-02T23:59:59Z".parse()?;
/// let midnight: Timestamp = "2026-11-03T00:00:00Z".parse()?;
/// assert_eq!(next_utc_midnight(almost), midnight);
/// assert_eq!(next_utc_midnight(midnight).to_string(), "2026-11-04T00:00:00Z");
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn next_utc_midnight(now: Timestamp) -> Timestamp {
    let day = 86_400_i64;
    let next_day = floor_second(now).div_euclid(day).saturating_add(1);
    let next = next_day.saturating_mul(day);
    match Timestamp::from_second(next) {
        Ok(midnight) => midnight,
        Err(_) => {
            if next < 0 {
                Timestamp::MIN
            } else {
                Timestamp::MAX
            }
        }
    }
}

fn floor_second(now: Timestamp) -> i64 {
    let second = now.as_second();
    if now.subsec_nanosecond() < 0 {
        second.saturating_sub(1)
    } else {
        second
    }
}

fn error_chain(err: &dyn std::error::Error) -> String {
    std::iter::successors(Some(err), |e| e.source())
        .take(ERROR_CHAIN_DEPTH)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
mod tests;
