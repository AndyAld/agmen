//! Ingest: what the hub does with a client's publish on `up/{identity_id}`
//! (protocol 5.2), and the path the hub posts its own messages on (ruling
//! R34).
//!
//! An accepted message is numbered and stored in one transaction, together
//! with a `publish_outbox` row for each copy and one for the ACK; the
//! sweeper publishes the outbox. A refused message gets a NACK and leaves
//! nothing behind.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use agmen_proto::{
    Body, ClientEnvelope, DeliveryReply, IdentityId, IdentityKind, ItemKey, LaneName, Mention,
    MessageId, NackReason, Payload, Seq, StampedEnvelope, Stream, Timestamp, Topics, Verb,
    validate,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::Notify;

use crate::db::{Role, is_unique_violation};
use crate::http::{Hub, error_chain};
use crate::numbering::{NumberingError, next_seq};
use crate::secrets_scan::find_secret;

/// The hub's own identity id, stamped as `from` on what it posts. It holds a
/// `0`, which the id alphabet the hub mints from leaves out, so no minted
/// human or identity can ever have it.
pub const HUB_IDENTITY: &str = "agmenhub0";

/// The hub's handle (protocol section 1).
const HUB_HANDLE: &str = "hub";

/// Publishes per second each sender may sustain (protocol section 8).
const RATE_PER_SEC: f64 = 10.0;
/// Publishes a sender may make at once after a quiet spell.
const BURST: f64 = 30.0;
/// The most senders the rate limiter tracks. Past it, senders whose bucket
/// has refilled are forgotten; if every tracked sender is still busy, new
/// senders get `hub_busy` until one calms down.
const MAX_TRACKED: usize = 10_000;
/// What a `hub_busy` NACK tells the client to wait.
const BUSY_RETRY_MS: u64 = 1_000;

/// The verbs the hub sends itself (ruling R34). ACK and NACK are delivery
/// replies, not stored messages.
const HUB_VERBS: [Verb; 7] = [
    Verb::State,
    Verb::Grant,
    Verb::Deny,
    Verb::Item,
    Verb::Alert,
    Verb::Release,
    Verb::Status,
];

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

