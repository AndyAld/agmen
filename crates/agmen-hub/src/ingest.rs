//! Ingest: what the hub does with a client's publish on `up/{identity_id}`
//! (protocol 5.2), and the path the hub posts its own messages on (ruling
//! R34).
//!
//! An accepted message is numbered and stored in one transaction, together
//! with a `publish_outbox` row for each copy and one for the ACK, and for a
//! ledger verb the ledger's work and the hub's answer; the sweeper publishes
//! the outbox. A refused message gets a NACK and leaves nothing behind.

use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use agmen_core::{
    AckCopy, Draft, LedgerMessage, Parent, RateLimiter, Reads, Roster, Sender, Store, StoreTx,
    candidate_item_keys, envelope_hash, hub_now, may_send, mentioned, message_id, needs_roster,
    parse_kind, route, stamp_refs, write,
};
pub(crate) use agmen_core::{HubPost, PostError, Posted};
use agmen_proto::{
    ClientEnvelope, DeliveryReply, IdentityId, IdentityKind, MessageId, NackReason, Payload,
    ProjectId, Seq, Stream, Timestamp, Topics, Verb, validate,
};
use sqlx::PgPool;
use tokio::sync::{Notify, watch};

use crate::events::Wakes;
use crate::http::{Hub, error_chain};
use crate::numbering::{NumberingError, lock_head};
use crate::secrets_scan::find_secret;
use crate::usage::{Gate, Taken, post_owner_alert, take_quota, with_thousands, write_gate};

/// What a `hub_busy` NACK tells the client to wait.
const BUSY_RETRY_MS: u64 = 1_000;

/// Why a publish could not be answered at all.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IngestError {
    /// The payload has no readable `id`, so no ACK or NACK can name it.
    #[error("the publish has no readable message id, so it cannot be answered")]
    NoMessageId,
    /// No project of this hub has the topic's tenant and project. Another
    /// hub on the same broker may own it, so this one stays silent.
    #[error("the topic names a project this hub does not have")]
    UnknownProject,
    /// The project could not be looked up.
    #[error("looking up the project failed")]
    Database(#[source] sqlx::Error),
}

/// Who sends the reply to a publish on `ack/{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplyRoute {
    /// The ACK is in the outbox, behind the message's copies.
    Outbox,
    /// Nothing was queued: the caller publishes the reply itself.
    Caller,
}

/// The answer to one publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Handled {
    pub(crate) reply: DeliveryReply,
    pub(crate) route: ReplyRoute,
}

/// Ingest's shared state: the rate limiter, the sweeper's wake-up and the
/// events streams' wake-up.
#[derive(Debug, Default)]
pub(crate) struct IngestState {
    limiter: Mutex<RateLimiter>,
    outbox_ready: Notify,
    stored: Wakes,
}

impl IngestState {
    /// Signalled after each commit that added outbox rows.
    pub(crate) fn outbox_ready(&self) -> &Notify {
        &self.outbox_ready
    }

    /// Call after each commit that stored messages in `project`: wakes the
    /// sweeper and that project's events streams.
    pub(crate) fn stored(&self, project: &ProjectId) {
        self.outbox_ready.notify_one();
        self.stored.wake(project);
    }

    /// A receiver that sees a change after each [`IngestState::stored`]
    /// for `project`.
    pub(crate) fn watch_stored(&self, project: &ProjectId) -> watch::Receiver<()> {
        self.stored.watch(project)
    }
}

