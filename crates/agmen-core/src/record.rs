//! Atomic message recording and the hub's own posts.

use std::collections::BTreeSet;

use agmen_proto::{
    Body, ClientEnvelope, DeliveryReply, IdentityId, IdentityKind, ItemKey, LaneName, MessageId,
    Payload, ProjectId, Ref, Seq, StampedEnvelope, Stream, Timestamp, Topics, Verb,
};

use crate::{
    Core, MessageRow, Reads, Store, StoreError, StoreTx, ack_stream, candidate_item_keys,
    envelope_hash, hub_identity, hub_now, new_message_id, stamp_refs,
};

/// The hub's handle (protocol section 1).
const HUB_HANDLE: &str = "hub";

/// Leaves 16 KiB for topic and MQTT framing below the client's 96 KiB cap.
const MAX_STAMPED_ENVELOPE_BYTES: usize = 80 * 1024;

/// The verbs the hub posts as numbered messages (ruling R34). ACK and NACK
/// are delivery replies, and STATE goes to retained `state/` topics
/// (protocol section 4), so none of them is stored.
const HUB_VERBS: [Verb; 6] = [
    Verb::Grant,
    Verb::Deny,
    Verb::Item,
    Verb::Alert,
    Verb::Release,
    Verb::Status,
];

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

/// A message the hub posts as itself: `from` is [`crate::HUB_IDENTITY`],
/// `from_kind` is `hub`, and it takes the same numbering and sweeper as a
/// client's message, without the rate limit (ruling R34).
#[derive(Debug, Clone, PartialEq)]
pub struct HubPost {
    /// The verb and its data; one of GRANT, DENY, ITEM, ALERT, RELEASE
    /// and STATUS.
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

impl<S: Store> Core<S> {
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
    /// # async fn example<S: agmen_core::Store>(core: agmen_core::Core<S>) -> Result<(), Box<dyn std::error::Error>> {
    /// use agmen_core::HubPost;
    /// use agmen_proto::{Payload, Stream, Topics, Urgency, Needs};
    /// let topics = Topics::new("tenant01".parse()?, "project1".parse()?);
    /// let posted = core
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
        let mut tx = self.store().begin().await.map_err(PostError::store)?;
        let posted = post_hub(&mut tx, topics, post, None).await?;
        tx.commit().await.map_err(PostError::store)?;
        Ok(posted)
    }
}

/// [`Core::post_as_hub`] inside `tx`, which the caller commits.
///
/// # Errors
///
/// Returns [`PostError`] for an invalid post or a storage failure.
pub async fn post_in(
    tx: &mut impl StoreTx,
    topics: &Topics,
    post: HubPost,
) -> Result<Posted, PostError> {
    post_hub(tx, topics, post, None).await
}

/// [`post_in`] for an ALERT meant for the coordinators: when the project
/// has none, it mentions `fallback`, the owning human of the identity
/// concerned, instead (ruling R35).
///
/// # Errors
///
/// Returns [`PostError`] for an invalid post or a storage failure.
pub async fn post_alert(
    tx: &mut impl StoreTx,
    topics: &Topics,
    post: HubPost,
    fallback: &IdentityId,
) -> Result<Posted, PostError> {
    post_hub(tx, topics, post, Some(fallback)).await
}