/// Why the hub could not post as itself.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PostError {
    /// The hub does not send this verb (ruling R34).
    #[error("the hub does not send {0}")]
    NotHubVerb(String),
    /// The post names no stream to go to.
    #[error("a hub post needs at least one stream")]
    NoStreams,
    /// The operating system's random source failed while minting the id.
    #[error("minting a message id failed")]
    Random(#[source] getrandom::Error),
    /// Numbering or storing the message failed.
    #[error("storing the hub's message failed")]
    Store(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// A message the hub posts as itself: `from` is [`HUB_IDENTITY`],
/// `from_kind` is `hub`, and it takes the same numbering and sweeper as a
/// client's message, without the rate limit (ruling R34).
#[derive(Debug, Clone, PartialEq)]
pub struct HubPost {
    /// The verb and its data; one of STATE, GRANT, DENY, ITEM, ALERT,
    /// RELEASE and STATUS.
    pub payload: Payload,
    /// The text.
    pub body: Body,
    /// The message this one answers.
    pub reply_to: Option<MessageId>,
    /// Items it is about.
    pub items: Vec<ItemKey>,
    /// The lane it is about.
    pub lane: Option<LaneName>,
    /// Who it interrupts. An ALERT also gets the project's coordinators
    /// (ruling R35).
    pub mentioned: Vec<IdentityId>,
    /// Where it goes: the feed, inboxes, or both.
    pub streams: Vec<Stream>,
}

/// Where a hub post was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Posted {
    /// The id the hub gave the message.
    pub id: MessageId,
    /// Each copy's stream and seq.
    pub copies: Vec<(Stream, Seq)>,
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

/// Ingest's shared state: the rate limiter and the sweeper's wake-up.
#[derive(Debug, Default)]
pub(crate) struct IngestState {
    limiter: Mutex<RateLimiter>,
    outbox_ready: Notify,
}

impl IngestState {
    /// Signalled after each commit that added outbox rows.
    pub(crate) fn outbox_ready(&self) -> &Notify {
        &self.outbox_ready
    }
}

impl Hub {
    /// Handles one publish from `from` on `topics`' `up/` topic, or from a
    /// human posting to that project, and returns the ACK or NACK
    /// (protocol 5.2): parse and validate against the hub's clock, rate
    /// limit (10 per second, burst 30, per sender), check that the sender
    /// is a live member of the project and may send the verb, scan for
    /// credentials, dedupe on `(from, id)`, expand mentions, then number
    /// and store every copy with its outbox rows. A fresh ACK is also
    /// queued for `ack/{from}` behind the message itself.
    ///
    /// Storage failures answer `hub_busy`, which the client retries.
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
        let roster = if needs_roster(verb, &env) {
            Roster::load(pool, topics).await?
        } else {
            Roster::default()
        };
        let parent = match &env.reply_to {
            Some(reply_to) => find_parent(pool, topics, &sender.id, reply_to).await?,
            None => None,
        };
        let mentioned = mentioned(verb, &env, &sender, &roster, parent.as_ref())?;
        let streams = route(verb, &env, &sender, &roster, parent.as_ref())?;
        let id = env.id.clone();
        let draft = Draft {
            topics,
            client: env,
            from: sender.id.clone(),
            from_handle: sender.handle,
            from_kind: sender.kind,
            mentioned,
            streams,
            hash: hash.clone(),
            hub_at,
            ack: AckCopy::Queue,
        };
        match store(pool, draft).await {
            Ok(recorded) => {
                self.ingest_state().outbox_ready().notify_one();
                let (stream, seq) = recorded.ack;
                Ok(Handled {
                    reply: DeliveryReply::Ack { id, stream, seq },
                    route: ReplyRoute::Outbox,
                })
            }
            // A copy of the same message committed first: answer as for a
            // resend.
            Err(StoreError::Duplicate) => match recorded_reply(pool, &sender.id, &id, &hash).await?
            {
                Some(reply) => Ok(Handled {
                    reply,
                    route: ReplyRoute::Caller,
                }),
                None => Err(Refused::internal("duplicate row vanished")),
            },
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
        validate(&env, hub_at)?;
        let verb = env.payload.verb().ok_or(NackReason::Schema)?;
        let sender = load_sender(self.pool(), topics, from)
            .await?
            .ok_or(NackReason::IdentityRevoked)?;
        if !may_send(sender.kind, verb) {
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
        let verb = match &post.payload {
            Payload::Unknown { verb, .. } => return Err(PostError::NotHubVerb(verb.clone())),
            payload => payload.verb(),
        };
        let Some(verb) = verb.filter(|verb| HUB_VERBS.contains(verb)) else {
            let name = post.payload.verb().map_or("?", Verb::as_str);
            return Err(PostError::NotHubVerb(name.to_owned()));
        };
        if post.streams.is_empty() {
            return Err(PostError::NoStreams);
        }
        let hub_at = hub_now();
        let id = new_message_id(hub_at)?;
        let mut mentioned: BTreeSet<IdentityId> = post.mentioned.into_iter().collect();
        if verb == Verb::Alert {
            let roster = Roster::load(self.pool(), topics)
                .await
                .map_err(PostError::store)?;
            mentioned.extend(roster.coordinators().cloned());
        }
        let client = ClientEnvelope {
            v: 1,
            id: id.clone(),
            reply_to: post.reply_to,
            to: Vec::new(),
            mentions: Vec::new(),
            items: post.items,
            lane: post.lane,
            body: post.body,
            valid_until: None,
            sent_at: hub_at,
            payload: post.payload,
        };
        let hash = envelope_hash(&client).map_err(PostError::store)?;
        let draft = Draft {
            topics,
            client,
            from: hub_identity(),
            from_handle: HUB_HANDLE.to_owned(),
            from_kind: IdentityKind::Hub,
            mentioned: mentioned.into_iter().collect(),
            streams: post.streams.into_iter().collect(),
            hash,
            hub_at,
            ack: AckCopy::None,
        };
        let recorded = store(self.pool(), draft).await.map_err(PostError::store)?;
        self.ingest_state().outbox_ready().notify_one();
        Ok(Posted {
            id,
            copies: recorded.copies,
        })
    }
}

impl PostError {
    fn store(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Store(Box::new(err))
    }
}

/// [`HUB_IDENTITY`] as an id.
#[expect(clippy::expect_used, reason = "a constant checked by a unit test")]
pub(crate) fn hub_identity() -> IdentityId {
    HUB_IDENTITY
        .parse()
        .expect("HUB_IDENTITY is a valid identity id")
}

/// The hub's clock, in whole milliseconds as the protocol shows `hub_at`.
fn hub_now() -> Timestamp {
    let now = Timestamp::now();
    Timestamp::from_millisecond(now.as_millisecond()).unwrap_or(now)
}

/// The `id` of a raw publish, read on its own so even a publish that fails
/// to parse can be NACKed.
fn message_id(raw: &[u8]) -> Option<MessageId> {
    #[derive(Deserialize)]
    struct IdOnly {
        id: MessageId,
    }
    serde_json::from_slice::<IdOnly>(raw)
        .ok()
        .map(|only| only.id)
}

/// SHA-256 of the envelope as the hub parsed it, so a resend that differs
/// only in key order or spacing still matches.
fn envelope_hash(env: &ClientEnvelope) -> Result<Vec<u8>, serde_json::Error> {
    Ok(Sha256::digest(serde_json::to_vec(env)?).to_vec())
}

/// The `stream_heads` key of one project's stream: `<project_id>/<stream>`.
pub(crate) fn head_key(topics: &Topics, stream: &Stream) -> String {
    format!("{}/{stream}", topics.project())
}

/// The broker topic a copy in `stream` is published on.
fn stream_topic(topics: &Topics, stream: &Stream) -> String {
    match stream {
        Stream::Feed => topics.feed(),
        Stream::Inbox(id) => topics.inbox(id),
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
        Self::internal(error_chain(&err))
    }
}

/// Why storing or reading messages failed.
#[derive(Debug, thiserror::Error)]
enum StoreError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("numbering failed")]
    Numbering(#[from] NumberingError),
    #[error("encoding a message failed")]
    Encode(#[from] serde_json::Error),
    #[error("a stored value is invalid: {0}")]
    Stored(String),
    /// Another copy of the same `(from, id)` committed first.
    #[error("the message was already recorded")]
    Duplicate,
}

fn stored(what: impl fmt::Display) -> StoreError {
    StoreError::Stored(what.to_string())
}

/// A kind as the tables and stamped envelopes spell it.
fn parse_kind(kind: &str) -> Result<IdentityKind, StoreError> {
    Ok(match kind {
        "human" => IdentityKind::Human,
        "agent" => IdentityKind::Agent,
        "coordinator" => IdentityKind::Coordinator,
        "listener" => IdentityKind::Listener,
        "hub" => IdentityKind::Hub,
        other => return Err(stored(format!("unknown kind {other:?}"))),
    })
}

fn parse_id(id: &str) -> Result<IdentityId, StoreError> {
    id.parse().map_err(stored)
}

/// Whether `topics` names one of this hub's projects. Checked before any
/// reply, so hubs that share a broker (tests run many) answer only for
/// their own projects.
async fn project_exists(pool: &PgPool, topics: &Topics) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1 AND tenant_id = $2)")
        .bind(topics.project().as_str())
        .bind(topics.tenant().as_str())
        .fetch_one(pool)
        .await
}

/// Who sent a publish.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sender {
    id: IdentityId,
    kind: IdentityKind,
    handle: String,
    /// The human who owns an identity; `None` for a human.
    owner: Option<IdentityId>,
}

