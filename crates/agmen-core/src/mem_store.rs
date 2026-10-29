//! Bounded, serializable test storage with rollback on drop.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;

#[cfg(test)]
use agmen_proto::StampedEnvelope;
use agmen_proto::{
    IdentityId, IdentityKind, ItemKey, LaneName, MessageId, ProjectId, Seq, Severity, Stream,
    Timestamp, Topics,
};
use tokio::sync::{Mutex, MutexGuard, OwnedMutexGuard};

use crate::lanes::{LaneHolder, LaneRow, ResolvedLane};
use crate::{
    ItemLock, Lapsed, Member, MemberRole, MessageRow, NewItem, NewLend, Reads, ReleaseBy, Roster,
    Store, StoreError, StoreTx,
};

/// The maximum rows in each in-memory collection.
pub const MEM_MAX_ROWS: usize = 100_000;

/// The `item_counters.n` ceiling in the ledger schema.
const MAX_ITEM_NUMBER: u64 = 999_999;

/// A forgotten transaction must fail a waiting test instead of hanging it.
const MEM_TX_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
struct MemIdentity {
    handle: String,
    kind: IdentityKind,
    edits: Option<serde_json::Value>,
    live: bool,
    owner: Option<IdentityId>,
    role: Option<MemberRole>,
    identity_role: Option<String>,
}

#[derive(Debug, Clone)]
struct MemLane {
    name: LaneName,
    globs: Vec<String>,
    owner: Option<IdentityId>,
    v: i64,
}

#[derive(Debug, Clone)]
struct MemClaim {
    project: ProjectId,
    item: ItemKey,
    holder: IdentityId,
    granted_at: Timestamp,
    lease_until: Timestamp,
    released: bool,
}

#[derive(Debug, Clone)]
struct MemLend {
    project: ProjectId,
    lane: LaneName,
    message_id: MessageId,
    from: IdentityId,
    to: IdentityId,
    paths: Vec<String>,
    until: Timestamp,
}

#[derive(Debug, Clone)]
struct MemItem {
    state: String,
    lane: Option<LaneName>,
    assigned_to: Option<IdentityId>,
    // shortcut: retain finding details until Task 9 adds item-list reads.
    #[expect(dead_code, reason = "item-list reads arrive in Task 9")]
    title: String,
    #[expect(dead_code, reason = "item-list reads arrive in Task 9")]
    severity: Option<Severity>,
    #[expect(dead_code, reason = "item-list reads arrive in Task 9")]
    created_by: IdentityId,
}

#[derive(Debug, Clone, Default)]
struct MemState {
    heads: BTreeMap<String, Seq>,
    identities: BTreeMap<ProjectId, BTreeMap<IdentityId, MemIdentity>>,
    lanes: BTreeMap<ProjectId, Vec<MemLane>>,
    holders: BTreeMap<ProjectId, Vec<LaneHolder>>,
    lends: Vec<MemLend>,
    claims: Vec<MemClaim>,
    item_counters: BTreeMap<(ProjectId, String), u64>,
    messages: Vec<MemMessage>,
    outbox: Vec<(Topics, String, Vec<u8>)>,
    items: BTreeMap<ProjectId, BTreeMap<ItemKey, MemItem>>,
}

#[derive(Debug, Clone)]
// shortcut: keep complete rows for Task 7 resend reads and Task 9 replay.
#[expect(
    dead_code,
    reason = "message query methods arrive with ingest and replay"
)]
struct MemMessage {
    from: IdentityId,
    id: MessageId,
    stream: Stream,
    project: ProjectId,
    seq: Seq,
    payload_hash: Vec<u8>,
    envelope_json: String,
    hub_at: Timestamp,
    ack_row: bool,
}

/// In-memory storage for backend-independent coordination tests.
///
/// Clones share committed state, the clock and a single pending fault.
/// Transactions serialize on one mutex and edit a working copy, so dropping
/// a transaction discards all its writes.
#[derive(Debug, Clone)]
pub struct MemStore {
    state: Arc<Mutex<MemState>>,
    clock: Arc<StdMutex<Timestamp>>,
    failure: Arc<StdMutex<Option<&'static str>>>,
}

impl Default for MemStore {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(MemState::default())),
            clock: Arc::new(StdMutex::new(Timestamp::now())),
            failure: Arc::new(StdMutex::new(None)),
        }
    }
}