async fn post_hub(
    tx: &mut impl StoreTx,
    topics: &Topics,
    post: HubPost,
    fallback: Option<&IdentityId>,
) -> Result<Posted, PostError> {
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
    let id = new_message_id(hub_at).map_err(PostError::Random)?;
    let mut mentioned: BTreeSet<IdentityId> = post.mentioned.into_iter().collect();
    let mut refs = Vec::new();
    if verb == Verb::Alert {
        let roster = tx
            .roster(topics.project())
            .await
            .map_err(database_error)
            .map_err(PostError::store)?;
        let mut coordinators = roster.coordinators().cloned().peekable();
        if coordinators.peek().is_none() {
            mentioned.extend(fallback.cloned());
        }
        mentioned.extend(coordinators);
        refs = load_refs(tx, topics, post.body.as_str(), &roster)
            .await
            .map_err(PostError::store)?;
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
    let hash = envelope_hash(&client).map_err(|err| PostError::Store(Box::new(err)))?;
    let draft = Draft {
        topics,
        client,
        from: hub_identity(),
        from_handle: HUB_HANDLE.to_owned(),
        from_kind: IdentityKind::Hub,
        mentioned: mentioned.into_iter().collect(),
        refs,
        streams: post.streams.into_iter().collect(),
        hash,
        hub_at,
        ack: AckCopy::None,
    };
    let recorded = write(tx, draft).await.map_err(PostError::store)?;
    Ok(Posted {
        id,
        copies: recorded.copies,
    })
}

/// The `stream_heads` key of one project's stream: `<project_id>/<stream>`.
fn head_key(topics: &Topics, stream: &Stream) -> String {
    stream_key(topics.project(), stream)
}

/// [`head_key`] from the project id alone.
fn stream_key(project: &ProjectId, stream: &Stream) -> String {
    format!("{project}/{stream}")
}

/// The broker topic a copy in `stream` is published on.
fn stream_topic(topics: &Topics, stream: &Stream) -> String {
    match stream {
        Stream::Feed => topics.feed(),
        Stream::Inbox(id) => topics.inbox(id),
    }
}

// shortcut: public bridge for hub accept; make crate-private when Task 7 moves ingest.
/// Whether the store queues an ACK on `ack/{from}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum AckCopy {
    /// A client's message: its ACK follows its copies.
    Queue,
    /// The hub's own message: nobody waits for an ACK.
    None,
}

/// A message ready to be numbered and stored.
#[derive(Debug)]
#[doc(hidden)]
pub struct Draft<'a> {
    pub topics: &'a Topics,
    pub client: ClientEnvelope,
    pub from: IdentityId,
    pub from_handle: String,
    pub from_kind: IdentityKind,
    pub mentioned: Vec<IdentityId>,
    pub refs: Vec<Ref>,
    pub streams: BTreeSet<Stream>,
    pub hash: Vec<u8>,
    pub hub_at: Timestamp,
    pub ack: AckCopy,
}

/// Where a message was recorded.
#[derive(Debug)]
#[doc(hidden)]
pub struct Recorded {
    /// The copy the ACK reports.
    pub ack: (Stream, Seq),
    pub copies: Vec<(Stream, Seq)>,
}

