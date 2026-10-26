//! The right dock's plan panel in a project with a plan (frame "Desktop ·
//! plan", node 8935): the goal, the team, the specs and their approvals,
//! the lanes, the kick-off checklist (`plan::checklist`) and Kick off, or
//! Pause all once the plan runs.
//!
//! Tests find the panel as `plan-panel`, its team rows as `team:row-{i}`,
//! the running glyphs as `team:running-{i}`, spec rows as
//! `plan:spec:{path}`, checks as `plan:check-{i}` and the buttons as
//! `plan:kickoff` and `plan:pause`.

use gpui::{
    Action, AnyElement, App, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::components::{
    Badge, Button, ButtonStyle, CheckRow, Color, Dot, Heading, Label, LabelSize, StatusDot,
    TableCard,
};
use super::dock::RightDockHeader;
use super::plan_model::PlanModel;
use super::tokens::{
    SPACE_8, SPACE_12, SPACE_14, TEAM_HANDLE_COLUMN, TEAM_HARNESS_COLUMN, TEAM_LANE_COLUMN,
    TEAM_OWNER_COLUMN, TEAM_ROLE_COLUMN,
};
use crate::backend::{AgentStatus, Approval, PlanInfo, SpecState, TeamRow};
use crate::plan::{self, Check};

/// Opens a spec's tab in the center: a click on a spec in the plan panel.
#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct OpenSpec(pub(crate) String);

/// The head's state pill (node 8940): what the plan waits on, that it is
/// ready, or that it runs.
#[must_use]
pub(crate) fn state_pill(plan: &PlanInfo) -> (String, Color) {
    if plan.running {
        return (
            format!("running · {}", plan::agent_count(plan.team.len())),
            Color::Accent,
        );
    }
    if plan::kickoff_enabled(plan) {
        return ("ready to kick off".into(), Color::Success);
    }
    let specs = plan
        .specs
        .iter()
        .filter(|spec| spec.draft_note.is_none())
        .flat_map(|spec| spec.approvals.values())
        .filter(|approval| **approval == Approval::Waiting)
        .count();
    let shares = plan::owners(plan)
        .into_iter()
        .filter(|owner| !plan.shares_approved.get(*owner).copied().unwrap_or(false))
        .count();
    match specs + shares {
        0 => ("not ready to kick off".into(), Color::Warning),
        1 => ("waiting on 1 approval".into(), Color::Warning),
        n => (format!("waiting on {n} approvals"), Color::Warning),
    }
}

/// The status bar's context while the project has a plan: "ridgeline ·
/// planning", then "ridgeline · 7 agents" once it runs (node 9804).
#[must_use]
pub(crate) fn status_context(plan: &PlanInfo) -> String {
    if plan.running {
        format!("{} · {}", plan.name, plan::agent_count(plan.team.len()))
    } else {
        format!("{} · planning", plan.name)
    }
}

/// The line under the team table (node 9010).
#[must_use]
fn share_note(plan: &PlanInfo) -> String {
    let owners = plan::owners(plan);
    let shares: Vec<String> = owners
        .iter()
        .map(|owner| format!("{owner} ${}", plan::share(plan, owner).usd_per_day))
        .collect();
    let total: u32 = plan.team.iter().map(|row| row.usd_per_day).sum();
    format!(
        "${total}/day: {}. Each agent runs on its owner's machine with that person's keys, \
         and each person approves their own share.",
        shares.join(", ")
    )
}

/// "L-CORE core/** · L-APP app/** ..." (node 9029).
#[must_use]
fn lanes_line(plan: &PlanInfo) -> String {
    plan.lanes
        .iter()
        .map(|lane| format!("{} {}", lane.name, lane.paths.join(" ")))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A spec row's state (nodes 9012 to 9024): its draft note, "approved",
/// or each approver with ✓ or ○.
#[must_use]
fn spec_state(spec: &SpecState) -> (String, Color) {
    if let Some(note) = &spec.draft_note {
        return (note.clone(), Color::Warning);
    }
    if spec.approved_by_all() {
        return ("approved".into(), Color::Success);
    }
    let people: Vec<String> = spec
        .approvals
        .iter()
        .map(|(person, approval)| match approval {
            Approval::Approved => format!("{person} ✓"),
            Approval::Waiting => format!("{person} ○"),
        })
        .collect();
    (people.join(" · "), Color::Warning)
}

/// The plan panel; it reads the plan model each time it draws.
#[derive(Debug, IntoElement)]
pub(crate) struct PlanPanel {
    model: Entity<PlanModel>,
}

impl PlanPanel {
    #[must_use]
    pub(crate) const fn new(model: Entity<PlanModel>) -> Self {
        Self { model }
    }
}

/// One team row's cells, as node 8947 lays them out.
fn team_cells(ix: usize, row: &TeamRow, running: bool) -> Vec<AnyElement> {
    let role = Color::Role(row.role.clone().into());
    let mut cells = Vec::with_capacity(9);
    if running {
        let selector = format!("team:running-{ix}");
        cells.push(
            div()
                .debug_selector(|| selector)
                .child(StatusDot::new(AgentStatus::Working))
                .into_any_element(),
        );
    }
    let column = |width, label: Label| {
        div()
            .w(width)
            .flex_none()
            .overflow_hidden()
            .child(label)
            .into_any_element()
    };
    cells.extend([
        Dot::new(role.clone()).into_any_element(),
        column(
            TEAM_ROLE_COLUMN,
            Label::new(row.role_label.clone())
                .size(LabelSize::Small)
                .color(role),
        ),
        column(TEAM_HANDLE_COLUMN, Label::new(row.handle.clone()).code()),
        column(
            TEAM_HARNESS_COLUMN,
            Label::new(row.harness.clone())
                .code()
                .color(Color::Placeholder),
        ),
        column(TEAM_LANE_COLUMN, Label::new(row.lane.clone()).code()),
        column(
            TEAM_OWNER_COLUMN,
            Label::new(row.owner.clone())
                .size(LabelSize::Small)
                .color(Color::Placeholder),
        ),
        div().flex_1().into_any_element(),
        Label::new(format!("${}/d", row.usd_per_day))
            .code()
            .color(Color::Placeholder)
            .into_any_element(),
    ]);
    cells
}

/// A spec's row; a spec the backend has text for opens its tab.
fn spec_row(spec: &SpecState, has_text: bool) -> impl IntoElement {
    let (state, state_color) = spec_state(spec);
    let selector = format!("plan:spec:{}", spec.path);
    let path = Label::new(spec.path.clone()).code().color(if has_text {
        Color::Accent
    } else {
        Color::Default
    });
    let row = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(|| selector)
        .w_full()
        .flex()
        .items_center()
        .gap(SPACE_8)
        .child(path)
        .child(div().flex_1())
        .child(Label::new(state).size(LabelSize::Small).color(state_color));
    if has_text {
        let open = spec.path.clone();
        row.cursor_pointer().on_click(move |_, window, cx| {
            window.dispatch_action(Box::new(OpenSpec(open.clone())), cx);
        })
    } else {
        row
    }
}

/// Kick off while planning, enabled once every check passes; Pause all
/// once the plan runs.
fn plan_button(plan: &PlanInfo, model: Entity<PlanModel>) -> Button {
    if plan.running {
        // shortcut: the backend has no pause action yet (desktop plan,
        // Task 19), so Pause all draws disabled; enable it once agmend has one.
        return Button::new("plan:pause", "Pause all")
            .large()
            .disabled(true);
    }
    let label = plan::kickoff_label(plan);
    if plan::kickoff_enabled(plan) {
        Button::new("plan:kickoff", label)
            .style(ButtonStyle::Primary)
            .large()
            .on_click(move |_, _, cx| model.update(cx, |model, cx| model.open_kickoff(cx)))
    } else {
        Button::new("plan:kickoff", label).large().disabled(true)
    }
}

impl RenderOnce for PlanPanel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let model = self.model.read(cx);
        let Some(plan) = model.plan().cloned() else {
            return div().debug_selector(|| "plan-panel".into());
        };
        let checks: Vec<Check> = plan::checklist(&plan, model.me());
        let (pill, pill_color) = state_pill(&plan);
        let team = plan
            .team
            .iter()
            .enumerate()
            .fold(TableCard::new("team"), |table, (ix, row)| {
                table.row(team_cells(ix, row, plan.running))
            });
        let specs = plan.specs.iter().map(|spec| {
            let has_text = plan.reviews.iter().any(|review| review.path == spec.path);
            spec_row(spec, has_text)
        });
        let checks = checks.into_iter().enumerate().map(|(ix, check)| {
            let selector = format!("plan:check-{ix}");
            div()
                .debug_selector(|| selector)
                .child(CheckRow::new(check.label, check.ok))
        });
        let content = div()
            .id("plan-content")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(SPACE_14)
            .py(SPACE_12)
            .px(SPACE_14)
            .child(Heading::new("Goal"))
            .child(Label::new(plan.goal.clone()).size(LabelSize::Small).wrap())
            .child(Heading::new(&format!(
                "Team · {}",
                plan::agent_count(plan.team.len())
            )))
            .child(team)
            .child(
                Label::new(share_note(&plan))
                    .size(LabelSize::Small)
                    .color(Color::Placeholder)
                    .wrap(),
            )
            .child(Heading::new("Specs"))
            .children(specs)
            .child(Heading::new("Lanes"))
            .child(Label::new(lanes_line(&plan)).code().wrap())
            .child(Heading::new("Before kick-off"))
            .children(checks)
            .child(div().flex().child(plan_button(&plan, self.model.clone())));
        div()
            .debug_selector(|| "plan-panel".into())
            .size_full()
            .flex()
            .flex_col()
            .child(
                RightDockHeader::new("Plan", format!("{} · v{}", plan.name, plan.version))
                    .end(Badge::new(pill, pill_color)),
            )
            .child(content)
    }
}

#[cfg(test)]
#[path = "plan_panel_tests.rs"]
mod tests;
