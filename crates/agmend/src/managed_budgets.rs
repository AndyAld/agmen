//! Shared reservations, durable descendant spend and cached safeguards.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use agmen_local::ApiError;
use agmen_local::safeguards::{ChildBudgets, MAX_MANAGED, Safeguards};
use agmen_proto::IdentityId;
use tokio::sync::Mutex;

use super::{Launch, Managed, Record, Worker, actions, error};
use crate::budget::{InvalidCost, Money, Overdrawn, PARENT_FLOOR_USD, Step};
use crate::console::bad_request;

#[derive(Debug)]
struct ReservationStep {
    parent: Arc<Mutex<Worker>>,
    child: Option<Arc<Mutex<Worker>>>,
    amount: Money,
}

/// At most MAX_MANAGED changes, all made under accounting. Roll back on failure.
#[derive(Debug, Default)]
pub(super) struct Reservation(Vec<ReservationStep>);

impl Reservation {
    pub(super) async fn rollback(self) {
        for step in self.0.into_iter().rev() {
            step.parent.lock().await.budget.release(step.amount);
            if let Some(child) = step.child {
                let mut held = child.lock().await;
                held.reserved_from_parent = held.reserved_from_parent.saturating_sub(step.amount);
            }
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct SettingsCache {
    read_at: Option<tokio::time::Instant>,
    pub(super) last_good: Safeguards,
    failure: Option<ApiError>,
    warning: Option<String>,
}

impl SettingsCache {
    pub(super) fn message(&self) -> Option<String> {
        self.failure
            .as_ref()
            .map(|error| error.message.clone())
            .or_else(|| self.warning.clone())
    }
}

impl Managed {
    /// Caller holds launching and accounting until launch completes or refunds this amount.
    // shortcut: the parent's running batch keeps the `--max-budget-usd` it
    // started with, so that batch and the children it just reserved for can
    // spend the same dollars, by at most that batch's cap once per launching
    // level. The next batch sees the reservation. Revisit when a running
    // batch's cap can be lowered.
    pub(super) async fn reserve_child_budget(
        &self,
        launch: &mut Launch,
        parent: Option<&Record>,
        settings: &Safeguards,
    ) -> Result<Option<Reservation>, ApiError> {
        let Some(parent) = parent else {
            return Ok(None);
        };
        let shared = settings.budgets && settings.child_budgets == ChildBudgets::Shared;
        let floor = Money::from_usd(PARENT_FLOOR_USD).map_err(error)?;
        let parent = self
            .workers
            .lock()
            .await
            .get(&parent.identity.identity_id)
            .cloned()
            .ok_or_else(|| bad_request("the parent is no longer managed"))?;
        let held = parent.lock().await;
        let remaining = Money::from_usd(&held.budget.remaining_usd()).map_err(error)?;
        // An agent may pick a smaller cap, never one above what the person allows
        // (ruling 7). This holds with budgets off too: the cap is recorded now
        // and enforced when budgets come back on.
        let allowed = Money::from_usd(&settings.separate_child_usd.to_string()).map_err(error)?;
        let amount = match &launch.budget_usd {
            Some(asked) if shared => Money::from_usd(&asked.to_string()).map_err(error)?,
            Some(asked) => Money::from_usd(&asked.to_string())
                .map_err(error)?
                .min(allowed),
            None if shared => Money::from_usd("5")
                .map_err(error)?
                .min(remaining.saturating_sub(floor)),
            None => allowed,
        };
        launch.budget_usd = Some(amount.usd().parse().map_err(error)?);
        if !shared {
            return Ok(None);
        }
        if amount == Money::ZERO {
            return Err(bad_request(
                Overdrawn {
                    remaining: remaining.usd(),
                }
                .to_string(),
            ));
        }
        drop(held);
        self.reserve_from_parent(parent, amount, floor)
            .await
            .map(Some)
    }

    /// An offline intermediary must hold its live children's caps in its own parent too.
    pub(super) async fn reserve_from_parent(
        &self,
        mut parent: Arc<Mutex<Worker>>,
        mut amount: Money,
        floor: Money,
    ) -> Result<Reservation, ApiError> {
        let mut reservation = Reservation::default();
        let mut child: Option<Arc<Mutex<Worker>>> = None;
        let mut seen = BTreeSet::new();
        for _ in 0..MAX_MANAGED {
            let mut held = parent.lock().await;
            let failure = if !seen.insert(held.record.identity.identity_id.clone()) {
                Some(bad_request("shared budget parent links contain a cycle"))
            } else {
                held.budget.reserve(amount, floor).err().map(|failure| {
                    bad_request(if floor == Money::ZERO {
                        format!(
                            "the parent has {} USD left; add to the parent first",
                            failure.remaining
                        )
                    } else {
                        failure.to_string()
                    })
                })
            };
            if let Some(failure) = failure {
                drop(held);
                reservation.rollback().await;
                return Err(failure);
            }
            let record = held.record.clone();
            let next_amount = if held.state == "offline" {
                held.budget
                    .reserved()
                    .saturating_sub(held.reserved_from_parent)
            } else {
                Money::ZERO
            };
            drop(held);
            if let Some(child) = &child {
                let mut held = child.lock().await;
                held.reserved_from_parent = held.reserved_from_parent.saturating_add(amount);
            }
            reservation.0.push(ReservationStep {
                parent: parent.clone(),
                child,
                amount,
            });
            if next_amount == Money::ZERO || !record.shared_budget {
                return Ok(reservation);
            }
            child = Some(parent);
            parent = match self.shared_parent(&record).await {
                Ok(Some(parent)) => parent,
                _ => {
                    reservation.rollback().await;
                    return Err(bad_request(
                        "the shared budget's parent is no longer managed",
                    ));
                }
            };
            amount = next_amount;
        }
        reservation.rollback().await;
        Err(bad_request("shared budget ancestry exceeds 256 agents"))
    }

    /// Returns free funds through stopped intermediaries while keeping live child caps.
    pub(super) async fn release_to_parent(&self, record: &Record, mut amount: Money) {
        let mut record = record.clone();
        let mut seen = BTreeSet::from([record.identity.identity_id.clone()]);
        for _ in 0..MAX_MANAGED {
            if !record.shared_budget || amount == Money::ZERO {
                break;
            }
            let Some(id) = record
                .parent
                .as_ref()
                .filter(|id| seen.insert((*id).clone()))
            else {
                break;
            };
            let parent = self.workers.lock().await.get(id).cloned();
            let Some(parent) = parent else { break };
            let mut held = parent.lock().await;
            held.budget.release(amount);
            amount = if held.state == "offline" {
                amount.min(
                    held.reserved_from_parent
                        .saturating_sub(held.budget.reserved()),
                )
            } else {
                Money::ZERO
            };
            held.reserved_from_parent = held.reserved_from_parent.saturating_sub(amount);
            record = held.record.clone();
        }
    }

    /// Caller holds accounting; release the worker before walking its ancestors.
    pub(super) async fn record_cost(
        &self,
        mut held: tokio::sync::MutexGuard<'_, Worker>,
        cost: &str,
    ) -> Result<(Record, Vec<Step>), InvalidCost> {
        let before = held.budget.spent();
        let mut steps = Vec::new();
        held.budget.add_cost_usd(cost, |step| steps.push(step))?;
        let spent = held.budget.spent();
        let cost = spent.saturating_sub(before);
        held.reserved_from_parent = held.reserved_from_parent.saturating_sub(cost);
        let children_spent = held.budget.children_spent();
        let record = held.record.clone();
        drop(held);
        // shortcut: the agent's status is saved first, then each ancestor's. A
        // crash between the files leaves an ancestor short by this one cost.
        // Revisit when status moves to one durable store.
        actions::save_status(
            &self.agent_dir(&record.identity.identity_id),
            spent,
            children_spent,
        )
        .await;
        self.charge_ancestors(&record, cost).await;
        Ok((record, steps))
    }

    pub(crate) async fn safeguards(&self) -> Result<Safeguards, ApiError> {
        self.read_safeguards(false).await
    }

    /// Launches force a fresh validation; batches reuse the last read for one second.
    pub(super) async fn read_safeguards(&self, fresh: bool) -> Result<Safeguards, ApiError> {
        let mut cache = self.safeguards_cache.lock().await;
        if fresh
            || cache
                .read_at
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
        {
            let path = self.settings_file.clone();
            let result = tokio::task::spawn_blocking(move || {
                crate::safeguards::load_report(path.as_deref())
            })
            .await
            .map_err(error)
            .and_then(|result| result);
            cache.read_at = Some(tokio::time::Instant::now());
            match result {
                Ok((settings, warning)) => {
                    cache.last_good = settings;
                    cache.warning = warning;
                    cache.failure = None;
                }
                Err(failure) => {
                    tracing::warn!(message = %failure.message, "reading agent safeguards failed");
                    cache.failure = Some(failure);
                }
            }
        }
        match &cache.failure {
            Some(failure) => Err(failure.clone()),
            None => Ok(cache.last_good),
        }
    }

    pub(super) async fn shared_parent(
        &self,
        record: &Record,
    ) -> Result<Option<Arc<Mutex<Worker>>>, ApiError> {
        if !record.shared_budget {
            return Ok(None);
        }
        let workers = self.workers.lock().await;
        let parent = record
            .parent
            .as_ref()
            .and_then(|id| workers.get(id).cloned());
        parent
            .map(Some)
            .ok_or_else(|| bad_request("the shared budget's parent is no longer managed"))
    }

    /// Caller holds accounting; never hold the child and parent worker locks together.
    pub(super) async fn reserve_on_start(
        &self,
        worker: &Arc<Mutex<Worker>>,
    ) -> Result<(), ApiError> {
        let held = worker.lock().await;
        let record = held.record.clone();
        let amount = held
            .budget
            .unspent()
            .saturating_sub(held.reserved_from_parent);
        drop(held);
        // An agent that went offline without a stop (a role that changed) still
        // holds its cap in its parent, so it needs no new funds to start.
        if amount == Money::ZERO {
            return Ok(());
        }
        if let Some(parent) = self.shared_parent(&record).await? {
            let floor = Money::from_usd(PARENT_FLOOR_USD).map_err(error)?;
            self.reserve_from_parent(parent, amount, floor).await?;
            let mut held = worker.lock().await;
            held.reserved_from_parent = held.reserved_from_parent.saturating_add(amount);
        }
        Ok(())
    }

    /// Caller holds accounting. A stopped intermediary still accounts for live children.
    pub(super) async fn charge_ancestors(&self, record: &Record, cost: Money) {
        let mut record = record.clone();
        let mut seen = BTreeSet::from([record.identity.identity_id.clone()]);
        for _ in 0..MAX_MANAGED {
            if !record.shared_budget {
                break;
            }
            let Some(id) = record
                .parent
                .as_ref()
                .filter(|id| seen.insert((*id).clone()))
            else {
                break;
            };
            let parent = self.workers.lock().await.get(id).cloned();
            let Some(parent) = parent else { break };
            let mut held = parent.lock().await;
            held.budget.charge_child(cost);
            held.reserved_from_parent = held.reserved_from_parent.saturating_sub(cost);
            let spent = held.budget.spent();
            let children_spent = held.budget.children_spent();
            record = held.record.clone();
            drop(held);
            actions::save_status(
                &self.agent_dir(&record.identity.identity_id),
                spent,
                children_spent,
            )
            .await;
        }
    }
}

pub(super) fn tree_spend(
    spends: &BTreeMap<IdentityId, Money>,
    tree: &BTreeMap<IdentityId, Option<IdentityId>>,
) -> BTreeMap<IdentityId, Money> {
    let mut totals = spends.clone();
    // Both snapshots have at most MAX_MANAGED entries; cycles count no node twice.
    for (id, cost) in spends {
        let mut seen = BTreeSet::from([id.clone()]);
        let mut current = id;
        for _ in 0..MAX_MANAGED {
            let Some(parent) = tree.get(current).and_then(Option::as_ref) else {
                break;
            };
            if !seen.insert(parent.clone()) {
                break;
            }
            if let Some(total) = totals.get_mut(parent) {
                *total = total.saturating_add(*cost);
            }
            current = parent;
        }
    }
    totals
}