impl MemStore {
    #[cfg(test)]
    pub(crate) async fn recorded_messages(&self) -> Result<Vec<StampedEnvelope>, StoreError> {
        let state = self.locked().await?;
        state
            .messages
            .iter()
            .map(|row| {
                serde_json::from_str(&row.envelope_json)
                    .map_err(|err| StoreError::Backend(Box::new(err)))
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) async fn queued_publishes(&self) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        Ok(self
            .locked()
            .await?
            .outbox
            .iter()
            .map(|(_, topic, payload)| (topic.clone(), payload.clone()))
            .collect())
    }

    #[cfg(test)]
    pub(crate) async fn seed_item(
        &self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> Result<(), StoreError> {
        let mut tx = self.begin().await?;
        if tx
            .working
            .items
            .get(project)
            .is_some_and(|items| items.contains_key(item))
        {
            return Ok(());
        }
        tx.insert_item(&NewItem {
            project,
            key: item,
            title: "seeded item",
            lane: None,
            severity: None,
            created_by: &"aaaaaaaa"
                .parse()
                .map_err(|err: agmen_proto::ProtoError| StoreError::Stored(err.to_string()))?,
        })
        .await?;
        tx.commit().await
    }

    #[cfg(test)]
    pub(crate) async fn seed_member(
        &self,
        project: &ProjectId,
        member: Member,
    ) -> Result<(), StoreError> {
        let mut state = self.locked().await?;
        if !state
            .identities
            .get(project)
            .is_some_and(|ids| ids.contains_key(&member.id))
            && identity_rows(&state) >= MEM_MAX_ROWS
        {
            return Err(full());
        }
        state.identities.entry(project.clone()).or_default().insert(
            member.id,
            MemIdentity {
                handle: member.handle,
                kind: member.kind,
                edits: None,
                live: true,
                owner: member.owner,
                role: member.role,
                identity_role: member.identity_role,
            },
        );
        Ok(())
    }

    /// Sets the test clock, initially the time this store was created.
    pub fn set_now(&self, at: Timestamp) {
        *self.clock.lock().unwrap_or_else(PoisonError::into_inner) = at;
    }

    /// Fails the next call to method `op` with [`StoreError::Backend`].
    ///
    /// Replaces any pending fault. Other methods do not consume it, and a
    /// rollback does not rearm a consumed fault.
    pub fn fail_next(&self, op: &'static str) {
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(op);
    }

    /// Records a live identity with no stored edit scope.
    #[cfg(test)]
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Stored`] when the identity collection is full.
    pub(crate) async fn seed_live_identity(
        &self,
        project: &ProjectId,
        id: &IdentityId,
        handle: &str,
        kind: IdentityKind,
    ) -> Result<(), StoreError> {
        fail(&self.failure, "seed_live_identity")?;
        let mut state = self.locked().await?;
        if let Some(existing) = state
            .identities
            .get_mut(project)
            .and_then(|ids| ids.get_mut(id))
        {
            existing.handle = handle.to_owned();
            existing.kind = kind;
            existing.edits = None;
            existing.live = true;
            return Ok(());
        }
        if identity_rows(&state) >= MEM_MAX_ROWS {
            return Err(full());
        }
        state.identities.entry(project.clone()).or_default().insert(
            id.clone(),
            MemIdentity {
                handle: handle.to_owned(),
                kind,
                edits: None,
                live: true,
                owner: None,
                role: None,
                identity_role: None,
            },
        );
        Ok(())
    }

    /// Records a lane with no owner. Replacing an existing name keeps its version.
    #[cfg(test)]
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Stored`] when `name` is not a lane name or the
    /// lane collection is full.
    pub(crate) async fn seed_lane(
        &self,
        project: &ProjectId,
        name: &str,
        globs: &[&str],
    ) -> Result<(), StoreError> {
        fail(&self.failure, "seed_lane")?;
        let name: LaneName = name
            .parse()
            .map_err(|err: agmen_proto::ProtoError| StoreError::Stored(err.to_string()))?;
        let globs: Vec<String> = globs.iter().map(|glob| (*glob).to_owned()).collect();
        let mut state = self.locked().await?;
        if let Some(existing) = state
            .lanes
            .get_mut(project)
            .and_then(|lanes| lanes.iter_mut().find(|lane| lane.name == name))
        {
            existing.globs = globs;
            return Ok(());
        }
        if lane_rows(&state) >= MEM_MAX_ROWS {
            return Err(full());
        }
        state
            .lanes
            .entry(project.clone())
            .or_default()
            .push(MemLane {
                name,
                globs,
                owner: None,
                v: 1,
            });
        Ok(())
    }

    /// Records a holder. Order is claim order: the first seeded holder is oldest.
    #[cfg(test)]
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Stored`] when the holder collection is full.
    pub(crate) async fn seed_holder(
        &self,
        project: &ProjectId,
        holder: LaneHolder,
    ) -> Result<(), StoreError> {
        fail(&self.failure, "seed_holder")?;
        let mut state = self.locked().await?;
        if holder_rows(&state) >= MEM_MAX_ROWS {
            return Err(full());
        }
        state
            .holders
            .entry(project.clone())
            .or_default()
            .push(holder);
        Ok(())
    }

    async fn locked(&self) -> Result<MutexGuard<'_, MemState>, StoreError> {
        tokio::time::timeout(MEM_TX_WAIT, self.state.lock())
            .await
            .map_err(|source| StoreError::Backend(Box::new(source)))
    }
}

impl Reads for &MemStore {
    async fn roster(&mut self, project: &ProjectId) -> Result<Roster, StoreError> {
        fail(&self.failure, "roster")?;
        let state = (*self).locked().await?;
        Ok(roster_of(&state, project))
    }

    async fn ref_targets(
        &mut self,
        project: &ProjectId,
        candidates: &[String],
    ) -> Result<(Vec<String>, Vec<ItemKey>), StoreError> {
        fail(&self.failure, "ref_targets")?;
        let state = (*self).locked().await?;
        Ok(ref_targets_of(&state, project, candidates))
    }

    async fn lane_globs(
        &mut self,
        project: &ProjectId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        fail(&self.failure, "lane_globs")?;
        let state = (*self).locked().await?;
        Ok(lane_globs_of(&state, project))
    }

    async fn lane_holders(&mut self, project: &ProjectId) -> Result<Vec<LaneHolder>, StoreError> {
        fail(&self.failure, "lane_holders")?;
        let state = (*self).locked().await?;
        holders_of(
            &state,
            project,
            *self.clock.lock().unwrap_or_else(PoisonError::into_inner),
        )
    }

    async fn live_edit_scope(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<Option<(IdentityKind, Option<serde_json::Value>)>, StoreError> {
        fail(&self.failure, "live_edit_scope")?;
        let state = (*self).locked().await?;
        Ok(live_scope_of(&state, project, id))
    }

    async fn lends_to(
        &mut self,
        project: &ProjectId,
        to: &IdentityId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        fail(&self.failure, "lends_to")?;
        let state = (*self).locked().await?;
        Ok(lends_of(
            &state,
            project,
            to,
            *self.clock.lock().unwrap_or_else(PoisonError::into_inner),
        ))
    }

    async fn lane_list(&mut self, project: &ProjectId) -> Result<Vec<LaneRow>, StoreError> {
        fail(&self.failure, "lane_list")?;
        let state = (*self).locked().await?;
        Ok(lane_list_of(&state, project))
    }
}

impl Store for MemStore {
    type Reader<'a> = &'a Self;
    type Tx<'a> = MemTx;

    fn reader(&self) -> Self::Reader<'_> {
        self
    }

    async fn begin(&self) -> Result<Self::Tx<'_>, StoreError> {
        fail(&self.failure, "begin")?;
        let guard = tokio::time::timeout(MEM_TX_WAIT, Arc::clone(&self.state).lock_owned())
            .await
            .map_err(|source| StoreError::Backend(Box::new(source)))?;
        let working = guard.clone();
        Ok(MemTx {
            guard,
            working,
            now: *self.clock.lock().unwrap_or_else(PoisonError::into_inner),
            failure: Arc::clone(&self.failure),
        })
    }
}

/// One in-memory transaction, rolled back unless committed.
#[derive(Debug)]
pub struct MemTx {
    guard: OwnedMutexGuard<MemState>,
    working: MemState,
    now: Timestamp,
    failure: Arc<StdMutex<Option<&'static str>>>,
}

impl MemTx {
    fn head(&mut self, key: &str) -> Result<&mut Seq, StoreError> {
        if self.working.heads.len() >= MEM_MAX_ROWS && !self.working.heads.contains_key(key) {
            return Err(full());
        }
        Ok(self.working.heads.entry(key.to_owned()).or_default())
    }
}

impl Reads for MemTx {
    async fn roster(&mut self, project: &ProjectId) -> Result<Roster, StoreError> {
        fail(&self.failure, "roster")?;
        Ok(roster_of(&self.working, project))
    }

    async fn ref_targets(
        &mut self,
        project: &ProjectId,
        candidates: &[String],
    ) -> Result<(Vec<String>, Vec<ItemKey>), StoreError> {
        fail(&self.failure, "ref_targets")?;
        Ok(ref_targets_of(&self.working, project, candidates))
    }

    async fn lane_globs(
        &mut self,
        project: &ProjectId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        fail(&self.failure, "lane_globs")?;
        Ok(lane_globs_of(&self.working, project))
    }

    async fn lane_holders(&mut self, project: &ProjectId) -> Result<Vec<LaneHolder>, StoreError> {
        fail(&self.failure, "lane_holders")?;
        holders_of(&self.working, project, self.now)
    }

    async fn live_edit_scope(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<Option<(IdentityKind, Option<serde_json::Value>)>, StoreError> {
        fail(&self.failure, "live_edit_scope")?;
        Ok(live_scope_of(&self.working, project, id))
    }

    async fn lends_to(
        &mut self,
        project: &ProjectId,
        to: &IdentityId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        fail(&self.failure, "lends_to")?;
        Ok(lends_of(&self.working, project, to, self.now))
    }

    async fn lane_list(&mut self, project: &ProjectId) -> Result<Vec<LaneRow>, StoreError> {
        fail(&self.failure, "lane_list")?;
        Ok(lane_list_of(&self.working, project))
    }
}

impl StoreTx for MemTx {
    async fn lock_item(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> Result<Option<ItemLock>, StoreError> {
        fail(&self.failure, "lock_item")?;
        Ok(self
            .working
            .items
            .get(project)
            .and_then(|items| items.get(item))
            .map(|item| {
                let owner = item.lane.as_ref().and_then(|name| {
                    self.working
                        .lanes
                        .get(project)?
                        .iter()
                        .find(|lane| lane.name == *name)?
                        .owner
                        .as_ref()
                });
                (
                    item.state.clone(),
                    item.lane.as_ref().map(ToString::to_string),
                    item.assigned_to.as_ref().map(ToString::to_string),
                    owner.map(ToString::to_string),
                )
            }))
    }

    async fn release_lapsed(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> Result<Option<Lapsed>, StoreError> {
        fail(&self.failure, "release_lapsed")?;
        let Some(claim) = self.working.claims.iter_mut().find(|claim| {
            claim.project == *project
                && claim.item == *item
                && !claim.released
                && claim.lease_until <= self.now
        }) else {
            return Ok(None);
        };
        let Some(identity) = self
            .working
            .identities
            .get(project)
            .and_then(|ids| ids.get(&claim.holder))
        else {
            return Ok(None);
        };
        claim.released = true;
        Ok(Some(Lapsed {
            holder: claim.holder.clone(),
            handle: identity.handle.clone(),
        }))
    }

    async fn active_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
    ) -> Result<Option<(IdentityId, String)>, StoreError> {
        fail(&self.failure, "active_claim")?;
        Ok(self
            .working
            .claims
            .iter()
            .find(|claim| claim.project == *project && claim.item == *item && !claim.released)
            .and_then(|claim| {
                let identity = self.working.identities.get(project)?.get(&claim.holder)?;
                Some((claim.holder.clone(), identity.handle.clone()))
            }))
    }

    async fn extend_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        lease: Duration,
    ) -> Result<Timestamp, StoreError> {
        fail(&self.failure, "extend_claim")?;
        let until = lease_until(self.now, lease)?;
        let claim = self
            .working
            .claims
            .iter_mut()
            .find(|claim| claim.project == *project && claim.item == *item && !claim.released)
            .ok_or_else(|| StoreError::Stored("no unreleased claim".into()))?;
        claim.lease_until = until;
        millisecond_reply(until)
    }

    async fn insert_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        from: &IdentityId,
        lease: Duration,
    ) -> Result<Timestamp, StoreError> {
        fail(&self.failure, "insert_claim")?;
        if self
            .working
            .claims
            .iter()
            .any(|claim| claim.project == *project && claim.item == *item && !claim.released)
        {
            return Err(StoreError::Duplicate);
        }
        if self.working.claims.len() >= MEM_MAX_ROWS {
            return Err(full());
        }
        let until = lease_until(self.now, lease)?;
        self.working.claims.push(MemClaim {
            project: project.clone(),
            item: item.clone(),
            holder: from.clone(),
            granted_at: self.now,
            lease_until: until,
            released: false,
        });
        millisecond_reply(until)
    }

    async fn renew_claims(
        &mut self,
        project: &ProjectId,
        from: &IdentityId,
        items: &[ItemKey],
        lease: Duration,
    ) -> Result<u64, StoreError> {
        fail(&self.failure, "renew_claims")?;
        let until = lease_until(self.now, lease)?;
        let mut count = 0;
        for claim in self.working.claims.iter_mut().take(MEM_MAX_ROWS) {
            if claim.project == *project
                && claim.holder == *from
                && !claim.released
                && claim.lease_until > self.now
                && items.contains(&claim.item)
            {
                claim.lease_until = until;
                count += 1;
            }
        }
        Ok(count)
    }

    async fn release_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        from: &IdentityId,
        by: ReleaseBy,
    ) -> Result<u64, StoreError> {
        fail(&self.failure, "release_claim")?;
        let mut count = 0;
        for claim in self.working.claims.iter_mut().take(MEM_MAX_ROWS) {
            if claim.project == *project
                && claim.item == *item
                && !claim.released
                && (claim.holder == *from || by == ReleaseBy::Coordinator)
            {
                claim.released = true;
                count += 1;
            }
        }
        Ok(count)
    }

    async fn next_item_number(
        &mut self,
        project: &ProjectId,
        prefix: &str,
    ) -> Result<u64, StoreError> {
        fail(&self.failure, "next_item_number")?;
        let key = (project.clone(), prefix.to_owned());
        if !self.working.item_counters.contains_key(&key)
            && self.working.item_counters.len() >= MEM_MAX_ROWS
        {
            return Err(full());
        }
        let counter = self.working.item_counters.entry(key).or_default();
        *counter = counter
            .checked_add(1)
            .filter(|next| *next <= MAX_ITEM_NUMBER)
            .ok_or_else(|| StoreError::Stored("item counter exhausted".into()))?;
        Ok(*counter)
    }

    async fn insert_item(&mut self, item: &NewItem<'_>) -> Result<(), StoreError> {
        fail(&self.failure, "insert_item")?;
        if self
            .working
            .items
            .get(item.project)
            .is_some_and(|items| items.contains_key(item.key))
        {
            return Err(StoreError::Duplicate);
        }
        if row_count(self.working.items.values().map(BTreeMap::len)) >= MEM_MAX_ROWS {
            return Err(full());
        }
        self.working
            .items
            .entry(item.project.clone())
            .or_default()
            .insert(
                item.key.clone(),
                MemItem {
                    state: "open".into(),
                    lane: item.lane.cloned(),
                    assigned_to: None,
                    title: item.title.to_owned(),
                    severity: item.severity,
                    created_by: item.created_by.clone(),
                },
            );
        Ok(())
    }

    async fn assign_item(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        to: &IdentityId,
    ) -> Result<u64, StoreError> {
        fail(&self.failure, "assign_item")?;
        if !self
            .working
            .identities
            .get(project)
            .and_then(|ids| ids.get(to))
            .is_some_and(|identity| identity.kind != IdentityKind::Human)
        {
            return Ok(0);
        }
        let Some(item) = self
            .working
            .items
            .get_mut(project)
            .and_then(|items| items.get_mut(item))
        else {
            return Ok(0);
        };
        item.assigned_to = Some(to.clone());
        Ok(1)
    }

    async fn close_claim(
        &mut self,
        project: &ProjectId,
        item: &ItemKey,
        holder: &IdentityId,
    ) -> Result<u64, StoreError> {
        fail(&self.failure, "close_claim")?;
        let mut count = 0;
        for claim in self.working.claims.iter_mut().take(MEM_MAX_ROWS) {
            if claim.project == *project
                && claim.item == *item
                && claim.holder == *holder
                && !claim.released
                && claim.lease_until > self.now
            {
                claim.released = true;
                count += 1;
            }
        }
        Ok(count)
    }

    async fn mark_done(&mut self, project: &ProjectId, item: &ItemKey) -> Result<(), StoreError> {
        fail(&self.failure, "mark_done")?;
        if let Some(item) = self
            .working
            .items
            .get_mut(project)
            .and_then(|items| items.get_mut(item))
        {
            item.state = "done".into();
        }
        Ok(())
    }

    async fn lock_lane_owner(
        &mut self,
        project: &ProjectId,
        lane: &LaneName,
    ) -> Result<Option<Option<IdentityId>>, StoreError> {
        fail(&self.failure, "lock_lane_owner")?;
        Ok(self
            .working
            .lanes
            .get(project)
            .and_then(|lanes| lanes.iter().find(|row| row.name == *lane))
            .map(|lane| lane.owner.clone()))
    }

    async fn identity_live(
        &mut self,
        project: &ProjectId,
        id: &IdentityId,
    ) -> Result<bool, StoreError> {
        fail(&self.failure, "identity_live")?;
        Ok(self
            .working
            .identities
            .get(project)
            .and_then(|ids| ids.get(id))
            .is_some_and(|identity| identity.live && identity.kind != IdentityKind::Human))
    }

    async fn identity_edits(
        &mut self,
        id: &IdentityId,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        fail(&self.failure, "identity_edits")?;
        Ok(self
            .working
            .identities
            .values()
            .take(MEM_MAX_ROWS)
            .find_map(|ids| ids.get(id))
            .filter(|identity| identity.kind != IdentityKind::Human)
            .and_then(|identity| identity.edits.clone()))
    }

    async fn active_lends(
        &mut self,
        project: &ProjectId,
        lane: &LaneName,
        to: &IdentityId,
    ) -> Result<u64, StoreError> {
        fail(&self.failure, "active_lends")?;
        Ok(self
            .working
            .lends
            .iter()
            .filter(|lend| {
                lend.project == *project
                    && lend.lane == *lane
                    && lend.to == *to
                    && lend.until > self.now
            })
            .count() as u64)
    }

    async fn insert_lend(&mut self, lend: &NewLend<'_>) -> Result<(), StoreError> {
        fail(&self.failure, "insert_lend")?;
        if self.working.lends.iter().any(|row| {
            row.project == *lend.project
                && row.from == *lend.from
                && row.message_id == *lend.message_id
        }) {
            return Err(StoreError::Duplicate);
        }
        if self.working.lends.len() >= MEM_MAX_ROWS {
            return Err(full());
        }
        self.working.lends.push(MemLend {
            project: lend.project.clone(),
            lane: lend.lane.clone(),
            message_id: lend.message_id.clone(),
            from: lend.from.clone(),
            to: lend.to.clone(),
            paths: lend.paths.to_vec(),
            until: lend.until,
        });
        Ok(())
    }

    async fn insert_message(&mut self, row: &MessageRow<'_>) -> Result<(), StoreError> {
        fail(&self.failure, "insert_message")?;
        if self.working.messages.iter().any(|stored| {
            stored.from == *row.from && stored.id == *row.id && stored.stream == *row.stream
        }) {
            return Err(StoreError::Duplicate);
        }
        if self.working.messages.len() >= MEM_MAX_ROWS {
            return Err(full());
        }
        self.working.messages.push(MemMessage {
            from: row.from.clone(),
            id: row.id.clone(),
            stream: row.stream.clone(),
            project: row.project.clone(),
            seq: row.seq,
            payload_hash: row.payload_hash.to_vec(),
            envelope_json: row.envelope_json.to_owned(),
            hub_at: row.hub_at,
            ack_row: row.ack_row,
        });
        Ok(())
    }

    async fn enqueue_publish(
        &mut self,
        topics: &Topics,
        topic: &str,
        payload: &[u8],
    ) -> Result<(), StoreError> {
        fail(&self.failure, "enqueue_publish")?;
        if self.working.outbox.len() >= MEM_MAX_ROWS {
            return Err(full());
        }
        self.working
            .outbox
            .push((topics.clone(), topic.to_owned(), payload.to_vec()));
        Ok(())
    }

    async fn lock_head(&mut self, key: &str) -> Result<(), StoreError> {
        fail(&self.failure, "lock_head")?;
        self.head(key)?;
        Ok(())
    }

    async fn next_seq(&mut self, key: &str) -> Result<Seq, StoreError> {
        fail(&self.failure, "next_seq")?;
        let head = self.head(key)?;
        head.0 = head
            .0
            .checked_add(1)
            .ok_or_else(|| StoreError::Stored("sequence exhausted".into()))?;
        Ok(*head)
    }

    async fn commit(mut self) -> Result<(), StoreError> {
        fail(&self.failure, "commit")?;
        *self.guard = self.working;
        Ok(())
    }

    async fn replace_lanes(
        &mut self,
        project: &ProjectId,
        lanes: &[ResolvedLane],
    ) -> Result<(), StoreError> {
        fail(&self.failure, "replace_lanes")?;
        replace_project(&mut self.working, project, lanes)
    }

    async fn resolve_owner(
        &mut self,
        project: &ProjectId,
        handle: &str,
    ) -> Result<Option<IdentityId>, StoreError> {
        fail(&self.failure, "resolve_owner")?;
        resolve_handle(&self.working, project, handle)
    }
}

fn fail(failure: &StdMutex<Option<&'static str>>, op: &'static str) -> Result<(), StoreError> {
    let mut failure = failure.lock().unwrap_or_else(PoisonError::into_inner);
    if *failure == Some(op) {
        *failure = None;
        return Err(StoreError::Backend(Box::new(std::io::Error::other(
            format!("injected {op} failure"),
        ))));
    }
    Ok(())
}

fn roster_of(state: &MemState, project: &ProjectId) -> Roster {
    Roster(
        state
            .identities
            .get(project)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter(|(_, id)| id.live)
            .map(|(id, identity)| Member {
                id: id.clone(),
                handle: identity.handle.clone(),
                kind: identity.kind,
                owner: identity.owner.clone(),
                role: identity.role,
                identity_role: identity.identity_role.clone(),
            })
            .collect(),
    )
}

fn ref_targets_of(
    state: &MemState,
    project: &ProjectId,
    candidates: &[String],
) -> (Vec<String>, Vec<ItemKey>) {
    let items = state
        .items
        .get(project)
        .into_iter()
        .flat_map(|items| items.keys())
        .filter(|item| {
            candidates
                .iter()
                .any(|candidate| candidate == item.as_str())
        })
        .cloned()
        .collect();
    (
        lane_globs_of(state, project)
            .into_iter()
            .map(|(name, _)| name.to_string())
            .collect(),
        items,
    )
}

fn full() -> StoreError {
    StoreError::Stored("mem store full".into())
}

/// Stops at [`MEM_MAX_ROWS`] groups or rows, so a count cannot walk an
/// unbounded map. Hitting the stop reports the collection full.
fn row_count(parts: impl Iterator<Item = usize>) -> usize {
    let mut total = 0_usize;
    for (seen, len) in parts.enumerate().take(MEM_MAX_ROWS + 1) {
        if seen == MEM_MAX_ROWS {
            return MEM_MAX_ROWS;
        }
        total = total.saturating_add(len);
        if total >= MEM_MAX_ROWS {
            return MEM_MAX_ROWS;
        }
    }
    total
}

#[cfg(test)]
fn identity_rows(state: &MemState) -> usize {
    row_count(state.identities.values().map(BTreeMap::len))
}

fn lane_rows(state: &MemState) -> usize {
    row_count(state.lanes.values().map(Vec::len))
}

#[cfg(test)]
fn holder_rows(state: &MemState) -> usize {
    row_count(state.holders.values().map(Vec::len))
}

fn lane_globs_of(state: &MemState, project: &ProjectId) -> Vec<(LaneName, Vec<String>)> {
    let mut rows: Vec<(LaneName, Vec<String>)> = match state.lanes.get(project) {
        Some(lanes) => lanes
            .iter()
            .map(|lane| (lane.name.clone(), lane.globs.clone()))
            .collect(),
        None => Vec::new(),
    };
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows
}

fn holders_of(
    state: &MemState,
    project: &ProjectId,
    now: Timestamp,
) -> Result<Vec<LaneHolder>, StoreError> {
    let mut claims: Vec<_> = state
        .claims
        .iter()
        .enumerate()
        .filter(|(_, claim)| {
            claim.project == *project && !claim.released && claim.lease_until > now
        })
        .collect();
    claims.sort_by_key(|(id, claim)| (claim.granted_at, *id));
    let mut holders = state.holders.get(project).cloned().unwrap_or_default();
    let live = claims.into_iter().filter_map(|(_, claim)| {
        let lane = state.items.get(project)?.get(&claim.item)?.lane.clone()?;
        let identity = state.identities.get(project)?.get(&claim.holder)?;
        Some(LaneHolder {
            lane,
            identity: claim.holder.clone(),
            handle: identity.handle.clone(),
        })
    });
    for holder in live.take(MEM_MAX_ROWS) {
        if holders.len() >= MEM_MAX_ROWS {
            return Err(StoreError::Stored(format!(
                "more than {MEM_MAX_ROWS} live lane claims"
            )));
        }
        holders.push(holder);
    }
    Ok(holders)
}

fn live_scope_of(
    state: &MemState,
    project: &ProjectId,
    id: &IdentityId,
) -> Option<(IdentityKind, Option<serde_json::Value>)> {
    state
        .identities
        .get(project)
        .and_then(|ids| ids.get(id))
        .filter(|identity| identity.live)
        .map(|identity| (identity.kind, identity.edits.clone()))
}

fn lends_of(
    state: &MemState,
    project: &ProjectId,
    to: &IdentityId,
    now: Timestamp,
) -> Vec<(LaneName, Vec<String>)> {
    let mut lends: Vec<_> = state
        .lends
        .iter()
        .filter(|lend| {
            if lend.project != *project || lend.to != *to || lend.until <= now {
                return false;
            }
            let owner = state
                .lanes
                .get(project)
                .and_then(|lanes| lanes.iter().find(|lane| lane.name == lend.lane));
            let from = state
                .identities
                .get(project)
                .and_then(|ids| ids.get(&lend.from));
            matches!((owner, from), (Some(lane), Some(from)) if from.live &&
            (lane.owner.as_ref() == Some(&lend.from) || from.kind == IdentityKind::Coordinator))
        })
        .collect();
    lends.sort_by(|a, b| (&a.lane, a.until, &a.message_id).cmp(&(&b.lane, b.until, &b.message_id)));
    lends
        .into_iter()
        .take(crate::MAX_LOADED_LENDS as usize)
        .map(|lend| (lend.lane.clone(), lend.paths.clone()))
        .collect()
}

fn lane_list_of(state: &MemState, project: &ProjectId) -> Vec<LaneRow> {
    let Some(lanes) = state.lanes.get(project) else {
        return Vec::new();
    };
    let mut rows: Vec<LaneRow> = lanes
        .iter()
        .map(|lane| {
            let (owner, owner_handle) = owner_columns(state, project, &lane.owner);
            LaneRow {
                name: lane.name.to_string(),
                globs: lane.globs.clone(),
                owner,
                owner_handle,
                v: lane.v,
            }
        })
        .collect();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

fn owner_columns(
    state: &MemState,
    project: &ProjectId,
    owner: &Option<IdentityId>,
) -> (Option<String>, Option<String>) {
    let Some(owner) = owner else {
        return (None, None);
    };
    let handle = state
        .identities
        .get(project)
        .and_then(|ids| ids.get(owner))
        .map(|identity| identity.handle.clone());
    (Some(owner.as_str().to_owned()), handle)
}

fn replace_project(
    state: &mut MemState,
    project: &ProjectId,
    lanes: &[ResolvedLane],
) -> Result<(), StoreError> {
    let old_len = state.lanes.get(project).map(Vec::len).unwrap_or(0);
    let next = lane_rows(state)
        .saturating_sub(old_len)
        .saturating_add(lanes.len());
    if next > MEM_MAX_ROWS {
        return Err(full());
    }
    // Postgres deletes every lane before insertion, cascading its lends.
    state.lends.retain(|lend| lend.project != *project);
    if lanes.is_empty() {
        state.lanes.remove(project);
        return Ok(());
    }
    let old_max = state
        .lanes
        .get(project)
        .and_then(|rows| rows.iter().map(|row| row.v).max())
        .unwrap_or(0);
    let v = old_max
        .checked_add(1)
        .ok_or_else(|| StoreError::Stored("lane version exhausted".into()))?;
    let rows = lanes
        .iter()
        .map(|lane| MemLane {
            name: lane.name.clone(),
            globs: lane.globs.clone(),
            owner: lane.owner.clone(),
            v,
        })
        .collect();
    state.lanes.insert(project.clone(), rows);
    Ok(())
}

fn resolve_handle(
    state: &MemState,
    project: &ProjectId,
    handle: &str,
) -> Result<Option<IdentityId>, StoreError> {
    let Some(ids) = state.identities.get(project) else {
        return Ok(None);
    };
    let mut found = None;
    for identity_id in ids.keys().take(MEM_MAX_ROWS) {
        let Some(identity) = ids.get(identity_id) else {
            continue;
        };
        if !identity.live {
            continue;
        }
        if identity_id.as_str() == handle || identity.handle == handle {
            if found.is_some() {
                return Err(StoreError::Stored(
                    "owner handle matches more than one identity".into(),
                ));
            }
            found = Some(identity_id.clone());
        }
    }
    Ok(found)
}

fn lease_until(now: Timestamp, lease: Duration) -> Result<Timestamp, StoreError> {
    let ms = i64::try_from(lease.as_millis()).unwrap_or(i64::MAX);
    now.checked_add(Duration::from_millis(ms.unsigned_abs()))
        .map_err(|err| StoreError::Stored(err.to_string()))
}

/// Matches Postgres's numeric-to-bigint rounding of epoch milliseconds.
fn millisecond_reply(until: Timestamp) -> Result<Timestamp, StoreError> {
    const NANOS_PER_MILLISECOND: i128 = 1_000_000;
    let ns = until.as_nanosecond();
    let ms = (ns + ns.signum() * (NANOS_PER_MILLISECOND / 2)) / NANOS_PER_MILLISECOND;
    let ms = i64::try_from(ms).map_err(|err| StoreError::Stored(err.to_string()))?;
    Timestamp::from_millisecond(ms).map_err(|err| StoreError::Stored(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::tests::{LEASE, item, message, setup};
    use agmen_proto::Payload;

    #[tokio::test]
    async fn lease_deadlines_keep_clock_precision() {
        for operation in ["insert_claim", "extend_claim", "renew_claims"] {
            let (store, topics) = setup().await;
            item(&store, &topics, false).await;
            let now: Timestamp = "2026-10-29T12:00:00.000900Z".parse().unwrap();
            store.set_now(now);
            let mut tx = store.begin().await.unwrap();
            let key = "B-1".parse().unwrap();
            let from = "aaaaaaaa".parse().unwrap();
            let mut reply = tx
                .insert_claim(topics.project(), &key, &from, LEASE)
                .await
                .unwrap();
            match operation {
                "extend_claim" => {
                    reply = tx
                        .extend_claim(topics.project(), &key, LEASE)
                        .await
                        .unwrap()
                }
                "renew_claims" => assert_eq!(
                    tx.renew_claims(topics.project(), &from, std::slice::from_ref(&key), LEASE)
                        .await
                        .unwrap(),
                    1
                ),
                _ => {}
            }
            assert_eq!(reply, "2026-10-29T12:01:00.001Z".parse().unwrap());
            tx.commit().await.unwrap();
            store.set_now(now + LEASE - Duration::from_micros(1));
            let mut tx = store.begin().await.unwrap();
            assert!(
                tx.release_lapsed(topics.project(), &key)
                    .await
                    .unwrap()
                    .is_none(),
                "{operation}"
            );
            drop(tx);
            store.set_now(now + LEASE);
            let mut tx = store.begin().await.unwrap();
            assert!(
                tx.release_lapsed(topics.project(), &key)
                    .await
                    .unwrap()
                    .is_some(),
                "{operation}"
            );
        }
    }

    #[tokio::test]
    async fn merged_lane_holders_are_capped() {
        let (store, topics) = setup().await;
        item(&store, &topics, false).await;
        let mut tx = store.begin().await.unwrap();
        tx.insert_claim(
            topics.project(),
            &"B-1".parse().unwrap(),
            &"aaaaaaaa".parse().unwrap(),
            LEASE,
        )
        .await
        .unwrap();
        let holder = LaneHolder {
            lane: "L-UI".parse().unwrap(),
            identity: "bbbbbbbb".parse().unwrap(),
            handle: "b".into(),
        };
        tx.working.holders.insert(
            topics.project().clone(),
            vec![holder.clone(); MEM_MAX_ROWS - 1],
        );
        assert_eq!(
            tx.lane_holders(topics.project()).await.unwrap().len(),
            MEM_MAX_ROWS
        );
        tx.working
            .holders
            .get_mut(topics.project())
            .unwrap()
            .push(holder);
        assert!(matches!(tx.lane_holders(topics.project()).await,
            Err(StoreError::Stored(message)) if message == "more than 100000 live lane claims"));
    }

    #[tokio::test]
    async fn lend_key_includes_sender() {
        let (store, topics) = setup().await;
        item(&store, &topics, true).await;
        let mut tx = store.begin().await.unwrap();
        let lane = "L-UI".parse().unwrap();
        let id = crate::new_message_id(Timestamp::now()).unwrap();
        for from in ["oooooooo", "cccccccc"] {
            tx.insert_lend(&NewLend {
                project: topics.project(),
                lane: &lane,
                message_id: &id,
                from: &from.parse().unwrap(),
                to: &"bbbbbbbb".parse().unwrap(),
                paths: &["ui/**".into()],
                until: Timestamp::now() + LEASE,
            })
            .await
            .unwrap();
        }
        assert_eq!(
            tx.active_lends(topics.project(), &lane, &"bbbbbbbb".parse().unwrap())
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn replacing_lanes_invalidates_lends() {
        let (store, topics) = setup().await;
        item(&store, &topics, true).await;
        let msg = message(
            "oooooooo",
            "o",
            Payload::Lend {
                lane: "L-UI".parse().unwrap(),
                paths: vec!["ui/**".into()],
                to: "bbbbbbbb".parse().unwrap(),
                until: Timestamp::now() + LEASE,
            },
        );
        let mut tx = store.begin().await.unwrap();
        crate::lend(&mut tx, &topics, &msg.from, msg.from_kind, &msg.env)
            .await
            .unwrap();
        assert_eq!(
            tx.lends_to(topics.project(), &"bbbbbbbb".parse().unwrap())
                .await
                .unwrap()
                .len(),
            1
        );
        tx.commit().await.unwrap();
        let mut tx = store.begin().await.unwrap();
        tx.replace_lanes(
            topics.project(),
            &[ResolvedLane {
                name: "L-UI".parse().unwrap(),
                globs: vec!["ui/**".into()],
                owner: Some("oooooooo".parse().unwrap()),
            }],
        )
        .await
        .unwrap();
        assert!(
            tx.lends_to(topics.project(), &"bbbbbbbb".parse().unwrap())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn item_counter_matches_database_ceiling() {
        let (store, topics) = setup().await;
        let mut tx = store.begin().await.unwrap();
        tx.working
            .item_counters
            .insert((topics.project().clone(), "B".into()), 999_998);
        assert_eq!(
            tx.next_item_number(topics.project(), "B").await.unwrap(),
            999_999
        );
        assert!(matches!(
            tx.next_item_number(topics.project(), "B").await,
            Err(StoreError::Stored(_))
        ));
    }
}
