//! Delegation authorization and caps, without launching or changing state.

// shortcut: L21 wires this policy into the listener runtime and launcher.
#![cfg_attr(not(test), expect(dead_code, reason = "wired by L21"))]

/// Nonnegative USD in whole cents, avoiding floating-point cap comparisons.
/// The boundary converting `budget_usd` must reject negative, nonfinite or
/// unrepresentable amounts. Zero is permitted: the policy specifies no minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub(crate) struct UsdCents(pub(crate) u64);

/// A worker's model tier, mapped to an agent's model by the launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Tier {
    Small,
    Standard,
    Deep,
}

/// Exactly one model selection, resolved and validated by launcher preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Model<'a> {
    Tier(Tier),
    Named(&'a str),
}

/// Policy-relevant part of a delegation, after boundary parsing.
/// Brief, reply-to and worktree fields belong to runtime and launcher checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DelegateRequest<'a> {
    /// None uses the launcher's default agent (Claude in this plan).
    pub(crate) agent: Option<&'a str>,
    /// Role resolved against the project's permitted worker role catalog.
    pub(crate) role: &'a str,
    /// Models and agents do not change delegation caps.
    pub(crate) model: Model<'a>,
    /// Maximum spend reserved for the requested worker.
    pub(crate) budget: UsdCents,
}

/// Effective settings for one person's listener in one project.
/// L21 supplies trusted owner/coordinator identities and resolved worker roles,
/// then replaces the defaults with the project's saved cap overrides.
/// Collections are borrowed; membership scans are bounded by their slice lengths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Settings<'a> {
    pub(crate) person: &'a str,
    pub(crate) coordinators: &'a [&'a str],
    /// Additional accepted senders; cannot remove the person or coordinators.
    pub(crate) accept_from: &'a [&'a str],
    /// Only resolved roles of kind agent, including permitted project roles.
    pub(crate) worker_roles: &'a [&'a str],
    pub(crate) per_delegation: UsdCents,
    pub(crate) daily: UsdCents,
    pub(crate) max_workers: usize,
}

impl<'a> Settings<'a> {
    /// Defaults from roles-listener.md's global constraints: $5, $20, 3 workers.
    /// The listener's own $2 daily cap and default Haiku model are runtime
    /// settings, separate from the workers' delegation policy.
    #[must_use]
    pub(crate) const fn new(
        person: &'a str,
        coordinators: &'a [&'a str],
        worker_roles: &'a [&'a str],
    ) -> Self {
        Self {
            person,
            coordinators,
            accept_from: &[],
            worker_roles,
            per_delegation: UsdCents(500),
            daily: UsdCents(2_000),
            max_workers: 3,
        }
    }
}

/// A cap that requires the listener's person to approve the delegation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingReason {
    PerDelegation,
    Daily,
    MaxWorkers,
}

/// A request that cannot be authorized by raising spend caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefuseReason {
    Requester,
    Role,
}

/// Policy result only; Allow still requires the launcher's preflight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    Allow,
    Pending(PendingReason),
    Refuse(RefuseReason),
}

/// Checks the hub-stamped sender and resolved role before checking caps.
/// `msg_from` comes from the reply-to message, never the model's claimed sender.
/// `today_spend` is today's worker spend plus outstanding budget reservations;
/// callers must serialize this check with reserving the new budget and slot.
/// `running` counts all workers sharing the person's project cap, including idle
/// workers. Exact dollar caps are allowed; an already full worker cap is pending.
/// If several caps fail, reports per-delegation, daily, then workers in that order.
/// No I/O, mutation, launch or approval occurs here.
#[must_use]
pub(crate) fn check_delegate(
    req: &DelegateRequest<'_>,
    msg_from: &str,
    settings: &Settings<'_>,
    today_spend: UsdCents,
    running: usize,
) -> Decision {
    if msg_from != settings.person
        && !settings.coordinators.contains(&msg_from)
        && !settings.accept_from.contains(&msg_from)
    {
        return Decision::Refuse(RefuseReason::Requester);
    }
    if !settings.worker_roles.contains(&req.role) {
        return Decision::Refuse(RefuseReason::Role);
    }
    if req.budget > settings.per_delegation {
        return Decision::Pending(PendingReason::PerDelegation);
    }
    if today_spend
        .0
        .checked_add(req.budget.0)
        .is_none_or(|total| total > settings.daily.0)
    {
        return Decision::Pending(PendingReason::Daily);
    }
    if running >= settings.max_workers {
        return Decision::Pending(PendingReason::MaxWorkers);
    }
    Decision::Allow
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
