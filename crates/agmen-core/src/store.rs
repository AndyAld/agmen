//! The backend boundary for coordination reads and atomic writes.

use std::future::Future;

use agmen_proto::{
    IdentityId, IdentityKind, ItemKey, LaneName, MessageId, ProjectId, Seq, Stream, Timestamp,
    Topics,
};

use crate::Roster;
use crate::lanes::{LaneHolder, LaneRow, ResolvedLane};

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