/// The live identity `from` in `topics`' project, or a human who is a
/// member of it. `None` for a revoked identity and for anyone else.
async fn load_sender(
    pool: &PgPool,
    topics: &Topics,
    from: &IdentityId,
) -> Result<Option<Sender>, StoreError> {
    let identity: Option<(String, String, String, bool)> = sqlx::query_as(
        "SELECT kind, handle, owner_human_id, revoked_at IS NOT NULL
         FROM identities WHERE id = $1 AND tenant_id = $2 AND project_id = $3",
    )
    .bind(from.as_str())
    .bind(topics.tenant().as_str())
    .bind(topics.project().as_str())
    .fetch_optional(pool)
    .await?;
    if let Some((kind, handle, owner, revoked)) = identity {
        if revoked {
            return Ok(None);
        }
        return Ok(Some(Sender {
            id: from.clone(),
            kind: parse_kind(&kind)?,
            handle,
            owner: Some(parse_id(&owner)?),
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
    }))
}

/// Whether `kind` may send `verb`, from the "Sent by" column of protocol
/// section 4. Checks that need more than the kind (claim holder, lane
/// owner, the PUSH_REQUEST's owner) belong to the verbs' own handling.
///
/// - "coordinator" there means the `coordinator` kind. A listener with the
///   coordinator role (ASSIGN) comes with roles (roles-listener plan).
/// - REVIEW and PLAN need the reviewer and planner roles, which no identity
///   holds before the roles-listener plan, so nobody may send them yet.
/// - GRANT, DENY, ITEM and STATE are the hub's own (ruling R34).
const fn may_send(kind: IdentityKind, verb: Verb) -> bool {
    use IdentityKind::{Agent, Coordinator, Human, Listener};
    match verb {
        Verb::Hello
        | Verb::Bye
        | Verb::Fyi
        | Verb::Ask
        | Verb::Answer
        | Verb::Ack
        | Verb::Status
        | Verb::Alert
        | Verb::Dm
        | Verb::Found
        | Verb::Proposal => matches!(kind, Human | Agent | Coordinator | Listener),
        Verb::Rollcall | Verb::Decision | Verb::Ruling => matches!(kind, Human | Coordinator),
        Verb::Claim | Verb::Heartbeat | Verb::Release | Verb::Done => {
            matches!(kind, Agent | Coordinator)
        }
        Verb::Lend => matches!(kind, Human | Agent | Coordinator),
        Verb::Assign => matches!(kind, Coordinator),
        Verb::Delegated => matches!(kind, Listener),
        Verb::PushRequest => matches!(kind, Agent),
        Verb::PushOk | Verb::PushNo => matches!(kind, Human),
        Verb::Review | Verb::Plan | Verb::Grant | Verb::Deny | Verb::Item | Verb::State => false,
    }
}

/// One live member of a project: a non-revoked identity or a human
/// member.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    id: IdentityId,
    kind: IdentityKind,
    /// The owning human of an identity.
    owner: Option<IdentityId>,
    /// A human's membership role.
    role: Option<Role>,
}

