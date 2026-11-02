//! SQLite coordination record for the local origin.

mod heads;
mod ledger;
mod messages;
mod presence;
mod replay;
pub(crate) mod schema;

#[cfg(test)]
mod test_rows;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Duration;

use agmen_core::{
    ExpiredClaim, ItemLock, ItemRow, LaneHolder, LaneRow, Lapsed, LatestHellos, MessageRow,
    NewItem, NewLend, Page, Parent, QuotaTaken, Reads, ReleaseBy, ResolvedLane, Roster, RosterRow,
    Sender, Store, StoreError, StoreTx,
};
use agmen_proto::{
    IdentityId, IdentityKind, ItemKey, LaneName, MessageId, ProjectId, Seq, Severity, Stream,
    TenantId, Timestamp, Topics,
};
use rusqlite::Connection;
use tokio::sync::{Mutex, OwnedMutexGuard};

pub(crate) type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;
pub(crate) const ACQUIRE_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
    clock: Clock,
    acquire_wait: Duration,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStore")
            .field("acquire_wait", &self.acquire_wait)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct SqliteReader<'a>(&'a SqliteStore);

#[derive(Debug)]
pub(crate) struct SqliteTx {
    held: Option<OwnedMutexGuard<Connection>>,
    now: i64,
}

impl SqliteStore {
    async fn acquire(&self) -> Result<OwnedMutexGuard<Connection>, StoreError> {
        tokio::time::timeout(self.acquire_wait, self.conn.clone().lock_owned())
            .await
            .map_err(|err| StoreError::Backend(Box::new(err)))
    }

    pub(super) async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection, i64) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let held = self.acquire().await?;
        // Read the clock only once the connection is held, so waiting for a
        // slow writer cannot hand out a time older than that writer's commit.
        let clock = self.clock.clone();
        tokio::task::spawn_blocking(move || {
            rollback_unfinished(&held)?;
            f(&held, micros(clock()))
        })
        .await
        .map_err(|err| StoreError::Backend(Box::new(err)))?
    }

    pub(super) async fn write<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection, i64) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let held = self.acquire().await?;
        let clock = self.clock.clone();
        tokio::task::spawn_blocking(move || {
            rollback_unfinished(&held)?;
            held.execute_batch("BEGIN IMMEDIATE").map_err(backend)?;
            // As in `begin`: the transaction's time starts under the write lock.
            match f(&held, micros(clock())) {
                Ok(value) => {
                    held.execute_batch("COMMIT").map_err(backend)?;
                    Ok(value)
                }
                Err(err) => {
                    held.execute_batch("ROLLBACK").map_err(backend)?;
                    Err(err)
                }
            }
        })
        .await
        .map_err(|err| StoreError::Backend(Box::new(err)))?
    }

    #[cfg(test)]
    fn with_acquire_wait(mut self, wait: Duration) -> Self {
        self.acquire_wait = wait;
        self
    }
}

impl SqliteReader<'_> {
    async fn call<T: Send + 'static>(
        &mut self,
        f: impl FnOnce(&Connection, i64) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        self.0.read(f).await
    }
}

impl SqliteTx {
    pub(super) async fn call<T: Send + 'static>(
        &mut self,
        f: impl FnOnce(&Connection, i64) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        // A cancelled call loses its guard. Never resume that transaction on
        // a connection a later caller may already have rolled back and reused.
        let held = self.held.take().ok_or_else(cancelled)?;
        let now = self.now;
        let (held, result) = tokio::task::spawn_blocking(move || {
            let result = f(&held, now);
            (held, result)
        })
        .await
        .map_err(|err| StoreError::Backend(Box::new(err)))?;
        self.held = Some(held);
        result
    }

    #[cfg(test)]
    fn now_micros(&self) -> i64 {
        self.now
    }
}

impl Store for SqliteStore {
    type Reader<'a> = SqliteReader<'a>;
    type Tx<'a> = SqliteTx;

    fn reader(&self) -> Self::Reader<'_> {
        SqliteReader(self)
    }

    async fn begin(&self) -> Result<Self::Tx<'_>, StoreError> {
        let held = self.acquire().await?;
        let clock = self.clock.clone();
        tokio::task::spawn_blocking(move || {
            rollback_unfinished(&held)?;
            held.execute_batch("BEGIN IMMEDIATE").map_err(backend)?;
            Ok(SqliteTx {
                held: Some(held),
                now: micros(clock()),
            })
        })
        .await
        .map_err(|err| StoreError::Backend(Box::new(err)))?
    }
}

