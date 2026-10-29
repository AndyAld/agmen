//! Pure settings direction policy for a listener or one of its role/agent targets.

use super::policy::{Settings, UsdCents};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PushApproval {
    Required,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentState {
    Stopped,
    Running,
}

/// Effective before/after values for the same target, resolved by the caller.
/// Role changes supply their resolved tools and lanes, not just a role name.
/// LEND entries identify complete grants (including scope), not mutable ids.
/// Membership scans are bounded by the supplied slice lengths. Parsing,
/// ownership, role ceilings and mandatory coordinator checks belong to L21.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ChangeSettings<'a> {
    pub(super) listener: Settings<'a>,
    pub(super) listener_daily: UsdCents,
    pub(super) budget: UsdCents,
    pub(super) tools: &'a [&'a str],
    pub(super) lanes: &'a [&'a str],
    pub(super) lends: &'a [&'a str],
    pub(super) push_approval: PushApproval,
    pub(super) state: AgentState,
}

/// Widening fields, ordered as in the comparison below, once per field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Field {
    PerDelegation,
    Daily,
    MaxWorkers,
    AcceptFrom,
    WorkerRoles,
    ListenerDaily,
    Budget,
    Tools,
    Lanes,
    Lends,
    PushApproval,
    AgentState,
    // The listener's own model, effort and reviews: judged by the settings
    // store, not by `classify`.
    Model,
    Effort,
    Reviews,
    // A listener setting no one classified: widening until someone does.
    Unclassified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Direction {
    Narrow,
    Widen(Vec<Field>),
}

/// Classifies a proposed change without applying it or granting approval.
/// Equal values are Narrow. Any widening wins over simultaneous tightening.
#[must_use]
pub(super) fn classify(before: &ChangeSettings<'_>, after: &ChangeSettings<'_>) -> Direction {
    let accepted_before = |sender: &str| {
        sender == before.listener.person
            || before.listener.coordinators.contains(&sender)
            || before.listener.accept_from.contains(&sender)
    };
    let accept_from_widens = std::iter::once(after.listener.person)
        .chain(after.listener.coordinators.iter().copied())
        .chain(after.listener.accept_from.iter().copied())
        .any(|sender| !accepted_before(sender));

    // Exactly twelve comparisons bound the result to at most twelve fields.
    // Set scans are bounded by the before/after slice lengths; no I/O occurs.
    let fields = [
        (
            Field::PerDelegation,
            after.listener.per_delegation > before.listener.per_delegation,
        ),
        (Field::Daily, after.listener.daily > before.listener.daily),
        (
            Field::MaxWorkers,
            after.listener.max_workers > before.listener.max_workers,
        ),
        (Field::AcceptFrom, accept_from_widens),
        (
            Field::WorkerRoles,
            adds(before.listener.worker_roles, after.listener.worker_roles),
        ),
        (
            Field::ListenerDaily,
            after.listener_daily > before.listener_daily,
        ),
        (Field::Budget, after.budget > before.budget),
        (Field::Tools, adds(before.tools, after.tools)),
        (Field::Lanes, adds(before.lanes, after.lanes)),
        (Field::Lends, adds(before.lends, after.lends)),
        (
            Field::PushApproval,
            before.push_approval == PushApproval::Required
                && after.push_approval == PushApproval::Disabled,
        ),
        (
            Field::AgentState,
            before.state == AgentState::Stopped && after.state == AgentState::Running,
        ),
    ];
    let widening: Vec<Field> = fields
        .into_iter()
        .filter_map(|(field, widens)| widens.then_some(field))
        .collect();
    if widening.is_empty() {
        Direction::Narrow
    } else {
        Direction::Widen(widening)
    }
}

fn adds(before: &[&str], after: &[&str]) -> bool {
    after.iter().any(|value| !before.contains(value))
}

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