impl Hub {
    /// Handles one publish from `from` on `topics`' `up/` topic, or from a
    /// human posting to that project, and returns the ACK or NACK
    /// (protocol 5.2): parse and validate against the hub's clock, rate
    /// limit (10 per second, burst 30, per sender), check that the sender
    /// is a live member of the project and may send the verb, scan for
    /// credentials, dedupe on `(from, id)`, expand mentions, then number
    /// and store every copy with its outbox rows. A fresh ACK from an
    /// identity is also queued for `ack/{from}` behind the message itself;
    /// a human's is not, because the human reads it from the HTTP reply.
    ///
    /// A ledger verb (CLAIM, HEARTBEAT, RELEASE, ASSIGN, FOUND, DONE) is
    /// acted on, and the hub's answer posted, in the same transaction.
    /// Storage and ledger failures record nothing and answer `hub_busy`,
    /// which the client retries.
    ///
    /// # Errors
    ///
    /// [`IngestError::NoMessageId`] when `raw` has no readable `id`,
    /// [`IngestError::UnknownProject`] when no project of this hub matches
    /// `topics`, and [`IngestError::Database`] when that lookup fails. None
    /// of them can be answered.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(hub: agmen_hub::Hub) -> Result<(), Box<dyn std::error::Error>> {
    /// use agmen_proto::{DeliveryReply, Topics};
    /// let topics = Topics::new("tenant01".parse()?, "project1".parse()?);
    /// let raw = br#"{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"FYI","data":{},
    ///     "body":"hi","sent_at":"2026-10-25T16:04:11Z"}"#;
    /// let reply = hub.ingest(&topics, &"idn7k2q9".parse()?, raw).await?;
    /// assert!(matches!(reply, DeliveryReply::Ack { .. } | DeliveryReply::Nack { .. }));
    /// # Ok(()) }
    /// ```
    pub async fn ingest(
        &self,
        topics: &Topics,
        from: &IdentityId,
        raw: &[u8],
    ) -> Result<DeliveryReply, IngestError> {
        Ok(self.ingest_publish(topics, from, raw).await?.reply)
    }

