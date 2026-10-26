//! The left dock's Agents tab: people and their agents, nested by who
//! delegated to whom.

use std::collections::BTreeSet;

use gpui::{
    Context, EventEmitter, FontWeight, IntoElement, ParentElement as _, Render, Styled as _,
    Window, div,
};

use super::components::{Color, Icon, IconSize, Indent, Label, LabelSize, ListItem, StatusDot};
use super::format::title_case;
use super::tokens::SPACE_6;
use crate::backend::{AgentRow, PersonGroup};
use agmen_proto::IdentityId;

use crate::icons::UiIcon;

/// What the Agents panel tells the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentsPanelEvent {
    /// An agent row was clicked; holds its identity id.
    Selected(IdentityId),
}

pub(crate) struct AgentsPanel {
    roster: Vec<PersonGroup>,
    folded: BTreeSet<String>,
    /// The row the right dock shows; the workspace sets it.
    selected: Option<IdentityId>,
}

impl EventEmitter<AgentsPanelEvent> for AgentsPanel {}

impl AgentsPanel {
    pub(crate) const fn new(roster: Vec<PersonGroup>) -> Self {
        Self {
            roster,
            folded: BTreeSet::new(),
            selected: None,
        }
    }

    /// Folds or unfolds `person`'s agents.
    pub(crate) fn toggle_fold(&mut self, person: &str, cx: &mut Context<Self>) {
        if !self.folded.remove(person) {
            self.folded.insert(person.to_owned());
        }
        cx.notify();
    }

    /// Tells the workspace a row was clicked; the workspace decides what is
    /// selected and calls [`AgentsPanel::set_selected`].
    fn clicked(&mut self, id: &IdentityId, cx: &mut Context<Self>) {
        cx.emit(AgentsPanelEvent::Selected(id.clone()));
    }

    /// Task 14: takes the backend's roster, such as an agent's harness
    /// after a live switch.
    pub(crate) fn set_roster(&mut self, roster: Vec<PersonGroup>, cx: &mut Context<Self>) {
        self.roster = roster;
        cx.notify();
    }

    /// Highlights the row the right dock shows, or none.
    pub(crate) fn set_selected(&mut self, id: Option<IdentityId>, cx: &mut Context<Self>) {
        self.selected = id;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) const fn selected(&self) -> Option<&IdentityId> {
        self.selected.as_ref()
    }

    /// The agent rows shown, in order: every agent of each unfolded person.
    #[cfg(test)]
    pub(crate) fn visible_agents(&self) -> impl Iterator<Item = &AgentRow> {
        self.roster
            .iter()
            .filter(|group| !self.folded.contains(&group.person))
            .flat_map(|group| &group.agents)
    }

    fn person_row(&self, group: &PersonGroup, cx: &mut Context<Self>) -> ListItem {
        let person = group.person.clone();
        let chevron = if self.folded.contains(&person) {
            UiIcon::ChevronRight
        } else {
            UiIcon::ChevronDown
        };
        let name = if group.you {
            format!("{} (you)", group.person)
        } else {
            group.person.clone()
        };
        ListItem::new(format!("person:{person}"), Indent::Group)
            .leading(Icon::new(chevron).size(IconSize::Small).color(Color::Muted))
            .child(Label::new(name).weight(FontWeight::MEDIUM))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(&person, cx)))
    }

    fn agent_row(&self, agent: &AgentRow, cx: &mut Context<Self>) -> ListItem {
        let id = agent.id.clone();
        ListItem::new(
            format!("agent:{}", agent.handle),
            Indent::Depth(agent.depth),
        )
        .selected(self.selected.as_ref() == Some(&id))
        .leading(StatusDot::new(agent.status))
        .child(Label::new(agent.name.clone()).code())
        .child(
            Label::new(title_case(&agent.role))
                .size(LabelSize::Small)
                .color(Color::Role(agent.role.clone().into())),
        )
        .end(
            Label::new(agent.harness.clone())
                .code()
                .color(Color::Placeholder),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.clicked(&id, cx)))
    }
}

impl Render for AgentsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = Vec::new();
        for group in &self.roster {
            rows.push(self.person_row(group, cx));
            if !self.folded.contains(&group.person) {
                for agent in &group.agents {
                    rows.push(self.agent_row(agent, cx));
                }
            }
        }
        div().w_full().flex().flex_col().py(SPACE_6).children(rows)
    }
}

#[cfg(test)]
#[path = "agents_panel_tests.rs"]
mod tests;
