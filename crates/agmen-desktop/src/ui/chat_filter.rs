//! Which messages a desktop chat view draws.

use std::collections::BTreeSet;

use agmen_local::safeguards::MAX_MANAGED;
use agmen_proto::{IdentityId, IdentityKind, StampedEnvelope};

use crate::backend::Snapshot;

/// A per-view choice that changes rows without changing delivery or alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ChatFilter {
    MyAgents,
    #[default]
    Everyone,
    JustHumans,
}

impl ChatFilter {
    /// The breadcrumb control's choices, in display order.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the breadcrumb control is added in Task 4")
    )]
    pub(crate) const ALL: [Self; 3] = [Self::MyAgents, Self::Everyone, Self::JustHumans];

    /// The label shown for this choice.
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::MyAgents => "My Agents",
            Self::Everyone => "Everyone",
            Self::JustHumans => "Just humans",
        }
    }

    /// Whether this choice includes a message, before ledger visibility.
    #[must_use]
    pub(crate) fn shows(self, env: &StampedEnvelope, mine: &BTreeSet<IdentityId>) -> bool {
        match self {
            Self::Everyone => true,
            Self::JustHumans => env.from_kind == IdentityKind::Human,
            Self::MyAgents => {
                mine.contains(&env.from)
                    || env.client.to.iter().any(|id| mine.contains(id))
                    || env.mentioned.iter().any(|id| mine.contains(id))
            }
        }
    }
}

/// The person's roster agents, including their listener and stopped agents.
#[must_use]
pub(crate) fn my_agents(snapshot: &Snapshot) -> BTreeSet<IdentityId> {
    snapshot
        .roster
        .iter()
        .find(|group| group.you)
        .into_iter()
        .flat_map(|group| group.agents.iter())
        .take(MAX_MANAGED)
        .map(|agent| agent.id.clone())
        .collect()
}

#[cfg(test)]
#[path = "chat_filter_tests.rs"]
mod tests;