    /// [`Hub::ingest`], also saying whether the reply is already queued.
    pub(crate) async fn ingest_publish(
        &self,
        topics: &Topics,
        from: &IdentityId,
        raw: &[u8],
    ) -> Result<Handled, IngestError> {
        let id = message_id(raw).ok_or(IngestError::NoMessageId)?;
        if !project_exists(self.pool(), topics)
            .await
            .map_err(IngestError::Database)?
        {
            return Err(IngestError::UnknownProject);
        }
        match self.accept(topics, from, raw).await {
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
    ) -> Result<Handled, Refused> {
        let hub_at = hub_now();
        let admitted = self.admit(topics, from, raw, hub_at).await?;
        let pool = self.pool();
        if let Some(reply) =
            recorded_reply(pool, &admitted.sender.id, &admitted.env.id, &admitted.hash).await?
        {
            return Ok(Handled {
                reply,
                route: ReplyRoute::Caller,
            });
        }
        let Admitted {
            env,
            verb,
            sender,
            hash,
        } = admitted;
        let gate = write_gate(self, topics.tenant())
            .await
            .map_err(StoreError::from)?;
        if gate == Gate::ReadOnly && !matches!(verb, Verb::Release | Verb::Bye) {
            return Err(NackReason::ReadOnly.into());
        }
        let roster = if needs_roster(verb, &env) {
            self.core()
                .store()
                .reader()
                .roster(topics.project())
                .await
                .map_err(StoreError::from)?
        } else {
            Roster::default()
        };
        let parent = match &env.reply_to {
            Some(reply_to) => find_parent(pool, topics, &sender.id, reply_to).await?,
            None => None,
        };
        if verb == Verb::Review && !parent.as_ref().is_some_and(|parent| parent.verb == "DONE") {
            return Err(NackReason::Schema.into());
        }
        if let Payload::Delegated { worker, .. } = &env.payload {
            let valid: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM identities WHERE id=$1 AND project_id=$2
                 AND delegated_by=$3 AND revoked_at IS NULL)",
            )
            .bind(worker.as_str())
            .bind(topics.project().as_str())
            .bind(sender.id.as_str())
            .fetch_one(pool)
            .await
            .map_err(StoreError::from)?;
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
                .core()
                .store()
                .reader()
                .ref_targets(topics.project(), &candidates)
                .await
                .map_err(StoreError::from)?;
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
        let mut tx = self
            .core()
            .store()
            .begin()
            .await
            .map_err(StoreError::from)?;
        let quota_warning = if let Gate::Quota { limit } = gate
            && !matches!(verb, Verb::Heartbeat | Verb::Release | Verb::Bye)
        {
            match take_quota(tx.sqlx(), topics.tenant(), limit)
                .await
                .map_err(StoreError::from)?
            {
                Taken::Exhausted => {
                    // A copy of this message may have taken the last slot
                    // while this one waited on the counter: answer as for a
                    // resend instead of refusing a recorded message.
                    drop(tx);
                    if let Some(reply) = recorded_reply(pool, &sender.id, &id, &hash).await? {
                        return Ok(Handled {
                            reply,
                            route: ReplyRoute::Caller,
                        });
                    }
                    let now = Timestamp::now();
                    let millis =
                        crate::next_utc_midnight(now).as_millisecond() - now.as_millisecond();
                    return Err(Refused::retry(
                        NackReason::QuotaExceeded,
                        Duration::from_millis(millis.unsigned_abs()),
                    ));
                }
                Taken::Counted { messages, warn } => {
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
            lock_head(tx.sqlx(), &head_key(topics, &Stream::Feed))
                .await
                .map_err(StoreError::from)?;
        }
        match write(&mut tx, draft).await.map_err(StoreError::from) {
            Ok(recorded) => {
                if let Some(lend) = &lend {
                    agmen_core::lend(&mut tx, topics, &sender.id, sender.kind, lend)
                        .await
                        .map_err(|err| match err {
                            agmen_core::LendError::Refused(reason) => Refused::from(reason),
                            agmen_core::LendError::Store(err) => Refused::internal(err),
                        })?;
                }
                // The ledger runs in the message's transaction: if it fails,
                // nothing is recorded and the NACK `hub_busy` makes the
                // client resend, which runs it again.
                if let Some(ledger) = &ledger {
                    agmen_core::apply_ledger(&mut tx, topics, ledger, self.claim_lease())
                        .await
                        .map_err(|err| {
                            Refused::internal(format!("ledger: {}", error_chain(&err)))
                        })?;
                }
                tx.commit().await.map_err(StoreError::from)?;
                if let Some(limit) = quota_warning {
                    let alerted = async {
                        let name: String = sqlx::query_scalar("SELECT name FROM tenants WHERE id = $1")
                            .bind(topics.tenant().as_str())
                            .fetch_one(pool)
                            .await
                            .map_err(|err| PostError::Store(Box::new(err)))?;
                        let limit = with_thousands(limit);
                        let body = format!(
                            "{name} used 80% of today's {limit} messages; posts stop at {limit} until 00:00 UTC."
                        );
                        post_owner_alert(self, topics.tenant(), &body).await
                    }
                    .await;
                    if let Err(err) = alerted {
                        tracing::warn!(tenant = %topics.tenant(), error = %error_chain(&err), "posting the daily quota alert failed");
                    }
                }
                self.ingest_state().stored(topics.project());
                self.on_recorded(topics, verb, &sender.id, sender.kind, &id)
                    .await;
                let (stream, seq) = recorded.ack;
                Ok(Handled {
                    reply: DeliveryReply::Ack { id, stream, seq },
                    route,
                })
            }
            // A copy of the same message committed first: answer as for a
            // resend.
            Err(StoreError::Duplicate) => {
                drop(tx);
                match recorded_reply(pool, &sender.id, &id, &hash).await? {
                    Some(reply) => Ok(Handled {
                        reply,
                        route: ReplyRoute::Caller,
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
    ) -> Result<Admitted, Refused> {
        self.ingest_state()
            .limiter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(from, Instant::now())
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
        let sender = load_sender(self.pool(), topics, from)
            .await?
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

    /// Posts `post` as the hub in `topics`' project (ruling R34) and
    /// returns where each copy was recorded. The sweeper publishes it like
    /// any other message.
    ///
    /// # Errors
    ///
    /// [`PostError::NotHubVerb`] for a verb the hub does not send,
    /// [`PostError::NoStreams`] when `streams` is empty, and
    /// [`PostError::Random`] or [`PostError::Store`] when minting the id or
    /// storing fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(hub: agmen_hub::Hub) -> Result<(), Box<dyn std::error::Error>> {
    /// use agmen_hub::HubPost;
    /// use agmen_proto::{Payload, Stream, Topics, Urgency, Needs};
    /// let topics = Topics::new("tenant01".parse()?, "project1".parse()?);
    /// let posted = hub
    ///     .post_as_hub(&topics, HubPost {
    ///         payload: Payload::Alert {
    ///             need: "look at B-412".to_owned(),
    ///             urgency: Urgency::Normal,
    ///             needs: Needs::Anyone,
    ///             actions: Vec::new(),
    ///         },
    ///         body: "B-412 changed paths outside its lane".try_into()?,
    ///         reply_to: None,
    ///         items: vec!["B-412".parse()?],
    ///         lane: None,
    ///         mentioned: Vec::new(),
    ///         streams: vec![Stream::Feed],
    ///     })
    ///     .await?;
    /// assert_eq!(posted.copies.len(), 1);
    /// # Ok(()) }
    /// ```
    pub async fn post_as_hub(&self, topics: &Topics, post: HubPost) -> Result<Posted, PostError> {
        let posted = self.core().post_as_hub(topics, post).await?;
        self.ingest_state().stored(topics.project());
        Ok(posted)
    }
}

/// The `stream_heads` key of one project's stream: `<project_id>/<stream>`.
pub(crate) fn head_key(topics: &Topics, stream: &Stream) -> String {
    stream_key(topics.project(), stream)
}

/// [`head_key`] from the project id alone.
pub(crate) fn stream_key(project: &ProjectId, stream: &Stream) -> String {
    format!("{project}/{stream}")
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
            StoreError::TooLarge => NackReason::Schema.into(),
            other => Self::internal(error_chain(&other)),
        }
    }
}

/// Why storing or reading messages failed.
#[derive(Debug, thiserror::Error)]
enum StoreError {
    #[error("the stamped envelope exceeds 81920 bytes without refs")]
    TooLarge,
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("numbering failed")]
    Numbering(#[from] NumberingError),
    #[error("a stored value is invalid: {0}")]
    Stored(String),
    #[error(transparent)]
    Backend(Box<dyn std::error::Error + Send + Sync>),
    /// Another copy of the same `(from, id)` committed first.
    #[error("the message was already recorded")]
    Duplicate,
}

impl From<agmen_core::StoreError> for StoreError {
    fn from(err: agmen_core::StoreError) -> Self {
        match err {
            agmen_core::StoreError::Duplicate => Self::Duplicate,
            agmen_core::StoreError::Stored(message)
                if message == "the stamped envelope exceeds 81920 bytes without refs" =>
            {
                Self::TooLarge
            }
            agmen_core::StoreError::Stored(message) => Self::Stored(
                message
                    .strip_prefix("a stored value is invalid: ")
                    .unwrap_or(&message)
                    .to_owned(),
            ),
            agmen_core::StoreError::Backend(source) => match source.downcast::<sqlx::Error>() {
                Ok(err) => Self::Database(*err),
                Err(source) => match source.downcast::<NumberingError>() {
                    Ok(err) => Self::Numbering(*err),
                    Err(source) => Self::Backend(source),
                },
            },
            other => Self::Backend(Box::new(other)),
        }
    }
}

fn stored(what: impl fmt::Display) -> StoreError {
    StoreError::Stored(what.to_string())
}

fn parse_id(id: &str) -> Result<IdentityId, StoreError> {
    id.parse().map_err(stored)
}

/// Whether `topics` names one of this hub's projects. Checked before any
/// reply, so hubs that share a broker (tests run many) answer only for
/// their own projects.
pub(crate) async fn project_exists(pool: &PgPool, topics: &Topics) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1 AND tenant_id = $2)")
        .bind(topics.project().as_str())
        .bind(topics.tenant().as_str())
        .fetch_one(pool)
        .await
}

/// The live identity `from` in `topics`' project, or a human who is a
/// member of it. `None` for a revoked identity and for anyone else.
type SenderRow = (String, String, String, bool, Option<Vec<String>>);

async fn load_sender(
    pool: &PgPool,
    topics: &Topics,
    from: &IdentityId,
) -> Result<Option<Sender>, StoreError> {
    let identity: Option<SenderRow> = sqlx::query_as(
        "SELECT kind, handle, owner_human_id, revoked_at IS NOT NULL, verbs
         FROM identities WHERE id = $1 AND tenant_id = $2 AND project_id = $3",
    )
    .bind(from.as_str())
    .bind(topics.tenant().as_str())
    .bind(topics.project().as_str())
    .fetch_optional(pool)
    .await?;
    if let Some((kind, handle, owner, revoked, verbs)) = identity {
        if revoked {
            return Ok(None);
        }
        return Ok(Some(Sender {
            id: from.clone(),
            kind: parse_kind(&kind).map_err(stored)?,
            handle,
            owner: Some(parse_id(&owner)?),
            verbs,
        }));
    }
    let human: Option<String> = sqlx::query_scalar(
        "SELECT h.handle FROM humans h
         JOIN memberships m ON m.human_id = h.id
         JOIN projects p ON p.id = m.project_id
         WHERE h.id = $1 AND p.tenant_id = $2 AND p.id = $3",
    )
    .bind(from.as_str())
    .bind(topics.tenant().as_str())
    .bind(topics.project().as_str())
    .fetch_optional(pool)
    .await?;
    Ok(human.map(|handle| Sender {
        id: from.clone(),
        kind: IdentityKind::Human,
        handle,
        owner: None,
        verbs: None,
    }))
}

/// A publish that passed the checks that need no roster.
#[derive(Debug)]
struct Admitted {
    env: ClientEnvelope,
    verb: Verb,
    sender: Sender,
    hash: Vec<u8>,
}

/// The message `reply_to` names in a stream `sender` reads: the feed or
/// its own inbox. Every party to a DM has a copy in their own inbox, so
/// that covers every message the sender has seen. If two senders reused
/// one id, the earliest copy wins.
async fn find_parent(
    pool: &PgPool,
    topics: &Topics,
    sender: &IdentityId,
    reply_to: &MessageId,
) -> Result<Option<Parent>, StoreError> {
    let readable = [
        Stream::Feed.to_string(),
        Stream::Inbox(sender.clone()).to_string(),
    ];
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT from_id, envelope->>'from_kind', envelope->>'verb' FROM messages
         WHERE project_id = $1 AND id = $2 AND stream = ANY($3)
         ORDER BY hub_at, stream, seq LIMIT 1",
    )
    .bind(topics.project().as_str())
    .bind(reply_to.as_str())
    .bind(&readable[..])
    .fetch_optional(pool)
    .await?;
    let Some((from, kind, verb)) = row else {
        return Ok(None);
    };
    let streams: Vec<String> = sqlx::query_scalar(
        "SELECT stream FROM messages WHERE project_id = $1 AND from_id = $2 AND id = $3",
    )
    .bind(topics.project().as_str())
    .bind(&from)
    .bind(reply_to.as_str())
    .fetch_all(pool)
    .await?;
    Ok(Some(Parent {
        from: parse_id(&from)?,
        from_kind: parse_kind(&kind).map_err(stored)?,
        verb,
        streams: streams
            .iter()
            .map(|stream| stream.parse().map_err(stored))
            .collect::<Result<_, _>>()?,
    }))
}

/// The ACK a resend of `(from, id)` gets, or `conflict` when its hash
/// differs from the recorded one (ruling R33a); `None` when it is new.
async fn recorded_reply(
    pool: &PgPool,
    from: &IdentityId,
    id: &MessageId,
    hash: &[u8],
) -> Result<Option<DeliveryReply>, StoreError> {
    let row: Option<(String, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT stream, seq, payload_hash FROM messages
         WHERE from_id = $1 AND id = $2 AND ack_row",
    )
    .bind(from.as_str())
    .bind(id.as_str())
    .fetch_optional(pool)
    .await?;
    let Some((stream, seq, recorded_hash)) = row else {
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
        stream: stream.parse().map_err(stored)?,
        seq: Seq(u64::try_from(seq).map_err(stored)?),
    }))
}
