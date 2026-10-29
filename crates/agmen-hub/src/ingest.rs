//! Ingest: what the hub does with a client's publish on `up/{identity_id}`
//! (protocol 5.2), and the path the hub posts its own messages on (ruling
//! R34).
//!
//! An accepted message is numbered and stored in one transaction, together
//! with a `publish_outbox` row for each copy and one for the ACK, and for a
//! ledger verb the ledger's work and the hub's answer; the sweeper publishes
//! the outbox. A refused message gets a NACK and leaves nothing behind.

use agmen_proto::{DeliveryReply, IdentityId, ProjectId, Stream, Topics};
use sqlx::PgPool;
use tokio::sync::{Notify, watch};

use crate::events::Wakes;
use crate::http::{Hub, error_chain};
use crate::usage::{post_owner_alert, with_thousands, write_gate};
pub(crate) use agmen_core::{Handled, HubPost, PostError, Posted, ReplyRoute};

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

/// Ingest's transport state: the sweeper's wake-up and the
/// events streams' wake-up.
#[derive(Debug, Default)]
pub(crate) struct IngestState {
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
        let handled = self
            .core()
            .ingest_with_gate(topics, from, raw, async {
                write_gate(self, topics.tenant())
                    .await
                    .map_err(|err| agmen_core::StoreError::Backend(Box::new(err)))
            })
            .await
            .map_err(from_core)?;
        if let Some(limit) = handled.quota_warning {
            let alerted = async {
                let name: String = sqlx::query_scalar("SELECT name FROM tenants WHERE id = $1")
                    .bind(topics.tenant().as_str()).fetch_one(self.pool()).await
                    .map_err(|err| PostError::Store(Box::new(err)))?;
                let limit = with_thousands(limit);
                let body = format!("{name} used 80% of today's {limit} messages; posts stop at {limit} until 00:00 UTC.");
                post_owner_alert(self, topics.tenant(), &body).await
            }.await;
            if let Err(err) = alerted {
                tracing::warn!(tenant = %topics.tenant(), error = %error_chain(&err), "posting the daily quota alert failed");
            }
        }
        if let Some(recorded) = &handled.recorded {
            self.ingest_state().stored(topics.project());
            self.on_recorded(
                topics,
                recorded.verb,
                &recorded.from,
                recorded.from_kind,
                &recorded.id,
            )
            .await;
        }
        Ok(handled)
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

fn from_core(err: agmen_core::IngestError) -> IngestError {
    match err {
        agmen_core::IngestError::NoMessageId => IngestError::NoMessageId,
        agmen_core::IngestError::UnknownProject => IngestError::UnknownProject,
        agmen_core::IngestError::Store(agmen_core::StoreError::Backend(source)) => {
            IngestError::Database(match source.downcast::<sqlx::Error>() {
                Ok(err) => *err,
                Err(source) => sqlx::Error::Decode(source),
            })
        }
        other => IngestError::Database(sqlx::Error::Decode(Box::new(other))),
    }
}

/// Whether the project belongs to this hub, including its tenant scope.
pub(crate) async fn project_exists(pool: &PgPool, topics: &Topics) -> Result<bool, sqlx::Error> {
    crate::store_pg::project_exists(pool, topics).await
}
