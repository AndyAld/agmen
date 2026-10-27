//! The planning room's state that the plan panel, the spec tab, the Review
//! panel and the kick-off dialog share: the plan as the backend last sent
//! it, whether the dialog is open and where it took focus from, and why
//! the last action from each view failed. It hears
//! the backend's updates, so an approval or a kick-off shows everywhere.

use agmen_proto::GitSha;
use futures::StreamExt as _;
use gpui::{AppContext as _, Context, Entity, FocusHandle, Global, SharedString, Task, Window};

use crate::backend::{Action, Approval, BackendError, BackendHandle, PlanInfo, Snapshot, Update};

/// The workspace's plan model, for views the workspace does not build
/// itself (the spec tab).
#[derive(Debug, Clone)]
pub(crate) struct PlanHandle(pub(crate) Entity<PlanModel>);

impl Global for PlanHandle {}

/// What a refusal came from, so each view shows only its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ErrorSource {
    /// Approving the spec at this path.
    Spec(String),
    /// Kicking off the plan.
    KickOff,
}

pub(crate) struct PlanModel {
    project: String,
    me: String,
    plan: Option<PlanInfo>,
    kickoff_open: bool,
    /// Why the last action from each source failed, until an action from
    /// there succeeds. At most one entry per source (two sources today).
    errors: Vec<(ErrorSource, SharedString)>,
    dialog_focus: FocusHandle,
    restore_focus: Option<FocusHandle>,
    _updates: Option<Task<()>>,
}

impl std::fmt::Debug for PlanModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanModel")
            .field("project", &self.project)
            .field("kickoff_open", &self.kickoff_open)
            .field("errors", &self.errors)
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
                    let applied = model.update(cx, |model, cx| model.apply(snapshot.plan, cx));
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
            errors: Vec::new(),
            dialog_focus: cx.focus_handle(),
            restore_focus: None,
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

    /// Takes the plan the backend sent.
    pub(crate) fn apply(&mut self, plan: Option<PlanInfo>, cx: &mut Context<Self>) {
        self.plan = plan;
        cx.notify();
    }

    /// Why the last action from `source` failed, until one from it succeeds.
    pub(crate) fn error(&self, source: &ErrorSource) -> Option<&SharedString> {
        self.errors
            .iter()
            .find(|(from, _)| from == source)
            .map(|(_, reason)| reason)
    }

    /// Where the dialog holds focus while it is open, so Escape reaches its
    /// key context.
    pub(crate) const fn dialog_focus(&self) -> &FocusHandle {
        &self.dialog_focus
    }

    /// Records what had focus before the dialog took it, to give it back on
    /// close.
    pub(crate) fn remember_focus(&mut self, previous: Option<FocusHandle>) {
        self.restore_focus = previous.filter(|previous| *previous != self.dialog_focus);
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

    /// Approves the spec at `path` at `shown`, the commit whose text the
    /// tab shows (spec 7: an approval names the commit its approver read).
    pub(crate) fn approve_spec(&mut self, path: &str, shown: GitSha, cx: &mut Context<Self>) {
        let action = Action::ApproveSpec {
            path: path.to_owned(),
            sha: shown,
        };
        self.act(action, ErrorSource::Spec(path.to_owned()), cx);
    }

    /// Opens the kick-off dialog, when the plan is ready to kick off.
    pub(crate) fn open_kickoff(&mut self, cx: &mut Context<Self>) {
        if self.plan.as_ref().is_some_and(crate::plan::kickoff_enabled) {
            self.kickoff_open = true;
            self.clear_error(&ErrorSource::KickOff);
            cx.notify();
        }
    }

    /// Closes the dialog and gives focus back to what had it before.
    pub(crate) fn close_kickoff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.kickoff_open = false;
        if let Some(previous) = self.restore_focus.take() {
            window.focus(&previous, cx);
        }
        cx.notify();
    }

    /// Kicks off the plan; the dialog closes when the backend takes it and
    /// stays open with the reason when it refuses.
    pub(crate) fn kick_off(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.act(Action::KickOff, ErrorSource::KickOff, cx);
        if self.error(&ErrorSource::KickOff).is_none() {
            self.close_kickoff(window, cx);
        }
    }

    fn clear_error(&mut self, source: &ErrorSource) {
        self.errors.retain(|(from, _)| from != source);
    }

    fn act(&mut self, action: Action, source: ErrorSource, cx: &mut Context<Self>) {
        let result = match cx.try_global::<BackendHandle>() {
            Some(backend) => backend.0.act(action),
            None => Err(BackendError::Unsupported("acting without a backend")),
        };
        match result {
            Ok(()) => self.clear_error(&source),
            Err(err) => {
                self.clear_error(&source);
                self.errors.push((source, err.to_string().into()));
            }
        }
        cx.notify();
    }
}
