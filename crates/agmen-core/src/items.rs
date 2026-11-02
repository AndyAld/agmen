//! FOUND, ASSIGN, closing claimed items and bounded item lists.

use crate::{
    Core, HubPost, LedgerError, LedgerMessage, NewItem, Reads, Store, StoreError, StoreTx, post_in,
};
use agmen_proto::{
    Body, IdentityId, ItemKey, LaneName, Payload, ProjectId, Severity, Stream, Topics,
};
use serde::Serialize;

const FOUND_PREFIX: &str = "B";

#[cfg(test)]
#[path = "items_tests.rs"]
mod list_tests;

/// The most items one list holds, ordered oldest first.
pub const MAX_LISTED: i64 = 1000;

/// A severity as the `items` table spells it.
pub(crate) const fn severity_str(severity: Severity) -> &'static str {
    match severity {
        Severity::Low => "low",
        Severity::Med => "med",
        Severity::High => "high",
        Severity::Security => "security",
    }
}

/// One listed item. A security item's title is left out.
#[derive(Debug, Clone, Serialize)]
pub struct ItemView {
    /// The minted item key.
    pub key: String,
    /// The finding's title, absent for security findings.
    pub title: Option<String>,
    /// The item's lane, in storage spelling.
    pub lane: Option<String>,
    /// The finding's severity, in storage spelling.
    pub severity: Option<String>,
    /// The item's state, in storage spelling.
    pub state: String,
    /// The finder's identity id.
    pub created_by: String,
    /// The assignee's identity id, when assigned.
    pub assigned_to: Option<String>,
    /// The identity with the live claim.
    pub holder: Option<String>,
    /// The live claim holder's handle.
    pub holder_handle: Option<String>,
    /// When the live claim's lease ends.
    pub lease_until: Option<agmen_proto::Timestamp>,
}

/// The bounded item-list answer.
#[derive(Debug, Clone, Serialize)]
pub struct ItemList {
    /// The oldest items, with security titles removed.
    pub items: Vec<ItemView>,
}

impl<S: Store> Core<S> {
    /// Lists at most [`MAX_LISTED`] items with live holders and security titles removed.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the read or stored timestamp conversion fails.
    ///
    /// ```no_run
    /// # async fn example<S: agmen_core::Store>(core: &agmen_core::Core<S>, project: &agmen_proto::ProjectId) -> Result<(), agmen_core::StoreError> {
    /// let list = core.items(project).await?;
    /// assert!(list.items.len() <= agmen_core::MAX_LISTED as usize);
    /// # Ok(()) }
    /// ```
    pub async fn items(&self, project: &ProjectId) -> Result<ItemList, StoreError> {
        let rows = self.store().reader().items(project).await?;
        let items = rows
            .into_iter()
            .map(
                |(
                    key,
                    title,
                    lane,
                    severity,
                    state,
                    created_by,
                    assigned_to,
                    holder,
                    handle,
                    until,
                )| {
                    let secret = severity.as_deref() == Some(severity_str(Severity::Security));
                    Ok(ItemView {
                        key,
                        title: (!secret).then_some(title),
                        lane,
                        severity,
                        state,
                        created_by,
                        assigned_to,
                        holder,
                        holder_handle: handle,
                        lease_until: until
                            .map(agmen_proto::Timestamp::from_millisecond)
                            .transpose()
                            .map_err(|err| StoreError::Backend(Box::new(err)))?,
                    })
                },
            )
            .collect::<Result<_, StoreError>>()?;
        Ok(ItemList { items })
    }
}

/// FOUND: mints the next `B-<n>` key, stores the item, and answers in `tx`
/// with ITEM `{key}` in the feed and the finder's inbox. The ITEM carries
/// only the key and lane, so a security finding's title stays in the
/// finder's inboxes (protocol 4).
pub(crate) async fn found<T: StoreTx>(
    tx: &mut T,
    topics: &Topics,
    msg: &LedgerMessage,
    title: &str,
    lane: Option<&LaneName>,
    severity: Option<Severity>,
) -> Result<(), LedgerError> {
    let project = topics.project();
    let n = tx.next_item_number(project, FOUND_PREFIX).await?;
    let key: ItemKey = format!("{FOUND_PREFIX}-{n}").parse()?;
    tx.insert_item(&NewItem {
        project,
        key: &key,
        title,
        lane,
        severity,
        created_by: &msg.from,
    })
    .await?;
    let body = match lane {
        Some(lane) => format!("{key} ({lane})"),
        None => key.to_string(),
    };
    post_in(
        tx,
        topics,
        HubPost {
            payload: Payload::Item { key: key.clone() },
            body: Body::try_from(body)?,
            reply_to: Some(msg.env.id.clone()),
            items: vec![key],
            lane: lane.cloned(),
            mentioned: Vec::new(),
            streams: vec![Stream::Feed, Stream::Inbox(msg.from.clone())],
        },
    )
    .await?;
    Ok(())
}

/// ASSIGN: records `to` as `item`'s assignee, which lets it claim the item
/// in a lane another identity owns. Ingest already routed the ASSIGN to
/// the assignee's inbox; an unknown item, or an assignee that is a human,
/// records nothing.
pub(crate) async fn assign<T: StoreTx>(
    tx: &mut T,
    project: &ProjectId,
    item: &ItemKey,
    to: &IdentityId,
) -> Result<(), LedgerError> {
    let assigned = tx.assign_item(project, item, to).await?;
    if assigned == 0 {
        tracing::debug!(%item, %to, "ASSIGN recorded nothing");
    }
    Ok(())
}

/// Ends `holder`'s live claim on `item` and moves the item to done, which
/// closes it to new claims. A claim is live by the test
/// [`crate::Reads::lane_holders`] uses: not released and its lease not
/// over. Without one, nothing changes.
pub(crate) async fn close_claimed<T: StoreTx>(
    tx: &mut T,
    project: &ProjectId,
    item: &ItemKey,
    holder: &IdentityId,
) -> Result<(), LedgerError> {
    let released = tx.close_claim(project, item, holder).await?;
    if released == 0 {
        tracing::debug!(%item, %holder, "DONE from an identity without a live claim");
        return Ok(());
    }
    tx.mark_done(project, item).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use agmen_proto::{Payload, Severity, Stream};

    use crate::claims::tests::{apply, message, setup};
    use crate::{Store, StoreTx};

    #[tokio::test]
    async fn found_mints_sequential_keys() {
        let (store, topics) = setup().await;
        for severity in [None, Some(Severity::Security)] {
            apply(
                &store,
                &topics,
                &message(
                    "aaaaaaaa",
                    "a",
                    Payload::Found {
                        title: "private title".into(),
                        lane: None,
                        severity,
                    },
                ),
            )
            .await;
        }
        let rows = store.recorded_messages().await.unwrap();
        let feed: Vec<_> = rows
            .iter()
            .filter(|row| row.stream == Stream::Feed)
            .collect();
        assert_eq!(feed.len(), 2);
        for (row, key) in feed.iter().zip(["B-1", "B-2"]) {
            assert_eq!(
                row.client.payload,
                Payload::Item {
                    key: key.parse().unwrap()
                }
            );
            assert_eq!(row.client.body.as_str(), key);
            assert!(!row.client.body.as_str().contains("private title"));
        }
        assert_eq!(rows.len(), 4);
        let mut tx = store.begin().await.unwrap();
        assert!(
            tx.lock_item(topics.project(), &"B-1".parse().unwrap())
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            tx.lock_item(topics.project(), &"B-2".parse().unwrap())
                .await
                .unwrap()
                .is_some()
        );
    }
}
