//! The backend boundary for coordination reads and atomic writes.

use std::future::Future;
use std::time::Duration;

use agmen_proto::{
    IdentityId, IdentityKind, ItemKey, LaneName, MessageId, ProjectId, Seq, Severity, Stream,
    TenantId, Timestamp, Topics,
};

use crate::lanes::{LaneHolder, LaneRow, ResolvedLane};
use crate::{Parent, QuotaTaken, Roster, Sender};

/// An item's state, lane, assignee and lane owner, in storage spelling.
pub type ItemLock = (String, Option<String>, Option<String>, Option<String>);

/// A claim released because its lease ran out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lapsed {
    /// The identity whose lease ended.
    pub holder: IdentityId,
    /// That identity's handle.
    pub handle: String,
}

/// Who may release a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseBy {
    /// Only the claim's holder.
    Holder,
    /// A coordinator may release any holder's claim.
    Coordinator,
}

/// One item to insert after minting its key.
#[derive(Debug, Clone, Copy)]
pub struct NewItem<'a> {
    /// The item's project.
    pub project: &'a ProjectId,
    /// The minted item key.
    pub key: &'a ItemKey,
    /// The finding's title, kept out of the ITEM reply.
    pub title: &'a str,
    /// The item's lane, when given.
    pub lane: Option<&'a LaneName>,
    /// The finding's severity, when given.
    pub severity: Option<Severity>,
    /// The finder.
    pub created_by: &'a IdentityId,
}

/// One authorized temporary lane grant.
#[derive(Debug, Clone, Copy)]
pub struct NewLend<'a> {
    /// The grant's project.
    pub project: &'a ProjectId,
    /// The lane being lent.
    pub lane: &'a LaneName,
    /// The recorded LEND's id.
    pub message_id: &'a MessageId,
    /// The lane owner or coordinator making the grant.
    pub from: &'a IdentityId,
    /// The borrower.
    pub to: &'a IdentityId,
    /// The allowed path globs.
    pub paths: &'a [String],
    /// The grant's expiration.
    pub until: Timestamp,
}

/// One numbered copy to insert within a message transaction.
#[derive(Debug, Clone, Copy)]
pub struct MessageRow<'a> {
    /// The project holding this copy.
    pub project: &'a ProjectId,
    /// The stream holding this copy.
    pub stream: &'a Stream,
    /// Its position in the stream.
    pub seq: Seq,
    /// The client or hub message id.
    pub id: &'a MessageId,
    /// The stamped sender.
    pub from: &'a IdentityId,
    /// Hash used to recognize resends.
    pub payload_hash: &'a [u8],
    /// The serialized stamped envelope.
    pub envelope_json: &'a str,
    /// The hub's receive time.
    pub hub_at: Timestamp,
    /// Whether a resend's ACK reports this copy.
    pub ack_row: bool,
}

/// Why a storage operation could not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A record with the same unique key already exists.
    #[error("duplicate record")]
    Duplicate,
    /// Stored data or a backend capacity limit prevents the operation.
    #[error("{0}")]
    Stored(String),
    /// The backend failed, preserving its original error as the source.
    #[error("storage backend failed")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// A kind as the tables and stamped envelopes spell it.
///
/// # Errors
///
/// Returns [`StoreError::Stored`] for an unknown kind, preserving its spelling.
pub fn parse_kind(kind: &str) -> Result<IdentityKind, StoreError> {
    Ok(match kind {
        "human" => IdentityKind::Human,
        "agent" => IdentityKind::Agent,
        "coordinator" => IdentityKind::Coordinator,
        "listener" => IdentityKind::Listener,
        "hub" => IdentityKind::Hub,
        other => return Err(StoreError::Stored(format!("unknown kind {other:?}"))),
    })
}

/// A backend that supplies readers and transactions.
///
/// The core never holds a reader and a transaction at the same time. A
/// transaction needs to be `Send`, but its state need not be `Sync`.
pub trait Store: Send + Sync + 'static {
    /// Queries outside a write transaction.
    type Reader<'a>: Reads
    where
        Self: 'a;

    /// One atomic write transaction, rolled back when dropped.
    type Tx<'a>: StoreTx
    where
        Self: 'a;

    /// Opens a reader without starting a write transaction.
    fn reader(&self) -> Self::Reader<'_>;

    /// Begins an atomic transaction.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the backend cannot begin a transaction.
    fn begin(&self) -> impl Future<Output = Result<Self::Tx<'_>, StoreError>> + Send;
}

