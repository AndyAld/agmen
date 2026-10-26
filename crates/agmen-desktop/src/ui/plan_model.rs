//! The planning room's state that the plan panel, the spec tab, the Review
//! panel and the kick-off dialog share: the plan as the backend last sent
//! it, whether the dialog is open, and why the last action failed. It hears
//! the backend's updates, so an approval or a kick-off shows everywhere.

use futures::StreamExt as _;
use gpui::{AppContext as _, Context, Entity, Global, SharedString, Task};

use crate::backend::{Action, Approval, BackendError, BackendHandle, PlanInfo, Snapshot, Update};

/// The workspace's plan model, for views the workspace does not build
/// itself (the spec tab).
#[derive(Debug, Clone)]
pub(crate) struct PlanHandle(pub(crate) Entity<PlanModel>);

impl Global for PlanHandle {}

pub(crate) struct PlanModel {
    project: String,
    me: String,
    plan: Option<PlanInfo>,
    kickoff_open: bool,
    /// Why the last action failed, until the next one succeeds.
    error: Option<SharedString>,
    _updates: Option<Task<()>>,
}

impl std::fmt::Debug for PlanModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanModel")
            .field("project", &self.project)
            .field("kickoff_open", &self.kickoff_open)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl PlanModel {
    /// The plan in `snapshot`, kept up to date from the [`BackendHandle`]
    /// global's updates when there is one.
    pub(crate) fn new(snapshot: &Snapshot, cx: &mut Context<Self>) -> Self {
        let updates = cx
            .try_global::<BackendHandle>()
            .map(|backend| backend.0.subscribe());
        // The loop ends when the backend drops its sender or the model goes.
        let updates = updates.map(|mut updates| {
            cx.spawn(async move |model, cx| {
                while let Some(update) = updates.next().await {
                    let Update::Snapshot(snapshot) = update else {
                        continue;
                    };
                    let applied = model.update(cx, |model, cx| {
                        model.plan = snapshot.plan;
                        cx.notify();
                    });
                    if applied.is_err() {
                        break;
                    }
                }
            })
        });
        Self {
            project: snapshot.project.clone(),
            me: snapshot.me.clone(),
            plan: snapshot.plan.clone(),
            kickoff_open: false,
            error: None,
            _updates: updates,
        }
    }

    /// Makes the model for `snapshot` and sets it as the [`PlanHandle`]
    /// global; `None` when the project has no plan.
    pub(crate) fn install(snapshot: &Snapshot, cx: &mut gpui::App) -> Option<Entity<Self>> {
        snapshot.plan.as_ref()?;
        let model = cx.new(|cx| Self::new(snapshot, cx));
        cx.set_global(PlanHandle(model.clone()));
        Some(model)
    }

    pub(crate) fn project(&self) -> &str {
        &self.project
    }

    /// The person's handle, such as `andy`.
    pub(crate) fn me(&self) -> &str {
        &self.me
    }

    pub(crate) const fn plan(&self) -> Option<&PlanInfo> {
        self.plan.as_ref()
    }

    pub(crate) const fn kickoff_open(&self) -> bool {
        self.kickoff_open
    }

    pub(crate) fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// My approval of the spec at `path`, or `None` when I am not asked to
    /// approve it (a draft, or someone else's review).
    pub(crate) fn my_approval(&self, path: &str) -> Option<Approval> {
        self.plan
            .as_ref()?
            .specs
            .iter()
            .find(|spec| spec.path == path && spec.draft_note.is_none())?
            .approvals
            .get(&self.me)
            .copied()
    }

    /// Approves the spec at `path` at the commit the tab shows (spec 7:
    /// an approval names the commit its approver read).
    pub(crate) fn approve_spec(&mut self, path: &str, cx: &mut Context<Self>) {
        let sha = self
            .plan
            .as_ref()
            .and_then(|plan| plan.specs.iter().find(|spec| spec.path == path))
            .map(|spec| spec.sha.clone());
        match sha {
            Some(sha) => self.act(
                Action::ApproveSpec {
                    path: path.to_owned(),
                    sha,
                },
                cx,
            ),
            None => self.fail(
                BackendError::Unsupported("approving a spec the plan does not list"),
                cx,
            ),
        }
    }

    /// Opens the kick-off dialog, when the plan is ready to kick off.
    pub(crate) fn open_kickoff(&mut self, cx: &mut Context<Self>) {
        if self.plan.as_ref().is_some_and(crate::plan::kickoff_enabled) {
            self.kickoff_open = true;
            self.error = None;
            cx.notify();
        }
    }

    pub(crate) fn close_kickoff(&mut self, cx: &mut Context<Self>) {
        self.kickoff_open = false;
        cx.notify();
    }

    /// Kicks off the plan; the dialog closes when the backend takes it and
    /// stays open with the reason when it refuses.
    pub(crate) fn kick_off(&mut self, cx: &mut Context<Self>) {
        self.act(Action::KickOff, cx);
        if self.error.is_none() {
            self.kickoff_open = false;
        }
    }

    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        let result = match cx.try_global::<BackendHandle>() {
            Some(backend) => backend.0.act(action),
            None => Err(BackendError::Unsupported("acting without a backend")),
        };
        match result {
            Ok(()) => {
                self.error = None;
                cx.notify();
            }
            Err(err) => self.fail(err, cx),
        }
    }

    fn fail(&mut self, err: BackendError, cx: &mut Context<Self>) {
        self.error = Some(err.to_string().into());
        cx.notify();
    }
}