/// A project's live members.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Roster(Vec<Member>);

type MemberRow = (String, String, Option<String>, Option<String>);

impl Roster {
    async fn load(pool: &PgPool, topics: &Topics) -> Result<Self, StoreError> {
        let rows: Vec<MemberRow> = sqlx::query_as(
            "SELECT id, kind, owner_human_id, NULL::text FROM identities
             WHERE project_id = $1 AND revoked_at IS NULL
             UNION ALL
             SELECT human_id, 'human', NULL, role FROM memberships WHERE project_id = $1",
        )
        .bind(topics.project().as_str())
        .fetch_all(pool)
        .await?;
        rows.into_iter()
            .map(|(id, kind, owner, role)| {
                Ok(Member {
                    id: parse_id(&id)?,
                    kind: parse_kind(&kind)?,
                    owner: owner.as_deref().map(parse_id).transpose()?,
                    role: role
                        .as_deref()
                        .map(str::parse::<Role>)
                        .transpose()
                        .map_err(stored)?,
                })
            })
            .collect::<Result<_, StoreError>>()
            .map(Self)
    }

    fn get(&self, id: &IdentityId) -> Option<&Member> {
        self.0.iter().find(|member| &member.id == id)
    }

    /// coordinators(project) of ruling R35: the live `coordinator`
    /// identities and the humans whose membership role is coordinator.
    /// Admins are not included until the user decides.
    fn coordinators(&self) -> impl Iterator<Item = &IdentityId> {
        self.0
            .iter()
            .filter(|member| {
                member.kind == IdentityKind::Coordinator
                    || (member.kind == IdentityKind::Human
                        && member.role == Some(Role::Coordinator))
            })
            .map(|member| &member.id)
    }
}