/// Numbers and stores every copy of `draft` in `tx`, with one outbox row
/// per copy and the ACK's outbox row; the caller commits. Streams are
/// numbered in `BTreeSet` order, so two transactions lock `stream_heads`
/// rows in the same order and cannot deadlock.
#[doc(hidden)]
pub async fn write(tx: &mut impl StoreTx, draft: Draft<'_>) -> Result<Recorded, StoreError> {
    let ack_at = ack_stream(&draft.streams, &draft.from).ok_or_else(|| {
        StoreError::Stored("a stored value is invalid: a message with no stream".into())
    })?;
    let mut copies = Vec::with_capacity(draft.streams.len());
    for stream in &draft.streams {
        let seq = tx
            .next_seq(&head_key(draft.topics, stream))
            .await
            .map_err(|err| backend_context(err, "numbering failed"))?;
        let mut stamped = StampedEnvelope {
            client: draft.client.clone(),
            stream: stream.clone(),
            seq,
            from: draft.from.clone(),
            from_handle: draft.from_handle.clone(),
            from_kind: draft.from_kind,
            mentioned: draft.mentioned.clone(),
            refs: draft.refs.clone(),
            hub_at: draft.hub_at,
        };
        let mut json = serde_json::to_string(&stamped).map_err(encoding_error)?;
        if json.len() > MAX_STAMPED_ENVELOPE_BYTES {
            stamped.refs.clear();
            json = serde_json::to_string(&stamped).map_err(encoding_error)?;
            if json.len() > MAX_STAMPED_ENVELOPE_BYTES {
                return Err(StoreError::Stored(TOO_LARGE.to_owned()));
            }
        }
        tx.insert_message(&MessageRow {
            project: draft.topics.project(),
            stream,
            seq,
            id: &draft.client.id,
            from: &draft.from,
            payload_hash: &draft.hash,
            envelope_json: &json,
            hub_at: draft.hub_at,
            ack_row: *stream == ack_at,
        })
        .await
        .map_err(database_error)?;
        tx.enqueue_publish(
            draft.topics,
            &stream_topic(draft.topics, stream),
            json.as_bytes(),
        )
        .await
        .map_err(database_error)?;
        copies.push((stream.clone(), seq));
    }
    let ack = copies
        .iter()
        .find(|(stream, _)| *stream == ack_at)
        .cloned()
        .ok_or_else(|| {
            StoreError::Stored("a stored value is invalid: the ACK copy was not stored".into())
        })?;
    if draft.ack == AckCopy::Queue {
        let reply = DeliveryReply::Ack {
            id: draft.client.id.clone(),
            stream: ack.0.clone(),
            seq: ack.1,
        };
        let topic = draft.topics.ack(&draft.from);
        tx.enqueue_publish(
            draft.topics,
            &topic,
            &serde_json::to_vec(&reply).map_err(encoding_error)?,
        )
        .await
        .map_err(database_error)?;
    }
    Ok(Recorded { ack, copies })
}

pub(crate) const TOO_LARGE: &str = "the stamped envelope exceeds 81920 bytes without refs";

#[derive(Debug, thiserror::Error)]
#[error("{context}")]
struct RecordError {
    context: &'static str,
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
}

fn encoding_error(err: serde_json::Error) -> StoreError {
    StoreError::Backend(Box::new(RecordError {
        context: "encoding a message failed",
        source: Box::new(err),
    }))
}

fn backend_context(err: StoreError, context: &'static str) -> StoreError {
    match err {
        StoreError::Backend(source) => {
            StoreError::Backend(Box::new(RecordError { context, source }))
        }
        other => other,
    }
}

fn database_error(err: StoreError) -> StoreError {
    backend_context(err, "database error")
}

impl PostError {
    fn store(err: StoreError) -> Self {
        match err {
            StoreError::Backend(source) => Self::Store(source),
            StoreError::Duplicate => Self::Store(Box::new(std::io::Error::other(
                "the message was already recorded",
            ))),
            other => Self::Store(Box::new(other)),
        }
    }
}

