//! The coding agents installed on this machine and the models the composer's
//! picker offers from them (plan Task 15, spec 4 "Model and effort per
//! message").

use std::fmt;

/// How hard a model thinks, for agents that take a level (spec 4: Claude
/// Code `--effort` low to max, `agy --effort` low to high).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    /// The word each agent's `--effort` takes, which the picker shows.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How long a model choice lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum ChoiceScope {
    /// The next message only; the chip goes back after it is sent.
    #[default]
    ThisMessage,
    /// Every message until the person picks again.
    UntilChanged,
}

/// The model a message runs on: the agent, its model (`None` is the
/// agent's configured model) and the effort when the agent takes one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ModelChoice {
    pub(crate) agent: String,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<Effort>,
    pub(crate) scope: ChoiceScope,
}

impl ModelChoice {
    /// `agent`'s configured model, as the listener runs it by default.
    #[must_use]
    pub(crate) fn configured(agent: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            model: None,
            effort: None,
            scope: ChoiceScope::ThisMessage,
        }
    }

    /// The chip's and the message tag's text: "codex · default" for an
    /// agent's configured model, "agy · high" for one with an effort,
    /// "opus · high" for a model with an effort, and the model alone for
    /// one without.
    #[must_use]
    pub(crate) fn label(&self) -> String {
        match (&self.model, self.effort) {
            (None, None) => format!("{} · default", self.agent),
            (None, Some(effort)) => format!("{} · {effort}", self.agent),
            (Some(model), Some(effort)) => format!("{model} · {effort}"),
            (Some(model), None) => model.clone(),
        }
    }

    /// Whether both name the same model and effort, whatever their scope.
    #[must_use]
    pub(crate) fn same_model(&self, other: &Self) -> bool {
        self.agent == other.agent && self.model == other.model && self.effort == other.effort
    }
}

/// An agent `agmen agents scan` found, or one agmen knows how to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledAgent {
    /// The command, such as `claude`.
    pub(crate) name: String,
    /// What `--version` printed; `None` when it is not on PATH.
    pub(crate) version: Option<String>,
    /// Why it cannot be used as is, such as "update to 1.1.15+ for JSON".
    pub(crate) note: Option<String>,
}

/// The models one installed agent offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelGroup {
    pub(crate) agent: String,
    /// Its models by name; empty when it lists none, so the picker offers
    /// only its configured model.
    pub(crate) models: Vec<String>,
    /// The levels its `--effort` takes, lowest first; empty when it takes
    /// a model only.
    pub(crate) efforts: Vec<Effort>,
}

impl ModelGroup {
    /// The level a model of this agent starts at: high, as the mockup's
    /// picker shows, or the agent's highest when it stops below high.
    #[must_use]
    pub(crate) fn default_effort(&self) -> Option<Effort> {
        if self.efforts.contains(&Effort::High) {
            Some(Effort::High)
        } else {
            self.efforts.last().copied()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_the_model_and_effort() {
        assert_eq!(ModelChoice::configured("codex").label(), "codex · default");
        // A configured model with a level names the level, as "agy · high".
        let agy = ModelChoice {
            effort: Some(Effort::High),
            ..ModelChoice::configured("agy")
        };
        assert_eq!(agy.label(), "agy · high");
        let opus = ModelChoice {
            agent: "claude".into(),
            model: Some("opus".into()),
            effort: Some(Effort::High),
            scope: ChoiceScope::ThisMessage,
        };
        assert_eq!(opus.label(), "opus · high");
        let plain = ModelChoice {
            effort: None,
            ..opus.clone()
        };
        assert_eq!(plain.label(), "opus");
        let sticky = ModelChoice {
            scope: ChoiceScope::UntilChanged,
            ..opus.clone()
        };
        assert!(sticky.same_model(&opus));
        assert!(!plain.same_model(&opus));
    }

    #[test]
    fn effort_starts_high_or_at_the_top() {
        let group = |efforts: Vec<Effort>| ModelGroup {
            agent: "a".into(),
            models: Vec::new(),
            efforts,
        };
        let claude = group(vec![
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::XHigh,
            Effort::Max,
        ]);
        assert_eq!(claude.default_effort(), Some(Effort::High));
        assert_eq!(
            group(vec![Effort::Low, Effort::Medium]).default_effort(),
            Some(Effort::Medium)
        );
        assert_eq!(group(Vec::new()).default_effort(), None);
    }
}