/// Whether handling `env` needs the project's members.
fn needs_roster(verb: Verb, env: &ClientEnvelope) -> bool {
    !env.mentions.is_empty() || !env.to.is_empty() || matches!(verb, Verb::Alert | Verb::Assign)
}

/// What `mentions` expand to (protocol section 3): `all`, `agents` and
/// `humans` expand to the project's live members of those kinds, an
/// identity must be a live member, and the sender is left out.
///
/// # Errors
///
/// `forbidden_mention` for anything but identities from a kind other than
/// human and coordinator, or an identity that is not a live member;
/// `schema` for lane and role mentions, which the MVP does not expand
/// (ruling R14).
fn expand_mentions(
    sender: &Sender,
    mentions: &[Mention],
    roster: &Roster,
) -> Result<BTreeSet<IdentityId>, NackReason> {
    let may_group = matches!(sender.kind, IdentityKind::Human | IdentityKind::Coordinator);
    let mut out = BTreeSet::new();
    for mention in mentions {
        let pick: fn(&Member) -> bool = match mention {
            Mention::Identity(id) => {
                if roster.get(id).is_none() {
                    return Err(NackReason::ForbiddenMention);
                }
                out.insert(id.clone());
                continue;
            }
            _ if !may_group => return Err(NackReason::ForbiddenMention),
            Mention::Lane(_) | Mention::Role(_) => return Err(NackReason::Schema),
            Mention::All => |_| true,
            Mention::Agents => {
                |member| !matches!(member.kind, IdentityKind::Human | IdentityKind::Hub)
            }
            Mention::Humans => |member| member.kind == IdentityKind::Human,
        };
        out.extend(
            roster
                .0
                .iter()
                .filter(|member| pick(member))
                .map(|member| member.id.clone()),
        );
    }
    out.remove(&sender.id);
    Ok(out)
}

/// A publish that passed the checks that need no roster.
#[derive(Debug)]
struct Admitted {
    env: ClientEnvelope,
    verb: Verb,
    sender: Sender,
    hash: Vec<u8>,
}

/// The `mentioned` a message is stamped with (protocol section 3): its
/// expanded mentions, the author of `reply_to` when a human, and for an
/// ALERT the sender's owning human and the coordinators (protocol 4
/// notifies them, and protocol 3 interrupts only through `mentioned`:
/// ruling R35). Never the sender. Sorted by id.
fn mentioned(
    verb: Verb,
    env: &ClientEnvelope,
    sender: &Sender,
    roster: &Roster,
    parent: Option<&Parent>,
) -> Result<Vec<IdentityId>, NackReason> {
    let mut mentioned = expand_mentions(sender, &env.mentions, roster)?;
    if let Some(parent) = parent
        && parent.from_kind == IdentityKind::Human
    {
        mentioned.insert(parent.from.clone());
    }
    if verb == Verb::Alert {
        mentioned.extend(sender.owner.iter().cloned());
        mentioned.extend(roster.coordinators().cloned());
    }
    mentioned.remove(&sender.id);
    Ok(mentioned.into_iter().collect())
}

/// The message a `reply_to` names, as the sender can see it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Parent {
    from: IdentityId,
    from_kind: IdentityKind,
    /// Every stream it was stored in.
    streams: BTreeSet<Stream>,
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
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT from_id, envelope->>'from_kind' FROM messages
         WHERE project_id = $1 AND id = $2 AND stream = ANY($3)
         ORDER BY hub_at, stream, seq LIMIT 1",
    )
    .bind(topics.project().as_str())
    .bind(reply_to.as_str())
    .bind(&readable[..])
    .fetch_optional(pool)
    .await?;
    let Some((from, kind)) = row else {
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
        from_kind: parse_kind(&kind)?,
        streams: streams
            .iter()
            .map(|stream| stream.parse().map_err(stored))
            .collect::<Result<_, _>>()?,
    }))
}