fn rollback_unfinished(conn: &Connection) -> Result<(), StoreError> {
    if !conn.is_autocommit() {
        conn.execute_batch("ROLLBACK").map_err(backend)?;
    }
    Ok(())
}

fn cancelled() -> StoreError {
    StoreError::Stored("SQLite transaction call was cancelled".into())
}

pub(super) fn micros(at: Timestamp) -> i64 {
    at.as_microsecond()
}

pub(super) fn from_micros(at: i64) -> Result<Timestamp, StoreError> {
    Timestamp::from_microsecond(at).map_err(|err| StoreError::Stored(err.to_string()))
}

pub(super) fn backend(err: rusqlite::Error) -> StoreError {
    StoreError::Backend(Box::new(err))
}

#[cfg(test)]
pub(crate) fn test_clock(at: Timestamp) -> (Clock, Arc<std::sync::Mutex<Timestamp>>) {
    let time = Arc::new(std::sync::Mutex::new(at));
    let clock = time.clone();
    (
        Arc::new(move || *clock.lock().expect("test clock is not poisoned")),
        time,
    )
}

#[derive(Debug, Clone)]
pub(super) struct OwnedMessageRow {
    pub(super) project: ProjectId,
    pub(super) stream: Stream,
    pub(super) seq: Seq,
    pub(super) id: MessageId,
    pub(super) from: IdentityId,
    pub(super) payload_hash: Vec<u8>,
    pub(super) envelope_json: String,
    pub(super) hub_at: Timestamp,
    pub(super) ack_row: bool,
}

impl From<&MessageRow<'_>> for OwnedMessageRow {
    fn from(row: &MessageRow<'_>) -> Self {
        Self {
            project: row.project.clone(),
            stream: row.stream.clone(),
            seq: row.seq,
            id: row.id.clone(),
            from: row.from.clone(),
            payload_hash: row.payload_hash.to_vec(),
            envelope_json: row.envelope_json.to_owned(),
            hub_at: row.hub_at,
            ack_row: row.ack_row,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct OwnedItem {
    pub(super) project: ProjectId,
    pub(super) key: ItemKey,
    pub(super) title: String,
    pub(super) lane: Option<LaneName>,
    pub(super) severity: Option<Severity>,
    pub(super) created_by: IdentityId,
}

impl From<&NewItem<'_>> for OwnedItem {
    fn from(item: &NewItem<'_>) -> Self {
        Self {
            project: item.project.clone(),
            key: item.key.clone(),
            title: item.title.to_owned(),
            lane: item.lane.cloned(),
            severity: item.severity,
            created_by: item.created_by.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct OwnedLend {
    pub(super) project: ProjectId,
    pub(super) lane: LaneName,
    pub(super) message_id: MessageId,
    pub(super) from: IdentityId,
    pub(super) to: IdentityId,
    pub(super) paths: Vec<String>,
    pub(super) until: Timestamp,
}

impl From<&NewLend<'_>> for OwnedLend {
    fn from(lend: &NewLend<'_>) -> Self {
        Self {
            project: lend.project.clone(),
            lane: lend.lane.clone(),
            message_id: lend.message_id.clone(),
            from: lend.from.clone(),
            to: lend.to.clone(),
            paths: lend.paths.to_vec(),
            until: lend.until,
        }
    }
}

impl Reads for SqliteReader<'_> {
    async fn page(
        &mut self,
        project: &ProjectId,
        stream: &Stream,
        after: u64,
        limit: u32,
    ) -> Result<Page, StoreError> {
        let project = project.clone();
        let stream = stream.clone();
        self.call(move |conn, now| replay::page(conn, now, project, stream, after, limit))
            .await
    }

    async fn items(&mut self, project: &ProjectId) -> Result<Vec<ItemRow>, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| ledger::items(conn, now, project))
            .await
    }

    async fn project_exists(&mut self, topics: &Topics) -> Result<bool, StoreError> {
        let topics = topics.clone();
        self.call(move |conn, now| messages::project_exists(conn, now, topics))
            .await
    }

    async fn sender(
        &mut self,
        topics: &Topics,
        from: &IdentityId,
    ) -> Result<Option<Sender>, StoreError> {
        let topics = topics.clone();
        let from = from.clone();
        self.call(move |conn, now| messages::sender(conn, now, topics, from))
            .await
    }

    async fn parent(
        &mut self,
        project: &ProjectId,
        sender: &IdentityId,
        reply_to: &MessageId,
    ) -> Result<Option<Parent>, StoreError> {
        let project = project.clone();
        let sender = sender.clone();
        let reply_to = reply_to.clone();
        self.call(move |conn, now| messages::parent(conn, now, project, sender, reply_to))
            .await
    }

    async fn is_delegate(
        &mut self,
        project: &ProjectId,
        worker: &IdentityId,
        delegator: &IdentityId,
    ) -> Result<bool, StoreError> {
        let project = project.clone();
        let worker = worker.clone();
        let delegator = delegator.clone();
        self.call(move |conn, now| messages::is_delegate(conn, now, project, worker, delegator))
            .await
    }

    async fn recorded(
        &mut self,
        from: &IdentityId,
        id: &MessageId,
    ) -> Result<Option<(Stream, Seq, Vec<u8>)>, StoreError> {
        let from = from.clone();
        let id = id.clone();
        self.call(move |conn, now| messages::recorded(conn, now, from, id))
            .await
    }

    async fn roster(&mut self, project: &ProjectId) -> Result<Roster, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| messages::roster(conn, now, project))
            .await
    }

    async fn ref_targets(
        &mut self,
        project: &ProjectId,
        candidates: &[String],
    ) -> Result<(Vec<String>, Vec<ItemKey>), StoreError> {
        let project = project.clone();
        let candidates = candidates.to_vec();
        self.call(move |conn, now| messages::ref_targets(conn, now, project, candidates))
            .await
    }

    async fn lane_globs(
        &mut self,
        project: &ProjectId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| ledger::lane_globs(conn, now, project))
            .await
    }

    async fn lane_holders(&mut self, project: &ProjectId) -> Result<Vec<LaneHolder>, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| ledger::lane_holders(conn, now, project))
            .await
    }

    async fn live_edit_scope(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<Option<(IdentityKind, Option<serde_json::Value>)>, StoreError> {
        let project = project.clone();
        let id = id.clone();
        self.call(move |conn, now| ledger::live_edit_scope(conn, now, project, id))
            .await
    }

    async fn lends_to(
        &mut self,
        project: &ProjectId,
        to: &IdentityId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        let project = project.clone();
        let to = to.clone();
        self.call(move |conn, now| ledger::lends_to(conn, now, project, to))
            .await
    }

    async fn lane_list(&mut self, project: &ProjectId) -> Result<Vec<LaneRow>, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| ledger::lane_list(conn, now, project))
            .await
    }

    async fn roster_rows(&mut self, project: &ProjectId) -> Result<Vec<RosterRow>, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| presence::roster_rows(conn, now, project))
            .await
    }

    async fn latest_hellos(&mut self, project: &ProjectId) -> Result<LatestHellos, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| presence::latest_hellos(conn, now, project))
            .await
    }
}