pub(crate) async fn load_refs(
    tx: &mut impl Reads,
    topics: &Topics,
    body: &str,
    roster: &crate::Roster,
) -> Result<Vec<Ref>, StoreError> {
    let candidates = candidate_item_keys(body);
    let (lanes, items) = tx
        .ref_targets(topics.project(), &candidates)
        .await
        .map_err(database_error)?;
    Ok(stamp_refs(body, roster, &lanes, &items))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use agmen_proto::{
        IdentityId, IdentityKind, Needs, Payload, Seq, StampedEnvelope, Stream, Topics, Urgency,
    };

    use super::*;
    use crate::{Core, MemStore, Store, StoreError, StoreTx};

    fn setup() -> (Core<MemStore>, MemStore, Topics) {
        let store = MemStore::default();
        let core = Core::new(store.clone(), Duration::from_secs(60));
        let topics = Topics::new("tenant01".parse().unwrap(), "project01".parse().unwrap());
        (core, store, topics)
    }

    fn status(streams: Vec<Stream>) -> HubPost {
        HubPost {
            payload: Payload::Status {
                summary: "ready".into(),
                items: Vec::new(),
                spend_usd: None,
                hours: None,
                tokens: None,
            },
            body: "ready".try_into().unwrap(),
            reply_to: None,
            items: Vec::new(),
            lane: None,
            mentioned: Vec::new(),
            streams,
        }
    }

    fn alert() -> HubPost {
        HubPost {
            payload: Payload::Alert {
                need: "attention".into(),
                urgency: Urgency::Normal,
                needs: Needs::Anyone,
                actions: Vec::new(),
            },
            ..status(vec![Stream::Feed])
        }
    }

    #[tokio::test]
    async fn hub_post_numbers_each_stream() {
        let (core, store, topics) = setup();
        let inbox = Stream::Inbox("aaaaaaaa".parse().unwrap());
        for number in 1..=2 {
            let posted = core
                .post_as_hub(
                    &topics,
                    status(vec![inbox.clone(), Stream::Feed, Stream::Feed]),
                )
                .await
                .unwrap();
            assert_eq!(
                posted.copies,
                vec![(Stream::Feed, Seq(number)), (inbox.clone(), Seq(number))]
            );
            let rows = store.recorded_messages().await.unwrap();
            let outbox = store.queued_publishes().await.unwrap();
            assert_eq!(rows.len(), number as usize * 2);
            assert_eq!(outbox.len(), number as usize * 2);
            for ((topic, payload), row) in outbox.iter().zip(&rows) {
                assert_eq!(row.from, crate::hub_identity());
                assert_eq!(row.from_handle, "hub");
                assert_eq!(row.from_kind, IdentityKind::Hub);
                assert_eq!(
                    serde_json::from_slice::<StampedEnvelope>(payload).unwrap(),
                    *row
                );
                assert_eq!(topic, &stream_topic(&topics, &row.stream));
            }
        }
    }

    #[tokio::test]
    async fn alert_mentions_coordinators_or_fallback() {
        let fallback: IdentityId = "aaaaaaaa".parse().unwrap();
        let coordinator: IdentityId = "cccccccc".parse().unwrap();
        for with_coordinator in [true, false] {
            let (_core, store, topics) = setup();
            if with_coordinator {
                store
                    .seed_live_identity(
                        topics.project(),
                        &coordinator,
                        "c",
                        IdentityKind::Coordinator,
                    )
                    .await
                    .unwrap();
            }
            let mut post = alert();
            post.body = "@c needs attention".try_into().unwrap();
            let mut tx = store.begin().await.unwrap();
            post_alert(&mut tx, &topics, post, &fallback).await.unwrap();
            tx.commit().await.unwrap();
            let rows = store.recorded_messages().await.unwrap();
            assert_eq!(
                rows[0].mentioned,
                if with_coordinator {
                    vec![coordinator.clone()]
                } else {
                    vec![fallback.clone()]
                }
            );
            assert_eq!(rows[0].refs.len(), usize::from(with_coordinator));
            if with_coordinator {
                assert_eq!(rows[0].refs[0].target, coordinator.as_str());
            }
        }
    }

    #[tokio::test]
    async fn alert_mentions_human_coordinators_and_deduplicates() {
        let (core, store, topics) = setup();
        let coordinator: IdentityId = "cccccccc".parse().unwrap();
        for (id, role) in [
            (coordinator.clone(), crate::MemberRole::Coordinator),
            ("aaaaaaaa".parse().unwrap(), crate::MemberRole::Admin),
        ] {
            store
                .seed_member(
                    topics.project(),
                    crate::Member {
                        handle: id.to_string(),
                        id,
                        kind: IdentityKind::Human,
                        owner: None,
                        role: Some(role),
                        identity_role: None,
                    },
                )
                .await
                .unwrap();
        }
        let mut post = alert();
        post.mentioned = vec![coordinator.clone(), coordinator.clone()];
        core.post_as_hub(&topics, post).await.unwrap();
        assert_eq!(
            store.recorded_messages().await.unwrap()[0].mentioned,
            vec![coordinator]
        );
    }

    #[tokio::test]
    async fn oversized_refs_are_dropped_then_refused() {
        let (core, store, topics) = setup();
        let mut post = alert();
        // JSON escapes the control bytes; refs take this otherwise fitting
        // envelope past the 81,920-byte stamped limit.
        post.body = format!("{}{}", "\u{1}".repeat(13_200), "#1234567890 ".repeat(128))
            .try_into()
            .unwrap();
        let posted = core.post_as_hub(&topics, post).await.unwrap();
        let rows = store.recorded_messages().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].client.id, posted.id);
        assert!(rows[0].refs.is_empty());
        let mut with_refs = rows[0].clone();
        with_refs.refs = crate::stamp_refs(
            with_refs.client.body.as_str(),
            &crate::Roster::default(),
            &[],
            &[],
        );
        assert_eq!(with_refs.refs.len(), 128);
        assert!(serde_json::to_vec(&with_refs).unwrap().len() > 81_920);
        assert!(serde_json::to_vec(&rows[0]).unwrap().len() <= 81_920);

        let mut oversized = status(vec![Stream::Feed]);
        oversized.body = "\u{1}".repeat(14_000).try_into().unwrap();
        let err = core.post_as_hub(&topics, oversized).await.unwrap_err();
        let PostError::Store(source) = err else {
            panic!("wrong post error")
        };
        let stored = source.downcast_ref::<StoreError>().unwrap();
        assert!(matches!(stored, StoreError::Stored(_)));
        assert_eq!(
            stored.to_string(),
            "the stamped envelope exceeds 81920 bytes without refs"
        );
        assert_eq!(store.recorded_messages().await.unwrap(), rows);
        assert_eq!(store.queued_publishes().await.unwrap().len(), 1);
        assert_eq!(
            core.post_as_hub(&topics, status(vec![Stream::Feed]))
                .await
                .unwrap()
                .copies,
            vec![(Stream::Feed, Seq(2))]
        );
    }

    #[tokio::test]
    async fn non_hub_verbs_are_refused() {
        let (core, store, topics) = setup();
        let post = HubPost {
            payload: Payload::Fyi {},
            ..status(vec![Stream::Feed])
        };
        let err = core.post_as_hub(&topics, post).await.unwrap_err();
        assert!(matches!(&err, PostError::NotHubVerb(verb) if verb == "FYI"));
        assert_eq!(err.to_string(), "the hub does not send FYI");
        let err = core
            .post_as_hub(&topics, status(Vec::new()))
            .await
            .unwrap_err();
        assert!(matches!(err, PostError::NoStreams));
        assert_eq!(err.to_string(), "a hub post needs at least one stream");
        assert!(store.recorded_messages().await.unwrap().is_empty());
        assert!(store.queued_publishes().await.unwrap().is_empty());
    }

    fn client_draft(topics: &Topics) -> Draft<'_> {
        let from: IdentityId = "aaaaaaaa".parse().unwrap();
        let client = ClientEnvelope::from_json(br#"{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"FYI","data":{},"body":"hello","sent_at":"2026-10-25T16:04:11Z"}"#).unwrap();
        Draft {
            topics,
            hash: crate::envelope_hash(&client).unwrap(),
            hub_at: client.sent_at,
            client,
            from: from.clone(),
            from_handle: "a".into(),
            from_kind: IdentityKind::Agent,
            mentioned: Vec::new(),
            refs: Vec::new(),
            streams: [Stream::Inbox(from), Stream::Feed].into_iter().collect(),
            ack: AckCopy::Queue,
        }
    }

    #[tokio::test]
    async fn client_ack_follows_copies_and_resend_is_duplicate() {
        let (_core, store, topics) = setup();
        let draft = client_draft(&topics);
        let id = draft.client.id.clone();
        let from = draft.from.clone();
        let mut tx = store.begin().await.unwrap();
        let recorded = write(&mut tx, draft).await.unwrap();
        assert_eq!(recorded.ack, (Stream::Feed, Seq(1)));
        tx.commit().await.unwrap();
        let rows = store.recorded_messages().await.unwrap();
        assert_eq!(rows.len(), 2);
        let outbox = store.queued_publishes().await.unwrap();
        assert_eq!(outbox.len(), 3);
        assert_eq!(outbox[0].0, topics.feed());
        assert_eq!(outbox[1].0, topics.inbox(&from));
        assert_eq!(outbox[2].0, topics.ack(&from));
        assert_eq!(
            serde_json::from_slice::<DeliveryReply>(&outbox[2].1).unwrap(),
            DeliveryReply::Ack {
                id,
                stream: Stream::Feed,
                seq: Seq(1)
            }
        );
        let mut tx = store.begin().await.unwrap();
        assert!(matches!(
            write(&mut tx, client_draft(&topics)).await,
            Err(StoreError::Duplicate)
        ));
        drop(tx);
        assert_eq!(store.recorded_messages().await.unwrap(), rows);
        assert_eq!(store.queued_publishes().await.unwrap(), outbox);
        assert_eq!(
            store
                .begin()
                .await
                .unwrap()
                .next_seq(&head_key(&topics, &Stream::Feed))
                .await
                .unwrap(),
            Seq(2)
        );
    }

    #[tokio::test]
    async fn failed_recording_rolls_back_messages_outbox_and_numbers() {
        for fault in [
            "begin",
            "roster",
            "ref_targets",
            "next_seq",
            "insert_message",
            "enqueue_publish",
            "commit",
        ] {
            let (core, store, topics) = setup();
            store.fail_next(fault);
            assert!(
                matches!(
                    core.post_as_hub(&topics, alert()).await,
                    Err(PostError::Store(_))
                ),
                "{fault}"
            );
            assert!(
                store.recorded_messages().await.unwrap().is_empty(),
                "{fault}"
            );
            assert!(
                store.queued_publishes().await.unwrap().is_empty(),
                "{fault}"
            );
            assert_eq!(
                core.post_as_hub(&topics, alert()).await.unwrap().copies,
                vec![(Stream::Feed, Seq(1))],
                "{fault}"
            );
        }
    }

    #[tokio::test]
    async fn record_error_contexts_match_the_hub() {
        for (fault, expected) in [
            ("begin", "injected begin failure"),
            ("commit", "injected commit failure"),
            ("roster", "database error"),
            ("ref_targets", "database error"),
            ("next_seq", "numbering failed"),
            ("insert_message", "database error"),
            ("enqueue_publish", "database error"),
        ] {
            let (core, store, topics) = setup();
            store.fail_next(fault);
            let PostError::Store(source) = core.post_as_hub(&topics, alert()).await.unwrap_err()
            else {
                panic!("wrong post error for {fault}")
            };
            assert_eq!(source.to_string(), expected, "{fault}");
            if !matches!(fault, "begin" | "commit") {
                assert_eq!(
                    source.source().unwrap().to_string(),
                    format!("injected {fault} failure")
                );
            }
        }
    }

    #[tokio::test]
    async fn refs_use_known_lanes_items_and_live_roster() {
        let (core, store, topics) = setup();
        store
            .seed_lane(topics.project(), "L-UI", &["ui/**"])
            .await
            .unwrap();
        store
            .seed_item(topics.project(), &"B-1".parse().unwrap())
            .await
            .unwrap();
        store
            .seed_live_identity(
                topics.project(),
                &"aaaaaaaa".parse().unwrap(),
                "a",
                IdentityKind::Agent,
            )
            .await
            .unwrap();
        let mut post = alert();
        post.body = "@a L-UI B-1 B-2 #123".try_into().unwrap();
        core.post_as_hub(&topics, post).await.unwrap();
        let rows = store.recorded_messages().await.unwrap();
        assert_eq!(
            rows[0]
                .refs
                .iter()
                .map(|r| r.target.as_str())
                .collect::<Vec<_>>(),
            vec!["aaaaaaaa", "L-UI", "B-1", "123"]
        );
    }
}
