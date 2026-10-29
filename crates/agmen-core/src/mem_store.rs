//! Bounded, serializable test storage with rollback on drop.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::Duration;

use agmen_proto::{IdentityId, IdentityKind, LaneName, ProjectId, Seq, Timestamp};
use tokio::sync::{Mutex, MutexGuard, OwnedMutexGuard};

use crate::lanes::{LaneHolder, LaneRow, ResolvedLane};
use crate::{Reads, Store, StoreError, StoreTx};

/// The maximum rows in each in-memory collection.
pub const MEM_MAX_ROWS: usize = 100_000;

/// A forgotten transaction must fail a waiting test instead of hanging it.
const MEM_TX_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
struct MemIdentity {
    handle: String,
    kind: IdentityKind,
    edits: Option<serde_json::Value>,
    live: bool,
}

#[derive(Debug, Clone)]
struct MemLane {
    name: LaneName,
    globs: Vec<String>,
    owner: Option<IdentityId>,
    v: i64,
}

type MemLendRow = (LaneName, Vec<String>);
type MemLendsTo = BTreeMap<IdentityId, Vec<MemLendRow>>;

#[derive(Debug, Clone, Default)]
struct MemState {
    heads: BTreeMap<String, Seq>,
    identities: BTreeMap<ProjectId, BTreeMap<IdentityId, MemIdentity>>,
    lanes: BTreeMap<ProjectId, Vec<MemLane>>,
    holders: BTreeMap<ProjectId, Vec<LaneHolder>>,
    lends: BTreeMap<ProjectId, MemLendsTo>,
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
        Ok(holders_of(&state, project))
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
        Ok(lends_of(&state, project, to))
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
            failure: Arc::clone(&self.failure),
        })
    }
}

/// One in-memory transaction, rolled back unless committed.
#[derive(Debug)]
pub struct MemTx {
    guard: OwnedMutexGuard<MemState>,
    working: MemState,
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
    async fn lane_globs(
        &mut self,
        project: &ProjectId,
    ) -> Result<Vec<(LaneName, Vec<String>)>, StoreError> {
        fail(&self.failure, "lane_globs")?;
        Ok(lane_globs_of(&self.working, project))
    }

    async fn lane_holders(&mut self, project: &ProjectId) -> Result<Vec<LaneHolder>, StoreError> {
        fail(&self.failure, "lane_holders")?;
        Ok(holders_of(&self.working, project))
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
        Ok(lends_of(&self.working, project, to))
    }

    async fn lane_list(&mut self, project: &ProjectId) -> Result<Vec<LaneRow>, StoreError> {
        fail(&self.failure, "lane_list")?;
        Ok(lane_list_of(&self.working, project))
    }
}

impl StoreTx for MemTx {
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

fn holders_of(state: &MemState, project: &ProjectId) -> Vec<LaneHolder> {
    state.holders.get(project).cloned().unwrap_or_default()
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
) -> Vec<(LaneName, Vec<String>)> {
    state
        .lends
        .get(project)
        .and_then(|by_to| by_to.get(to))
        .cloned()
        .unwrap_or_default()
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