impl Reads for SqliteTx {
    async fn page(
        &mut self,
        project: &ProjectId,
        stream: &Stream,
        after: u64,
        limit: u32,
    ) -> Result<Page, StoreError> {
        let project = project.clone();
        let stream = stream.clone();
        self.call(move |conn, now| replay::page(conn, now, project, stream, after, limit))
            .await
    }

    async fn items(&mut self, project: &ProjectId) -> Result<Vec<ItemRow>, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| ledger::items(conn, now, project))
            .await
    }

    async fn project_exists(&mut self, topics: &Topics) -> Result<bool, StoreError> {
        let topics = topics.clone();
        self.call(move |conn, now| messages::project_exists(conn, now, topics))
            .await
    }

    async fn sender(
        &mut self,
        topics: &Topics,
        from: &IdentityId,
    ) -> Result<Option<Sender>, StoreError> {
        let topics = topics.clone();
        let from = from.clone();
        self.call(move |conn, now| messages::sender(conn, now, topics, from))
            .await
    }

    async fn parent(
        &mut self,
        project: &ProjectId,
        sender: &IdentityId,
        reply_to: &MessageId,
    ) -> Result<Option<Parent>, StoreError> {
        let project = project.clone();
        let sender = sender.clone();
        let reply_to = reply_to.clone();
        self.call(move |conn, now| messages::parent(conn, now, project, sender, reply_to))
            .await
    }

    async fn is_delegate(
        &mut self,
        project: &ProjectId,
        worker: &IdentityId,
        delegator: &IdentityId,
    ) -> Result<bool, StoreError> {
        let project = project.clone();
        let worker = worker.clone();
        let delegator = delegator.clone();
        self.call(move |conn, now| messages::is_delegate(conn, now, project, worker, delegator))
            .await
    }

    async fn recorded(
        &mut self,
        from: &IdentityId,
        id: &MessageId,
    ) -> Result<Option<(Stream, Seq, Vec<u8>)>, StoreError> {
        let from = from.clone();
        let id = id.clone();
        self.call(move |conn, now| messages::recorded(conn, now, from, id))
            .await
    }

    async fn roster(&mut self, project: &ProjectId) -> Result<Roster, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| messages::roster(conn, now, project))
            .await
    }

    async fn ref_targets(
        &mut self,
        project: &ProjectId,
        candidates: &[String],
    ) -> Result<(Vec<String>, Vec<ItemKey>), StoreError> {
        let project = project.clone();
        let candidates = candidates.to_vec();
        self.call(move |conn, now| messages::ref_targets(conn, now, project, candidates))
            .await
    }

    async fn lane_globs(
        &mut self,
        project: &ProjectId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| ledger::lane_globs(conn, now, project))
            .await
    }

    async fn lane_holders(&mut self, project: &ProjectId) -> Result<Vec<LaneHolder>, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| ledger::lane_holders(conn, now, project))
            .await
    }

    async fn live_edit_scope(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<Option<(IdentityKind, Option<serde_json::Value>)>, StoreError> {
        let project = project.clone();
        let id = id.clone();
        self.call(move |conn, now| ledger::live_edit_scope(conn, now, project, id))
            .await
    }

    async fn lends_to(
        &mut self,
        project: &ProjectId,
        to: &IdentityId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        let project = project.clone();
        let to = to.clone();
        self.call(move |conn, now| ledger::lends_to(conn, now, project, to))
            .await
    }

    async fn lane_list(&mut self, project: &ProjectId) -> Result<Vec<LaneRow>, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| ledger::lane_list(conn, now, project))
            .await
    }

    async fn roster_rows(&mut self, project: &ProjectId) -> Result<Vec<RosterRow>, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| presence::roster_rows(conn, now, project))
            .await
    }

    async fn latest_hellos(&mut self, project: &ProjectId) -> Result<LatestHellos, StoreError> {
        let project = project.clone();
        self.call(move |conn, now| presence::latest_hellos(conn, now, project))
            .await
    }
}