/// The streams a message goes to (protocol section 4):
///
/// - DM, and ASK with `to` (ruling R28): each recipient's inbox, the
///   sender's inbox and each owning human's inbox; never the feed.
/// - ANSWER, DELEGATED and REVIEW: the streams of the message they reply
///   to, so a reply in a private thread stays private; the feed when that
///   message was in the feed or is unknown.
/// - ASSIGN: the feed and the assignee's inbox.
/// - Everything else: the feed.
///
/// # Errors
///
/// `schema` when a recipient or assignee is not a live member.
fn route(
    verb: Verb,
    env: &ClientEnvelope,
    sender: &Sender,
    roster: &Roster,
    parent: Option<&Parent>,
) -> Result<BTreeSet<Stream>, NackReason> {
    let feed = || BTreeSet::from([Stream::Feed]);
    match verb {
        Verb::Dm => fan_out(&env.to, sender, roster),
        Verb::Ask if !env.to.is_empty() => fan_out(&env.to, sender, roster),
        Verb::Answer | Verb::Delegated | Verb::Review => Ok(match parent {
            Some(parent)
                if !parent.streams.is_empty() && !parent.streams.contains(&Stream::Feed) =>
            {
                let mut streams = parent.streams.clone();
                streams.insert(Stream::Inbox(sender.id.clone()));
                streams
            }
            _ => feed(),
        }),
        Verb::Assign => {
            let Payload::Assign { to, .. } = &env.payload else {
                return Err(NackReason::Schema);
            };
            if roster.get(to).is_none() {
                return Err(NackReason::Schema);
            }
            Ok(BTreeSet::from([Stream::Feed, Stream::Inbox(to.clone())]))
        }
        _ => Ok(feed()),
    }
}

/// The inboxes of `to`, of the sender, and of every owning human of them.
fn fan_out(
    to: &[IdentityId],
    sender: &Sender,
    roster: &Roster,
) -> Result<BTreeSet<Stream>, NackReason> {
    let mut streams = BTreeSet::from([Stream::Inbox(sender.id.clone())]);
    streams.extend(sender.owner.iter().cloned().map(Stream::Inbox));
    for recipient in to {
        let member = roster.get(recipient).ok_or(NackReason::Schema)?;
        streams.insert(Stream::Inbox(recipient.clone()));
        streams.extend(member.owner.iter().cloned().map(Stream::Inbox));
    }
    Ok(streams)
}

/// The copy an ACK reports (ruling R33a): the feed copy when there is one,
/// else the sender's own inbox copy, else the first.
fn ack_stream(streams: &BTreeSet<Stream>, from: &IdentityId) -> Option<Stream> {
    let own = Stream::Inbox(from.clone());
    if streams.contains(&Stream::Feed) {
        Some(Stream::Feed)
    } else if streams.contains(&own) {
        Some(own)
    } else {
        streams.first().cloned()
    }
}

/// Whether the store queues an ACK on `ack/{from}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AckCopy {
    /// A client's message: its ACK follows its copies.
    Queue,
    /// The hub's own message: nobody waits for an ACK.
    None,
}

/// A message ready to be numbered and stored.
#[derive(Debug)]
struct Draft<'a> {
    topics: &'a Topics,
    client: ClientEnvelope,
    from: IdentityId,
    from_handle: String,
    from_kind: IdentityKind,
    mentioned: Vec<IdentityId>,
    streams: BTreeSet<Stream>,
    hash: Vec<u8>,
    hub_at: Timestamp,
    ack: AckCopy,
}

/// Where a message was recorded.
#[derive(Debug)]
struct Recorded {
    /// The copy the ACK reports.
    ack: (Stream, Seq),
    copies: Vec<(Stream, Seq)>,
}

