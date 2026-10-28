//! The left dock's Agents tab: people and their agents, nested by who
//! delegated to whom.

use std::collections::{BTreeMap, BTreeSet};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, EventEmitter, FontWeight, IntoElement, ParentElement as _, Render,
    Styled as _, Window, div,
};

use super::components::{
    Button, ButtonStyle, Color, Icon, IconSize, Indent, Label, LabelSize, ListItem, Menu, MenuRow,
    StatusDot,
};
use super::format::title_case;
use super::tokens::SPACE_6;
use crate::backend::{AgentRow, AgentStatus, PersonGroup};
use agmen_proto::IdentityId;

use crate::icons::UiIcon;

/// What the Agents panel tells the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentsPanelEvent {
    /// An agent row was clicked; holds its identity id.
    Selected(IdentityId),
    NewAgent,
    Stop(IdentityId),
    /// Start from the agent's menu: back online with its previous context.
    Start(IdentityId),
    /// Attach Claude Code… from the agent's menu.
    Attach(IdentityId),
    Detach(IdentityId),
    /// Remove… from the agent's menu; the workspace asks first.
    Remove(IdentityId),
}

/// The agent menu's items for a managed state, as (key, label). An agent
/// agmend does not manage is offline.
fn menu_items(state: Option<&str>) -> &'static [(&'static str, &'static str)] {
    match state {
        Some("attached") => &[("detach", "Detach"), ("remove", "Remove…")],
        Some("running" | "idle") => &[
            ("stop", "Stop"),
            ("attach", "Attach Claude Code…"),
            ("remove", "Remove…"),
        ],
        _ => &[
            ("start", "Start"),
            ("attach", "Attach Claude Code…"),
            ("remove", "Remove…"),
        ],
    }
}

/// The event a menu item emits.
fn menu_event(key: &str, id: IdentityId) -> Option<AgentsPanelEvent> {
    Some(match key {
        "start" => AgentsPanelEvent::Start(id),
        "stop" => AgentsPanelEvent::Stop(id),
        "attach" => AgentsPanelEvent::Attach(id),
        "detach" => AgentsPanelEvent::Detach(id),
        "remove" => AgentsPanelEvent::Remove(id),
        _ => return None,
    })
}

pub(crate) struct AgentsPanel {
    roster: Vec<PersonGroup>,
    managed: BTreeMap<IdentityId, String>,
    error: Option<String>,
    folded: BTreeSet<String>,
    /// The row the right dock shows; the workspace sets it.
    selected: Option<IdentityId>,
    /// The person's listener, which has no agent menu.
    listener: Option<IdentityId>,
    /// The agent whose menu is open.
    menu: Option<IdentityId>,
}

impl EventEmitter<AgentsPanelEvent> for AgentsPanel {}

impl AgentsPanel {
    pub(crate) const fn new(roster: Vec<PersonGroup>) -> Self {
        Self {
            roster,
            managed: BTreeMap::new(),
            error: None,
            folded: BTreeSet::new(),
            selected: None,
            listener: None,
            menu: None,
        }
    }

    /// Tells the panel which agent is the person's listener.
    pub(crate) fn set_listener(&mut self, listener: Option<IdentityId>, cx: &mut Context<Self>) {
        self.listener = listener;
        cx.notify();
    }

