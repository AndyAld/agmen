//! The mockup's desktop screens, one per `docs/mockups/console/desktop-*.webp`.

use std::str::FromStr;

/// A screen of the mockup, by its render's file stem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Screen {
    AddAgent,
    Agent,
    Ask,
    Chat,
    ChatCodex,
    ChatTerminal,
    Git,
    GitCollapsed,
    KickOff,
    ListenerCollapsed,
    ModelPicker,
    NeedsYou,
    NeedsYouAnswered,
    Palette,
    Plan,
    PlanReady,
    Project,
    ProjectTestsOpen,
    RoleFile,
    Running,
    Settings,
    SettingsApplied,
    SettingsFromChat,
    Spec,
    SwitchAgent,
}

impl Screen {
    pub(crate) const ALL: [Self; 25] = [
        Self::AddAgent,
        Self::Agent,
        Self::Ask,
        Self::Chat,
        Self::ChatCodex,
        Self::ChatTerminal,
        Self::Git,
        Self::GitCollapsed,
        Self::KickOff,
        Self::ListenerCollapsed,
        Self::ModelPicker,
        Self::NeedsYou,
        Self::NeedsYouAnswered,
        Self::Palette,
        Self::Plan,
        Self::PlanReady,
        Self::Project,
        Self::ProjectTestsOpen,
        Self::RoleFile,
        Self::Running,
        Self::Settings,
        Self::SettingsApplied,
        Self::SettingsFromChat,
        Self::Spec,
        Self::SwitchAgent,
    ];

    /// The render's file stem after `desktop-`, such as `needs-you`.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::AddAgent => "add-agent",
            Self::Agent => "agent",
            Self::Ask => "ask",
            Self::Chat => "chat",
            Self::ChatCodex => "chat-codex",
            Self::ChatTerminal => "chat-terminal",
            Self::Git => "git",
            Self::GitCollapsed => "git-collapsed",
            Self::KickOff => "kick-off",
            Self::ListenerCollapsed => "listener-collapsed",
            Self::ModelPicker => "model-picker",
            Self::NeedsYou => "needs-you",
            Self::NeedsYouAnswered => "needs-you-answered",
            Self::Palette => "palette",
            Self::Plan => "plan",
            Self::PlanReady => "plan-ready",
            Self::Project => "project",
            Self::ProjectTestsOpen => "project-tests-open",
            Self::RoleFile => "role-file",
            Self::Running => "running",
            Self::Settings => "settings",
            Self::SettingsApplied => "settings-applied",
            Self::SettingsFromChat => "settings-from-chat",
            Self::Spec => "spec",
            Self::SwitchAgent => "switch-agent",
        }
    }
}

impl FromStr for Screen {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|screen| screen.name() == s)
            .ok_or_else(|| {
                let names: Vec<&str> = Self::ALL.iter().map(|s| s.name()).collect();
                format!("no screen \"{s}\"; screens are {}", names.join(", "))
            })
    }
}