/// Numbers and stores every copy of `draft`, with one outbox row per copy
/// and the ACK's outbox row, in one transaction. Streams are numbered in
/// `BTreeSet` order, so two transactions lock `stream_heads` rows in the
/// same order and cannot deadlock.
async fn store(pool: &PgPool, draft: Draft<'_>) -> Result<Recorded, StoreError> {
    let ack_at = ack_stream(&draft.streams, &draft.from)
        .ok_or_else(|| stored("a message with no stream"))?;
    let project = draft.topics.project().as_str();
    let hub_at = draft.hub_at.to_string();
    let mut tx = pool.begin().await?;
    let mut copies = Vec::with_capacity(draft.streams.len());
    for stream in &draft.streams {
        let seq = next_seq(&mut tx, &head_key(draft.topics, stream)).await?;
        let seq = Seq(u64::try_from(seq).map_err(stored)?);
        let stamped = StampedEnvelope {
            client: draft.client.clone(),
            stream: stream.clone(),
            seq,
            from: draft.from.clone(),
            from_handle: draft.from_handle.clone(),
            from_kind: draft.from_kind,
            mentioned: draft.mentioned.clone(),
            refs: Vec::new(),
            hub_at: draft.hub_at,
        };
        let json = serde_json::to_string(&stamped)?;
        let inserted = sqlx::query(
            "INSERT INTO messages
                 (project_id, stream, seq, id, from_id, payload_hash, envelope, hub_at, ack_row)
             VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb, $8::timestamptz, $9)",
        )
        .bind(project)
        .bind(stream.to_string())
        .bind(i64::try_from(seq.0).map_err(stored)?)
        .bind(draft.client.id.as_str())
        .bind(draft.from.as_str())
        .bind(&draft.hash)
        .bind(&json)
        .bind(&hub_at)
        .bind(*stream == ack_at)
        .execute(&mut *tx)
        .await;
        match inserted {
            Ok(_) => {}
            Err(err) if is_unique_violation(&err) => return Err(StoreError::Duplicate),
            Err(err) => return Err(err.into()),
        }
        enqueue(
            &mut tx,
            draft.topics,
            &stream_topic(draft.topics, stream),
            json.as_bytes(),
        )
        .await?;
        copies.push((stream.clone(), seq));
    }
    let ack = copies
        .iter()
        .find(|(stream, _)| *stream == ack_at)
        .cloned()
        .ok_or_else(|| stored("the ACK copy was not stored"))?;
    if draft.ack == AckCopy::Queue {
        let reply = DeliveryReply::Ack {
            id: draft.client.id.clone(),
            stream: ack.0.clone(),
            seq: ack.1,
        };
        let topic = draft.topics.ack(&draft.from);
        enqueue(&mut tx, draft.topics, &topic, &serde_json::to_vec(&reply)?).await?;
    }
    tx.commit().await?;
    Ok(Recorded { ack, copies })
}

async fn enqueue(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    topics: &Topics,
    topic: &str,
    payload: &[u8],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO publish_outbox (tenant_id, project_id, topic, payload)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(topics.tenant().as_str())
    .bind(topics.project().as_str())
    .bind(topic)
    .bind(payload)
    .execute(&mut **tx)
    .await?;
    Ok(())
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

/// A fresh ULID (26 Crockford base32 digits: 48 bits of milliseconds, then
/// 80 random bits) for a message the hub posts.
fn new_message_id(now: Timestamp) -> Result<MessageId, PostError> {
    const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let millis = u128::try_from(now.as_millisecond()).unwrap_or(0) & ((1 << 48) - 1);
    let mut random = [0_u8; 10];
    getrandom::fill(&mut random).map_err(PostError::Random)?;
    let value = random
        .iter()
        .fold(millis, |acc, byte| (acc << 8) | u128::from(*byte));
    let text: String = (0..26_u32)
        .rev()
        .map(|digit| char::from(CROCKFORD[((value >> (5 * digit)) & 31) as usize]))
        .collect();
    text.parse().map_err(PostError::store)
}

/// A token bucket per sender: [`RATE_PER_SEC`] sustained, [`BURST`] at
/// once.
#[derive(Debug, Default)]
struct RateLimiter {
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
    fn take(&mut self, id: &IdentityId, now: Instant) -> Result<(), Option<Duration>> {
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
#[path = "ingest_tests.rs"]
mod tests;
