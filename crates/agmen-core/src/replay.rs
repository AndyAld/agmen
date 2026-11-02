//! Bounded replay pages shared by storage backends.

use agmen_proto::{ProjectId, StampedEnvelope, Stream};
use serde::Serialize;

use crate::{Core, Reads, Store, StoreError};

/// The most messages one page holds, and the default replay page size.
pub const MAX_PAGE: u32 = 500;

/// One page of a stream, including the retained history's pruning point.
#[derive(Debug, Clone, Serialize)]
pub struct Page {
    /// The messages after the cursor, in seq order.
    pub messages: Vec<StampedEnvelope>,
    /// The cursor for the next page; `None` once this page reaches `head`.
    pub next_after: Option<u64>,
    /// The stream's last seq; 0 for a stream with no messages.
    pub head: u64,
    /// The highest seq removed from this stream; 0 when none was pruned.
    pub pruned_through: u64,
}

impl Page {
    /// Assembles a page after reading its messages and then its head.
    #[must_use]
    pub fn new(messages: Vec<StampedEnvelope>, head: u64, pruned_through: u64) -> Self {
        let next_after = messages
            .last()
            .map(|message| message.seq.0)
            .filter(|last| *last < head);
        Self {
            messages,
            next_after,
            head,
            pruned_through,
        }
    }
}

impl<S: Store> Core<S> {
    /// The next page after `after`, with `limit` capped at [`MAX_PAGE`].
    ///
    /// A cursor below `pruned_through` returns the surviving messages after
    /// it, preserving their original sequence numbers.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when `after` exceeds the signed 64-bit storage
    /// range, or when the read or stored envelope decoding fails.
    ///
    /// ```no_run
    /// # async fn example<S: agmen_core::Store>(core: &agmen_core::Core<S>, project: &agmen_proto::ProjectId) -> Result<(), agmen_core::StoreError> {
    /// let page = core.page(project, &agmen_proto::Stream::Feed, 0, 100).await?;
    /// assert!(page.messages.len() <= 100);
    /// # Ok(()) }
    /// ```
    pub async fn page(
        &self,
        project: &ProjectId,
        stream: &Stream,
        after: u64,
        limit: u32,
    ) -> Result<Page, StoreError> {
        i64::try_from(after).map_err(|err| StoreError::Backend(Box::new(err)))?;
        self.store()
            .reader()
            .page(project, stream, after, limit.min(MAX_PAGE))
            .await
    }
}

#[cfg(test)]
#[path = "replay_tests.rs"]
pub(crate) mod tests;
