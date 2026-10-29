//! Agent safeguard settings shared by agmend and the desktop.

use serde::{Deserialize, Serialize};

/// The settings table for agent safeguards.
pub const TABLE: &str = "agents";

/// The hard bound on managed agents, even when configurable limits are off.
pub const MAX_MANAGED: usize = 256;

/// The largest configurable depth or agent count.
pub const MAX_LIMIT: u32 = 256;

const _: () = assert!(MAX_LIMIT as usize == MAX_MANAGED);

/// Warning shown while the tree depth limit is off.
pub const DEPTH_OFF_WARNING: &str = "Agents can launch agents without limit. A looping agent can start processes until the machine or your API rate limit runs out. agmend still stops at 256 agents.";

/// Warning shown while the agent count limit is off.
pub const AGENTS_OFF_WARNING: &str = "Agents can launch any number of agents across all trees. A looping agent can start processes until the machine or your API rate limit runs out. agmend still stops at 256 agents.";

/// Warning shown while spending caps are off.
pub const BUDGETS_OFF_WARNING: &str = "No spending caps. Agents spend until you stop them.";

/// Whether a child draws its budget from its parent or gets a separate cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChildBudgets {
    /// Reserve the child's cap from the parent's remaining budget.
    Shared,
    /// Give each child the configured separate cap.
    Separate,
}

/// Per-person agent limits and budgets from the `[agents]` settings table.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Safeguards {
    /// Enforce the configured tree depth.
    pub limit_tree_depth: bool,
    /// Maximum tree depth, counting the person's launch as level one.
    pub max_tree_depth: u32,
    /// Enforce the configured agent count across all trees.
    pub limit_agents: bool,
    /// Maximum agents running for the person.
    pub max_agents: u32,
    /// Enforce spending caps.
    pub budgets: bool,
    /// How children receive their spending caps.
    pub child_budgets: ChildBudgets,
    /// Each child's cap in USD when child budgets are separate.
    pub separate_child_usd: f64,
}

impl Default for Safeguards {
    fn default() -> Self {
        Self {
            limit_tree_depth: true,
            max_tree_depth: 4,
            limit_agents: true,
            max_agents: 16,
            budgets: true,
            child_budgets: ChildBudgets::Shared,
            separate_child_usd: 5.0,
        }
    }
}

impl Safeguards {
    /// Checks stored values even when their corresponding limits are off.
    ///
    /// ```
    /// use agmen_local::safeguards::Safeguards;
    ///
    /// Safeguards::default().validate()?;
    /// # Ok::<(), agmen_local::safeguards::SafeguardsError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`SafeguardsError`] for an out-of-range limit or a non-finite
    /// or out-of-range separate child budget.
    pub fn validate(&self) -> Result<(), SafeguardsError> {
        for (key, value) in [
            ("max_tree_depth", self.max_tree_depth),
            ("max_agents", self.max_agents),
        ] {
            if !(1..=MAX_LIMIT).contains(&value) {
                return Err(SafeguardsError::OutOfRange { key, value });
            }
        }
        if !self.separate_child_usd.is_finite()
            || !(0.01..=1000.0).contains(&self.separate_child_usd)
        {
            return Err(SafeguardsError::SeparateChildUsd);
        }
        Ok(())
    }
}

/// Why an agent safeguard value is invalid.
#[derive(Debug, thiserror::Error)]
pub enum SafeguardsError {
    /// A depth or count lies outside the supported range.
    #[error("{key} must be 1 to 256, not {value}")]
    OutOfRange {
        /// The setting that failed validation.
        key: &'static str,
        /// The rejected value.
        value: u32,
    },
    /// A separate child budget is non-finite or outside the supported range.
    #[error("separate_child_usd must be 0.01 to 1000")]
    SeparateChildUsd,
}

#[cfg(test)]
#[path = "safeguards_tests.rs"]
mod tests;