impl StoreTx for SqliteTx {
    async fn take_quota(
        &mut self,
        tenant: &TenantId,
        limit: u32,
    ) -> Result<QuotaTaken, StoreError> {
        let tenant = tenant.clone();
        self.call(move |conn, now| ledger::take_quota(conn, now, tenant, limit))
            .await
    }

    async fn lock_item(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> Result<Option<ItemLock>, StoreError> {
        let project = project.clone();
        let item = item.clone();
        self.call(move |conn, now| ledger::lock_item(conn, now, project, item))
            .await
    }

    async fn release_lapsed(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> Result<Option<Lapsed>, StoreError> {
        let project = project.clone();
        let item = item.clone();
        self.call(move |conn, now| ledger::release_lapsed(conn, now, project, item))
            .await
    }

    async fn active_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> Result<Option<(IdentityId, String)>, StoreError> {
        let project = project.clone();
        let item = item.clone();
        self.call(move |conn, now| ledger::active_claim(conn, now, project, item))
            .await
    }

    async fn extend_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        lease: Duration,
    ) -> Result<Timestamp, StoreError> {
        let project = project.clone();
        let item = item.clone();
        self.call(move |conn, now| ledger::extend_claim(conn, now, project, item, lease))
            .await
    }

    async fn insert_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        from: &IdentityId,
        lease: Duration,
    ) -> Result<Timestamp, StoreError> {
        let project = project.clone();
        let item = item.clone();
        let from = from.clone();
        self.call(move |conn, now| ledger::insert_claim(conn, now, project, item, from, lease))
            .await
    }

    async fn renew_claims(
        &mut self,
        project: &ProjectId,
        from: &IdentityId,
        items: &[ItemKey],
        lease: Duration,
    ) -> Result<u64, StoreError> {
        let project = project.clone();
        let from = from.clone();
        let items = items.to_vec();
        self.call(move |conn, now| ledger::renew_claims(conn, now, project, from, items, lease))
            .await
    }