    fn open_menu(&mut self, id: &IdentityId, cx: &mut Context<Self>) {
        self.menu = Some(id.clone());
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    fn pick(&mut self, key: &str, id: &IdentityId, cx: &mut Context<Self>) {
        self.menu = None;
        if let Some(event) = menu_event(key, id.clone()) {
            cx.emit(event);
        }
        cx.notify();
    }

    pub(crate) fn set_managed(
        &mut self,
        managed: BTreeMap<IdentityId, String>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.managed = managed;
        self.error = error;
        cx.notify();
    }

    /// Folds or unfolds `person`'s agents.
    pub(crate) fn toggle_fold(&mut self, person: &str, cx: &mut Context<Self>) {
        if !self.folded.remove(person) {
            self.folded.insert(person.to_owned());
        }
        cx.notify();
    }

    /// Shows only `person`'s agents: every other person folds, and a click
    /// on their row unfolds them again (a person reference, spec 9.3).
    pub(crate) fn show_only(&mut self, person: &str, cx: &mut Context<Self>) {
        self.folded = self
            .roster
            .iter()
            .map(|group| &group.person)
            .filter(|name| *name != person)
            .cloned()
            .collect();
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

    /// An agent's row. The person's own agents, other than the listener,
    /// open the agent menu on a left or right click.
    fn agent_row(&self, agent: &AgentRow, mine: bool, cx: &mut Context<Self>) -> AnyElement {
        let menu = mine && self.listener.as_ref() != Some(&agent.id);
        let id = agent.id.clone();
        let item = agent_item(
            format!("agent:{}", agent.handle),
            agent.depth,
            agent.status,
            &agent.name,
            &AgentRole::of(&agent.role),
            &agent.harness,
        )
        .selected(self.selected.as_ref() == Some(&id))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.clicked(&id, cx);
            if menu {
                this.open_menu(&id, cx);
            }
        }));
        let item = if menu {
            let id = agent.id.clone();
            item.on_secondary_click(cx.listener(move |this, _, _, cx| this.open_menu(&id, cx)))
        } else {
            item
        };
        let open = menu && self.menu.as_ref() == Some(&agent.id);
        div()
            .relative()
            .w_full()
            .child(self.with_stop(item, agent, cx))
            .when(open, |row| {
                row.child(
                    div()
                        .absolute()
                        .top_full()
                        .left(SPACE_6)
                        .child(self.agent_menu(agent, cx)),
                )
            })
            .into_any_element()
    }

    /// The open menu of `agent`'s actions for its state.
    fn agent_menu(&self, agent: &AgentRow, cx: &mut Context<Self>) -> Menu {
        let rows: Vec<_> = menu_items(self.managed.get(&agent.id).map(String::as_str))
            .iter()
            .map(|(key, label)| {
                let id = agent.id.clone();
                MenuRow::new(format!("agent:{}:menu:{key}", agent.handle))
                    .child(Label::new(*label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.pick(key, &id, cx);
                    }))
            })
            .collect();
        let this = cx.entity().downgrade();
        Menu::new(format!("agent:{}:menu", agent.handle))
            .children(rows)
            .on_close(move |_, cx| {
                // Gone only while the window closes; nothing to close.
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| this.close_menu(cx));
                }
            })
    }

    /// The managed state and Stop at the row's end, as before the menu.
    fn with_stop(&self, item: ListItem, agent: &AgentRow, cx: &mut Context<Self>) -> ListItem {
        match self.managed.get(&agent.id) {
            Some(state) => {
                let identity = agent.id.clone();
                item.end(
                    div()
                        .flex()
                        .items_center()
                        .child(Label::new(state.clone()).size(LabelSize::Small))
                        .child(
                            Button::new(format!("agent:{}:stop", agent.handle), "Stop")
                                .style(ButtonStyle::Plain)
                                .disabled(state == "offline")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(AgentsPanelEvent::Stop(identity.clone()));
                                })),
                        ),
                )
            }
            None => item,
        }
    }
}

/// An agent's role: its name, which picks the color, and its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentRole {
    pub(crate) name: String,
    pub(crate) label: String,
}

impl AgentRole {
    /// A built-in role labelled as its name: `engineer` reads "Engineer".
    pub(crate) fn of(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            label: title_case(name),
        }
    }
}

/// An agent's row as the Agents dock draws it, without its click or
/// selection: status, name, role and harness. The planning room draws its
/// agents with it too (mockup fix 47).
pub(crate) fn agent_item(
    id: String,
    depth: u8,
    status: AgentStatus,
    name: &str,
    role: &AgentRole,
    harness: &str,
) -> ListItem {
    ListItem::new(id, Indent::Depth(depth))
        .leading(StatusDot::new(status))
        .child(Label::new(name.to_owned()).code())
        .child(
            Label::new(role.label.clone())
                .size(LabelSize::Small)
                .color(Color::Role(role.name.clone().into())),
        )
        .end(
            Label::new(harness.to_owned())
                .code()
                .color(Color::Placeholder),
        )
}

impl Render for AgentsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = Vec::new();
        for group in &self.roster {
            rows.push(self.person_row(group, cx).into_any_element());
            if !self.folded.contains(&group.person) {
                for agent in &group.agents {
                    rows.push(self.agent_row(agent, group.you, cx));
                }
            }
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .py(SPACE_6)
            .child(
                Button::new("agents:new", "New Agent…")
                    .style(ButtonStyle::Plain)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AgentsPanelEvent::NewAgent))),
            )
            .children(
                self.error
                    .clone()
                    .map(|error| Label::new(error).color(Color::Error).wrap()),
            )
            .children(rows)
    }
}

#[cfg(test)]
#[path = "agents_panel_tests.rs"]
mod tests;