/// Queries shared by a reader and an open transaction.
pub trait Reads: Send {
    /// Whether both tenant and project identify a project in this store.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the project lookup fails.
    fn project_exists(
        &mut self,
        topics: &Topics,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// A live project identity or human member, including stored verb permissions.
    ///
    /// # Errors
    /// Returns [`StoreError`] when reading or parsing the sender fails.
    fn sender(
        &mut self,
        topics: &Topics,
        from: &IdentityId,
    ) -> impl Future<Output = Result<Option<Sender>, StoreError>> + Send;

    /// The earliest parent visible in the feed or the sender's inbox, and all its streams.
    ///
    /// # Errors
    /// Returns [`StoreError`] when reading or parsing a parent fails.
    fn parent(
        &mut self,
        project: &ProjectId,
        sender: &IdentityId,
        reply_to: &MessageId,
    ) -> impl Future<Output = Result<Option<Parent>, StoreError>> + Send;

    /// Whether a live worker was delegated by this sender in the project.
    ///
    /// # Errors
    /// Returns [`StoreError`] when reading the delegation fails.
    fn is_delegate(
        &mut self,
        project: &ProjectId,
        worker: &IdentityId,
        delegator: &IdentityId,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// The ACK copy's stream, sequence and canonical payload hash for a resend.
    ///
    /// # Errors
    /// Returns [`StoreError`] when reading or parsing the recorded copy fails.
    fn recorded(
        &mut self,
        from: &IdentityId,
        id: &MessageId,
    ) -> impl Future<Output = Result<Option<(Stream, Seq, Vec<u8>)>, StoreError>> + Send;

    /// Live identities and human members in storage order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when reading or parsing a member fails.
    fn roster(
        &mut self,
        project: &ProjectId,
    ) -> impl Future<Output = Result<Roster, StoreError>> + Send;

    /// Known lane names and matching item keys, each sorted by name.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when reading or parsing targets fails.
    fn ref_targets(
        &mut self,
        project: &ProjectId,
        candidates: &[String],
    ) -> impl Future<Output = Result<(Vec<String>, Vec<ItemKey>), StoreError>> + Send;

    /// The project's lanes, ordered by name.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the lanes cannot be read or a stored name
    /// is not a lane name.
    fn lane_globs(
        &mut self,
        project: &ProjectId,
    ) -> impl Future<Output = Result<Vec<(LaneName, Vec<String>)>, StoreError>> + Send;

    /// Live claims on items that name a lane, oldest claim first.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the claims cannot be read or a stored id
    /// is malformed.
    fn lane_holders(
        &mut self,
        project: &ProjectId,
    ) -> impl Future<Output = Result<Vec<LaneHolder>, StoreError>> + Send;

    /// The live identity's kind and stored edit scope.
    ///
    /// `Ok(None)` means the identity is missing or revoked.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the identity cannot be read or its kind is
    /// not a known kind.
    fn live_edit_scope(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> impl Future<Output = Result<Option<(IdentityKind, Option<serde_json::Value>)>, StoreError>> + Send;

    /// Active lends to `to`, as the lane and its path globs.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the lends cannot be read or a stored lane
    /// name is malformed.
    fn lends_to(
        &mut self,
        project: &ProjectId,
        to: &IdentityId,
    ) -> impl Future<Output = Result<Vec<(LaneName, Vec<String>)>, StoreError>> + Send;

    /// Every lane of `project`, ordered by name, as the list route shows them.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the lanes cannot be read.
    fn lane_list(
        &mut self,
        project: &ProjectId,
    ) -> impl Future<Output = Result<Vec<LaneRow>, StoreError>> + Send;
}

/// Reads and writes within one atomic transaction.
///
/// Dropping a transaction without a successful [`commit`](Self::commit)
/// records nothing. Numbering and head locks last until commit or drop.
pub trait StoreTx: Reads {
    /// Takes the UTC daily usage row before stream heads, counting only on commit.
    /// The warning is acquired once when `messages * 5 >= limit * 4`.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the atomic usage update fails.
    fn take_quota(
        &mut self,
        tenant: &TenantId,
        limit: u32,
    ) -> impl Future<Output = Result<QuotaTaken, StoreError>> + Send;

    /// Locks the item row, returning its state, lane, assignee and lane owner.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the lookup or lock fails.
    fn lock_item(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> impl Future<Output = Result<Option<ItemLock>, StoreError>> + Send;

    /// Releases the unreleased claim when `lease_until <= now`.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the update fails.
    fn release_lapsed(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> impl Future<Output = Result<Option<Lapsed>, StoreError>> + Send;

    /// The unreleased claim's holder and handle, including an expired lease.
    ///
    /// # Errors
    /// Returns [`StoreError`] when reading the claim fails.
    fn active_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> impl Future<Output = Result<Option<(IdentityId, String)>, StoreError>> + Send;

    /// Extends the unreleased claim using the backend's clock.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the update or timestamp conversion fails.
    fn extend_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        lease: Duration,
    ) -> impl Future<Output = Result<Timestamp, StoreError>> + Send;

    /// Inserts a claim using the backend's clock for the lease end.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the insert or timestamp conversion fails.
    fn insert_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        from: &IdentityId,
        lease: Duration,
    ) -> impl Future<Output = Result<Timestamp, StoreError>> + Send;

    /// Renews only `from`'s unreleased claims whose lease is still live.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the update fails.
    fn renew_claims(
        &mut self,
        project: &ProjectId,
        from: &IdentityId,
        items: &[ItemKey],
        lease: Duration,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Releases a claim held by `from`, or any holder when `by` is coordinator.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the update fails.
    fn release_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        from: &IdentityId,
        by: ReleaseBy,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Takes the next item number for a project and prefix, starting at one.
    ///
    /// # Errors
    /// Returns [`StoreError`] when numbering fails.
    fn next_item_number(
        &mut self,
        project: &ProjectId,
        prefix: &str,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Inserts the finding with its minted key.
    ///
    /// # Errors
    /// Returns [`StoreError`] when insertion fails.
    fn insert_item(
        &mut self,
        item: &NewItem<'_>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Assigns an existing item when `to` is a project identity, even if revoked.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the update fails.
    fn assign_item(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        to: &IdentityId,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Releases `holder`'s claim only while its lease is live.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the update fails.
    fn close_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        holder: &IdentityId,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Marks the item done after its live claim is released.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the update fails.
    fn mark_done(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Locks the lane row; the outer option says whether the lane exists.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the lookup or lock fails.
    fn lock_lane_owner(
        &mut self,
        project: &ProjectId,
        lane: &LaneName,
    ) -> impl Future<Output = Result<Option<Option<IdentityId>>, StoreError>> + Send;

    /// Whether `id` is an unrevoked project identity.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the lookup fails.
    fn identity_live(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// The stored edit scope by id, without a project or revocation filter.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the lookup fails.
    fn identity_edits(
        &mut self,
        id: &IdentityId,
    ) -> impl Future<Output = Result<Option<serde_json::Value>, StoreError>> + Send;

    /// Counts grants to this borrower whose expiration is strictly after now.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the count fails.
    fn active_lends(
        &mut self,
        project: &ProjectId,
        lane: &LaneName,
        to: &IdentityId,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Inserts an authorized lane grant.
    ///
    /// # Errors
    /// Returns [`StoreError`] when insertion fails.
    fn insert_lend(
        &mut self,
        lend: &NewLend<'_>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Inserts one copy, returning [`StoreError::Duplicate`] on a
    /// `(from, id, stream)` conflict.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if inserting the copy fails.
    fn insert_message(
        &mut self,
        row: &MessageRow<'_>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Queues a publish in the same transaction as its message.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if queuing the publish fails.
    fn enqueue_publish(
        &mut self,
        topics: &Topics,
        topic: &str,
        payload: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Locks a key's head without taking a sequence number.
    ///
    /// Creates a missing head at zero, so its next sequence is still one.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the lock or head creation fails.
    fn lock_head(&mut self, key: &str) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Takes the next sequence number, starting at one for each key.
    ///
    /// Holds the key until commit or drop. Dropping the transaction returns
    /// every number it took, so committed numbers have no gaps.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if numbering fails or the sequence is exhausted.
    fn next_seq(&mut self, key: &str) -> impl Future<Output = Result<Seq, StoreError>> + Send;

    /// Makes all writes visible and releases the transaction's locks.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the backend cannot commit the transaction.
    fn commit(self) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Replaces the project's lanes with `lanes` under one new version.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the project cannot be locked or the rows
    /// cannot be written.
    fn replace_lanes(
        &mut self,
        project: &ProjectId,
        lanes: &[ResolvedLane],
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The id of a live identity named by id or by handle.
    ///
    /// `Ok(None)` means no live identity has that id or handle.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the lookup fails.
    fn resolve_owner(
        &mut self,
        project: &ProjectId,
        handle: &str,
    ) -> impl Future<Output = Result<Option<IdentityId>, StoreError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_kinds_match_the_protocol() {
        for (text, kind) in [
            ("human", IdentityKind::Human),
            ("agent", IdentityKind::Agent),
            ("coordinator", IdentityKind::Coordinator),
            ("listener", IdentityKind::Listener),
            ("hub", IdentityKind::Hub),
        ] {
            assert_eq!(parse_kind(text).unwrap(), kind);
        }
        for text in ["", "Agent", "other", "a\"b"] {
            let error = parse_kind(text).unwrap_err();
            assert!(matches!(error, StoreError::Stored(_)));
            assert_eq!(error.to_string(), format!("unknown kind {text:?}"));
        }
    }
}