    async fn release_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        from: &IdentityId,
        by: ReleaseBy,
    ) -> Result<u64, StoreError> {
        let project = project.clone();
        let item = item.clone();
        let from = from.clone();
        self.call(move |conn, now| ledger::release_claim(conn, now, project, item, from, by))
            .await
    }

    async fn next_item_number(
        &mut self,
        project: &ProjectId,
        prefix: &str,
    ) -> Result<u64, StoreError> {
        let project = project.clone();
        let prefix = prefix.to_owned();
        self.call(move |conn, now| ledger::next_item_number(conn, now, project, prefix))
            .await
    }

    async fn insert_item(&mut self, item: &NewItem<'_>) -> Result<(), StoreError> {
        let item = OwnedItem::from(item);
        self.call(move |conn, now| ledger::insert_item(conn, now, item))
            .await
    }

    async fn assign_item(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        to: &IdentityId,
    ) -> Result<u64, StoreError> {
        let project = project.clone();
        let item = item.clone();
        let to = to.clone();
        self.call(move |conn, now| ledger::assign_item(conn, now, project, item, to))
            .await
    }

    async fn close_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        holder: &IdentityId,
    ) -> Result<u64, StoreError> {
        let project = project.clone();
        let item = item.clone();
        let holder = holder.clone();
        self.call(move |conn, now| ledger::close_claim(conn, now, project, item, holder))
            .await
    }

    async fn mark_done(&mut self, project: &ProjectId, item: &ItemKey) -> Result<(), StoreError> {
        let project = project.clone();
        let item = item.clone();
        self.call(move |conn, now| ledger::mark_done(conn, now, project, item))
            .await
    }

    async fn lock_lane_owner(
        &mut self,
        project: &ProjectId,
        lane: &LaneName,
    ) -> Result<Option<Option<IdentityId>>, StoreError> {
        let project = project.clone();
        let lane = lane.clone();
        self.call(move |conn, now| ledger::lock_lane_owner(conn, now, project, lane))
            .await
    }

    async fn identity_live(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<bool, StoreError> {
        let project = project.clone();
        let id = id.clone();
        self.call(move |conn, now| ledger::identity_live(conn, now, project, id))
            .await
    }

    async fn identity_edits(
        &mut self,
        id: &IdentityId,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        let id = id.clone();
        self.call(move |conn, now| ledger::identity_edits(conn, now, id))
            .await
    }

    async fn active_lends(
        &mut self,
        project: &ProjectId,
        lane: &LaneName,
        to: &IdentityId,
    ) -> Result<u64, StoreError> {
        let project = project.clone();
        let lane = lane.clone();
        let to = to.clone();
        self.call(move |conn, now| ledger::active_lends(conn, now, project, lane, to))
            .await
    }

    async fn insert_lend(&mut self, lend: &NewLend<'_>) -> Result<(), StoreError> {
        let lend = OwnedLend::from(lend);
        self.call(move |conn, now| ledger::insert_lend(conn, now, lend))
            .await
    }

    async fn insert_message(&mut self, row: &MessageRow<'_>) -> Result<(), StoreError> {
        let row = OwnedMessageRow::from(row);
        self.call(move |conn, now| messages::insert_message(conn, now, row))
            .await
    }

    async fn enqueue_publish(
        &mut self,
        topics: &Topics,
        topic: &str,
        payload: &[u8],
    ) -> Result<(), StoreError> {
        let topics = topics.clone();
        let topic = topic.to_owned();
        let payload = payload.to_vec();
        self.call(move |conn, now| messages::enqueue_publish(conn, now, topics, topic, payload))
            .await
    }

    async fn lock_head(&mut self, key: &str) -> Result<(), StoreError> {
        let key = key.to_owned();
        self.call(move |conn, now| heads::lock_head(conn, now, key))
            .await
    }

    async fn next_seq(&mut self, key: &str) -> Result<Seq, StoreError> {
        let key = key.to_owned();
        self.call(move |conn, now| heads::next_seq(conn, now, key))
            .await
    }

    async fn replace_lanes(
        &mut self,
        project: &ProjectId,
        lanes: &[ResolvedLane],
    ) -> Result<(), StoreError> {
        let project = project.clone();
        let lanes = lanes.to_vec();
        self.call(move |conn, now| ledger::replace_lanes(conn, now, project, lanes))
            .await
    }

    async fn resolve_owner(
        &mut self,
        project: &ProjectId,
        handle: &str,
    ) -> Result<Option<IdentityId>, StoreError> {
        let project = project.clone();
        let handle = handle.to_owned();
        self.call(move |conn, now| ledger::resolve_owner(conn, now, project, handle))
            .await
    }

    async fn expire_claims(&mut self, limit: u32) -> Result<Vec<ExpiredClaim>, StoreError> {
        self.call(move |conn, now| ledger::expire_claims(conn, now, limit))
            .await
    }

    async fn commit(mut self) -> Result<(), StoreError> {
        let held = self.held.take().ok_or_else(cancelled)?;
        tokio::task::spawn_blocking(move || held.execute_batch("COMMIT").map_err(backend))
            .await
            .map_err(|err| StoreError::Backend(Box::new(err)))?
    }
}
