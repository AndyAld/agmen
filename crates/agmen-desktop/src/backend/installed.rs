//! The terminal agents installed on this machine, which the palette offers
//! workers on and Task 14's switch menu lists.

/// Whether an agent can run workers here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallState {
    Ready,
    /// Installed, but agmen needs a newer version; the note says which.
    Outdated(String),
    Missing,
}

/// One terminal agent agmen knows how to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledAgent {
    /// The command, such as `claude`.
    pub(crate) name: String,
    /// The installed version, `None` when missing.
    pub(crate) version: Option<String>,
    pub(crate) state: InstallState,
}

impl InstalledAgent {
    /// Whether the agent is on this machine at all.
    pub(crate) fn is_installed(&self) -> bool {
        self.state != InstallState::Missing
    }
}

/// console.py `INSTALLED`, in its order.
pub(super) fn fixture() -> Vec<InstalledAgent> {
    let agent = |name: &str, version: Option<&str>, state| InstalledAgent {
        name: name.to_owned(),
        version: version.map(str::to_owned),
        state,
    };
    vec![
        agent("claude", Some("2.1.310"), InstallState::Ready),
        agent("codex", Some("0.164.0"), InstallState::Ready),
        agent("opencode", Some("2.1.4"), InstallState::Ready),
        agent("gemini", Some("0.46.0"), InstallState::Ready),
        agent(
            "agy",
            Some("1.1.7"),
            InstallState::Outdated("update to 1.1.15+ for JSON".into()),
        ),
        agent("grok", None, InstallState::Missing),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_installed_agents_match_mockup() {
        let agents = fixture();
        let names: Vec<_> = agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            ["claude", "codex", "opencode", "gemini", "agy", "grok"]
        );
        let installed: Vec<_> = agents.iter().filter(|a| a.is_installed()).collect();
        assert_eq!(installed.len(), 5, "grok is not installed (mockup fix 28)");
        assert!(matches!(agents[4].state, InstallState::Outdated(_)));
    }
}
