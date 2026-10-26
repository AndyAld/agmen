//! The left dock's Agents tab in a project with a plan (frame "Desktop ·
//! plan", node 8656): the planning room's people with their status (V67)
//! and its agents, the planner first, then the plan's team, "planned" and
//! folded until the plan runs, then by owner (frame "Desktop · running",
//! V67). Rows are the Agents dock's (mockup fix 47).
//!
//! Tests find the sections as `planning-room` and `plan-team`, people as
//! `room:person:{name}`, agents as `room:agent:{handle}`, and the team as
//! `team:person:{owner}` and `team:agent:{owner}/{handle}`.

use std::collections::BTreeSet;

use gpui::{
    Context, Entity, EventEmitter, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Window, div,
};

use super::agents_panel::{AgentRole, AgentsPanelEvent, agent_item};
use super::components::{Color, Icon, IconSize, Indent, Label, ListItem, SectionHeader, StatusDot};
use super::composer::Target;
use super::format::title_case;
use super::plan_model::PlanModel;
use super::tokens::SPACE_6;
use crate::backend::{AgentStatus, PersonGroup, PlanInfo, Snapshot};
use crate::icons::UiIcon;

/// The role the planning room's composer talks to.
const PLANNER: &str = "planner";

/// The tag after a person in the room: "you", or "human".
#[must_use]
fn person_tag(group: &PersonGroup) -> &'static str {
    if group.you { "you" } else { "human" }
}

/// The team section's chip: "planned", then the team's size once it runs.
#[must_use]
fn team_count(plan: &PlanInfo) -> String {
    if plan.running {
        plan.team.len().to_string()
    } else {
        "planned".into()
    }
}

/// The planning room's composer target and placeholder (node 8927): the
/// project's planner, which runs on its owner's machine.
#[must_use]
pub(crate) fn planner_target(snapshot: &Snapshot) -> Option<(Target, String)> {
    snapshot.plan.as_ref()?;
    let agent = snapshot
        .roster
        .iter()
        .flat_map(|group| &group.agents)
        .find(|agent| agent.role == PLANNER)?;
    let owner = agent.handle.split('/').next().unwrap_or(&agent.handle);
    let placeholder = format!(
        "Plan with {} · it runs on {owner}'s machine with their keys",
        agent.handle
    );
    Some((
        Target {
            id: agent.id.clone(),
            handle: agent.handle.clone(),
        },
        placeholder,
    ))
}

pub(crate) struct PlanRoom {
    roster: Vec<PersonGroup>,
    model: Entity<PlanModel>,
    room_open: bool,
    /// Whether the team shows; `None` follows the plan: open once it runs.
    team_open: Option<bool>,
    /// Team owners whose agents are folded.
    folded: BTreeSet<String>,
    _model_changes: Subscription,
}

impl std::fmt::Debug for PlanRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanRoom")
            .field("room_open", &self.room_open)
            .field("team_open", &self.team_open)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<AgentsPanelEvent> for PlanRoom {}

impl PlanRoom {
    pub(crate) fn new(
        roster: Vec<PersonGroup>,
        model: Entity<PlanModel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let model_changes = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            roster,
            model,
            room_open: true,
            team_open: None,
            folded: BTreeSet::new(),
            _model_changes: model_changes,
        }
    }

    fn room_rows(&self, plan: &PlanInfo, cx: &mut Context<Self>) -> Vec<ListItem> {
        let people = self.roster.iter().map(|group| {
            let status = plan
                .presence
                .get(&group.person)
                .copied()
                .unwrap_or(AgentStatus::Idle);
            ListItem::new(format!("room:person:{}", group.person), Indent::Depth(0))
                .leading(StatusDot::new(status))
                .child(Label::new(group.person.clone()).code())
                .end(
                    Label::new(person_tag(group))
                        .code()
                        .color(Color::Placeholder),
                )
        });
        let mut agents: Vec<_> = self.roster.iter().flat_map(|group| &group.agents).collect();
        // The planner leads: the room is about its plan (frame 8656).
        agents.sort_by_key(|agent| agent.role != PLANNER);
        let agents = agents.into_iter().map(|agent| {
            let id = agent.id.clone();
            agent_item(
                format!("room:agent:{}", agent.handle),
                0,
                agent.status,
                &agent.handle,
                &AgentRole::of(&agent.role),
                &agent.harness,
            )
            .on_click(
                cx.listener(move |_, _, _, cx| cx.emit(AgentsPanelEvent::Selected(id.clone()))),
            )
        });
        people.chain(agents).collect()
    }

    fn team_rows(&self, plan: &PlanInfo, cx: &mut Context<Self>) -> Vec<ListItem> {
        let status = if plan.running {
            AgentStatus::Working
        } else {
            AgentStatus::Idle
        };
        let mut rows = Vec::new();
        for owner in crate::plan::owners(plan) {
            let folded = self.folded.contains(owner);
            let chevron = if folded {
                UiIcon::ChevronRight
            } else {
                UiIcon::ChevronDown
            };
            let name = owner.to_owned();
            rows.push(
                ListItem::new(format!("team:person:{owner}"), Indent::Depth(0))
                    .leading(Icon::new(chevron).size(IconSize::Small).color(Color::Muted))
                    .child(Label::new(owner.to_owned()).weight(FontWeight::MEDIUM))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.folded.remove(&name) {
                            this.folded.insert(name.clone());
                        }
                        cx.notify();
                    })),
            );
            if folded {
                continue;
            }
            // shortcut: team rows take no click; the backend has no details
            // to show for them. Emit `AgentsPanelEvent::Selected` with
            // `TeamRow::id` once it does.
            for row in plan.team.iter().filter(|row| row.owner == owner) {
                // The team table's harness reads "claude · sonnet"; the
                // dock shows the agent only, as the Agents dock does.
                let harness = row.harness.split(' ').next().unwrap_or(&row.harness);
                let role = AgentRole {
                    name: row.role.clone(),
                    label: row.role_label.clone(),
                };
                rows.push(agent_item(
                    format!("team:agent:{owner}/{}", row.handle),
                    1,
                    status,
                    &row.handle,
                    &role,
                    harness,
                ));
            }
        }
        rows
    }
}

impl Render for PlanRoom {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(plan) = self.model.read(cx).plan().cloned() else {
            return div().debug_selector(|| "plan-room".into());
        };
        let room_size = self.roster.len()
            + self
                .roster
                .iter()
                .map(|group| group.agents.len())
                .sum::<usize>();
        let team_open = self.team_open.unwrap_or(plan.running);
        let room = SectionHeader::new("planning-room", "Planning room")
            .open(self.room_open)
            .count(room_size.to_string())
            .on_click(cx.listener(|this, _, _, cx| {
                this.room_open = !this.room_open;
                cx.notify();
            }));
        let team = SectionHeader::new("plan-team", format!("{} team", title_case(&plan.name)))
            .open(team_open)
            .count(team_count(&plan))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.team_open = Some(!team_open);
                cx.notify();
            }));
        let room_rows = if self.room_open {
            self.room_rows(&plan, cx)
        } else {
            Vec::new()
        };
        let team_rows = if team_open {
            self.team_rows(&plan, cx)
        } else {
            Vec::new()
        };
        div()
            .debug_selector(|| "plan-room".into())
            .w_full()
            .flex()
            .flex_col()
            .py(SPACE_6)
            .child(room)
            .children(room_rows)
            .child(team)
            .children(team_rows)
    }
}

#[cfg(test)]
#[path = "plan_room_tests.rs"]
mod tests;
