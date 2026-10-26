//! What tabs and breadcrumbs call each item, from the snapshot.

use std::collections::BTreeMap;

use agmen_proto::IdentityId;

use super::item::TabItem;
use crate::backend::Snapshot;
use crate::icons::UiIcon;
use crate::refs::RefTarget;
use crate::ui::components::Color;
use crate::ui::roles_panel::role_file_name;

/// An agent's handle and role, for its tab.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentName {
    handle: String,
    role: String,
}

/// What tabs and breadcrumbs name, from the snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PaneNames {
    project: String,
    agents: BTreeMap<IdentityId, AgentName>,
}

/// The last part of a slash separated path.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl PaneNames {
    pub(super) fn from_snapshot(snapshot: &Snapshot) -> Self {
        let agents = snapshot
            .roster
            .iter()
            .flat_map(|group| &group.agents)
            .map(|agent| {
                (
                    agent.id.clone(),
                    AgentName {
                        handle: agent.handle.clone(),
                        role: agent.role.clone(),
                    },
                )
            })
            .collect();
        Self {
            project: snapshot.project.clone(),
            agents,
        }
    }

    /// An agent's handle, or its id when the roster does not have it.
    fn handle(&self, id: &IdentityId) -> String {
        self.agents
            .get(id)
            .map_or_else(|| id.to_string(), |agent| agent.handle.clone())
    }

    /// The tab a reference chip opens: an agent in the roster as its agent
    /// tab, anything else as a placeholder until Task 12 routes it.
    pub(super) fn tab_for_ref(&self, target: &RefTarget) -> TabItem {
        if let RefTarget::Agent(handle) = target
            && let Some((id, _)) = self.agents.iter().find(|(_, a)| a.handle == *handle)
        {
            return TabItem::Agent(id.clone());
        }
        TabItem::Ref(target.clone())
    }

    /// The tab's label, as the mockup's tab bar (console.py `tab_bar`).
    pub(super) fn tab_label(&self, item: &TabItem) -> String {
        match item {
            TabItem::Chat => "chat".into(),
            TabItem::Agent(id) => self.handle(id),
            TabItem::File { path, .. } => file_name(path).into(),
            TabItem::Diff { path, .. } => format!("{} (diff)", file_name(path)),
            TabItem::RoleFile(role) => role_file_name(role),
            TabItem::Plan => "plan".into(),
            TabItem::Spec(path) => file_name(path).into(),
            TabItem::Settings(_) => "settings".into(),
            TabItem::SettingsFile(path) => path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            TabItem::Search => "Search".into(),
            TabItem::Ref(target) => ref_label(target),
        }
    }

    /// The breadcrumb's parts, outermost first.
    pub(super) fn breadcrumb(&self, item: &TabItem) -> Vec<String> {
        match item {
            TabItem::File { worktree, path } | TabItem::Diff { worktree, path } => {
                vec![worktree.clone(), path.clone()]
            }
            TabItem::RoleFile(_) => vec!["roles".into(), self.tab_label(item)],
            TabItem::Settings(page) => vec!["settings".into(), page.label().into()],
            TabItem::SettingsFile(_) => vec!["settings".into(), self.tab_label(item)],
            TabItem::Spec(path) => vec![self.project.clone(), path.clone()],
            TabItem::Chat
            | TabItem::Agent(_)
            | TabItem::Plan
            | TabItem::Search
            | TabItem::Ref(_) => {
                vec![self.project.clone(), self.tab_label(item)]
            }
        }
    }

    /// The tab's icon and its color (console.py `tab_bar` and `plan_tabs`).
    pub(super) fn tab_icon(&self, item: &TabItem) -> (UiIcon, Color) {
        match item {
            TabItem::Chat | TabItem::Plan => (UiIcon::Chat, Color::Accent),
            TabItem::Agent(id) => (
                UiIcon::Sparkle,
                self.agents.get(id).map_or(Color::Placeholder, |agent| {
                    Color::Role(agent.role.clone().into())
                }),
            ),
            // The icon theme's file icon (`Pane::tab_file_icon`), or Lucide's
            // in the muted icon color, as the Project and Roles panels draw it.
            TabItem::File { .. } | TabItem::RoleFile(_) => (UiIcon::File, Color::Muted),
            TabItem::Diff { .. } => (UiIcon::Diff, Color::Warning),
            TabItem::Spec(_) => (UiIcon::File, Color::Warning),
            TabItem::Settings(_) => (UiIcon::Settings, Color::Placeholder),
            TabItem::SettingsFile(_) => (UiIcon::File, Color::Muted),
            TabItem::Search => (UiIcon::Search, Color::Placeholder),
            TabItem::Ref(RefTarget::File { .. }) => (UiIcon::File, Color::Accent),
            TabItem::Ref(RefTarget::Commit(_) | RefTarget::Branch { .. }) => {
                (UiIcon::Git, Color::Success)
            }
            TabItem::Ref(_) => (UiIcon::File, Color::Placeholder),
        }
    }
}

/// A reference as its chip reads: `src/main.rs:2-8`, `#9`.
fn ref_label(target: &RefTarget) -> String {
    match target {
        RefTarget::Agent(name)
        | RefTarget::Person(name)
        | RefTarget::Role(name)
        | RefTarget::Commit(name)
        | RefTarget::Item(name)
        | RefTarget::Lane(name) => name.clone(),
        RefTarget::Branch { name, at: None } => name.clone(),
        RefTarget::Branch { name, at: Some(at) } => format!("{name}@{at}"),
        RefTarget::File { path, lines: None } => file_name(path).into(),
        RefTarget::File {
            path,
            lines: Some((from, to)),
        } => format!("{}:{from}-{to}", file_name(path)),
        RefTarget::Pr(number) => format!("#{number}"),
    }
}

/// Labels in the UI font; the rest are names people type, in the code font.
pub(super) const fn label_is_code(item: &TabItem) -> bool {
    !matches!(
        item,
        TabItem::Chat | TabItem::Plan | TabItem::Settings(_) | TabItem::Search
    )
}
