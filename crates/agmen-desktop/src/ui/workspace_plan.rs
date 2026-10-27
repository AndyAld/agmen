//! The workspace's planning room (Task 19): the plan model, the planning
//! room in the Agents tab, the plan or Review panel in the right dock, the
//! kick-off dialog over the window, the status bar's context and the spec
//! tab a plan opens. A child of `workspace`, so it reaches its state.

use gpui::{
    AnyElement, App, AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement,
    Pixels, SharedString, Subscription,
};
use jiff::tz::TimeZone;

use super::Workspace;
use crate::backend::Snapshot;
use crate::local_time::LocalZone;
use crate::ui::agents_panel::AgentsPanelEvent;
use crate::ui::kickoff_dialog::KickoffDialog;
use crate::ui::pane::TabItem;
use crate::ui::plan_model::PlanModel;
use crate::ui::plan_panel::{self, OpenSpec, PlanPanel};
use crate::ui::plan_room::PlanRoom;
use crate::ui::review_panel::ReviewPanel;
use crate::ui::tokens::{PLAN_PANEL_WIDTH, RIGHT_DOCK_WIDTH};

/// The planning room's views, when the project has a plan.
pub(crate) struct PlanWiring {
    model: Option<Entity<PlanModel>>,
    pub(super) room: Option<Entity<PlanRoom>>,
    _subscriptions: Vec<Subscription>,
}

/// What the right dock shows for the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanDock {
    /// The plan panel (node 8935).
    Plan,
    /// The Review panel beside the spec at this path (node 9283).
    Review(String),
}

impl PlanWiring {
    pub(crate) fn new(snapshot: &Snapshot, cx: &mut Context<Workspace>) -> Self {
        let Some(model) = PlanModel::install(snapshot, cx) else {
            return Self {
                model: None,
                room: None,
                _subscriptions: Vec::new(),
            };
        };
        let room = cx.new(|cx| PlanRoom::new(snapshot.roster.clone(), model.clone(), cx));
        let room_events = cx.subscribe(&room, |this, _, event, cx| {
            let AgentsPanelEvent::Selected(id) = event;
            this.select_agent(id, cx);
        });
        // The dock, the dialog and the status bar follow the plan.
        let model_changes = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model: Some(model),
            room: Some(room),
            _subscriptions: vec![room_events, model_changes],
        }
    }
}

/// The spec the mockup opens beside the plan tab: the first one the
/// backend has text for (ridgeline-design.md, tab 9174).
pub(super) fn mockup_spec(snapshot: &Snapshot) -> Option<String> {
    let plan = snapshot.plan.as_ref()?;
    plan.specs
        .iter()
        .find(|spec| plan.reviews.iter().any(|review| review.path == spec.path))
        .map(|spec| spec.path.clone())
}

impl Workspace {
    /// What the right dock shows for the plan, unless a role or an agent
    /// with details is shown: Review beside a spec tab, else the plan.
    fn plan_dock(&self, cx: &App) -> Option<PlanDock> {
        self.plan.model.as_ref()?;
        if self.left.shown_role.is_some() || self.shown_agent().is_some() {
            return None;
        }
        Some(match self.pane_group.read(cx).active_item(cx) {
            Some(TabItem::Spec(path)) => PlanDock::Review(path),
            _ => PlanDock::Plan,
        })
    }

    /// The right dock's plan or Review panel, when it shows one.
    pub(super) fn plan_panel(&self, cx: &App) -> Option<AnyElement> {
        let model = self.plan.model.as_ref()?;
        match self.plan_dock(cx)? {
            PlanDock::Plan => Some(PlanPanel::new(model.clone()).into_any_element()),
            PlanDock::Review(path) => {
                let zone = cx
                    .try_global::<LocalZone>()
                    .map_or(TimeZone::UTC, |local| local.0.clone());
                let plan = model.read(cx).plan()?;
                ReviewPanel::new(plan, &path, &zone).map(IntoElement::into_any_element)
            }
        }
    }

    /// The right dock's width: the plan panel is wider (node 8935, 470).
    pub(super) fn plan_dock_width(&self, cx: &App) -> Option<Pixels> {
        Some(match self.plan_dock(cx)? {
            PlanDock::Plan => PLAN_PANEL_WIDTH,
            PlanDock::Review(_) => RIGHT_DOCK_WIDTH,
        })
    }

    /// The kick-off dialog over the window while it is open.
    pub(super) fn plan_overlay(&self, cx: &App) -> Option<AnyElement> {
        let model = self.plan.model.as_ref()?;
        model
            .read(cx)
            .kickoff_open()
            .then(|| KickoffDialog::new(model.clone()).into_any_element())
    }

    /// "ridgeline · planning", or "ridgeline · 7 agents" once it runs.
    pub(super) fn plan_status_context(&self, cx: &App) -> Option<SharedString> {
        let plan = self.plan.model.as_ref()?.read(cx).plan()?;
        Some(plan_panel::status_context(plan).into())
    }

    /// Opens the kick-off dialog, as the KickOff screen shows it.
    pub(super) fn open_kickoff(&self, cx: &mut Context<Self>) {
        if let Some(model) = &self.plan.model {
            model.update(cx, |model, cx| model.open_kickoff(cx));
        }
    }

    /// Registers the plan panel's actions; the dialog handles its own.
    pub(super) fn plan_actions(root: Div, cx: &Context<Self>) -> Div {
        root.on_action(cx.listener(|this, action: &OpenSpec, _, cx| {
            let item = TabItem::Spec(action.0.clone());
            this.pane_group.update(cx, |group, cx| group.open(item, cx));
            cx.notify();
        }))
    }
}
