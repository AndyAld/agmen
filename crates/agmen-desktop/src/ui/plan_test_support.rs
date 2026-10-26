//! Task 19 test helpers: a backend that records what the views ask for,
//! and the planning room's globals.

use std::cell::RefCell;
use std::rc::Rc;

use futures::channel::mpsc::UnboundedReceiver;
use gpui::{Entity, TestAppContext};

use super::plan_model::PlanModel;
use crate::backend::{
    Action, Backend, BackendError, BackendHandle, Draft, FixtureBackend, Screen, Snapshot, Update,
};

/// The fixture for a screen, recording every action before passing it on.
pub(crate) struct Recording {
    inner: FixtureBackend,
    pub(crate) acts: RefCell<Vec<Action>>,
}

impl Backend for Recording {
    fn snapshot(&self) -> Snapshot {
        self.inner.snapshot()
    }

    fn send(&self, draft: Draft) -> Result<(), BackendError> {
        self.inner.send(draft)
    }

    fn act(&self, action: Action) -> Result<(), BackendError> {
        self.acts.borrow_mut().push(action.clone());
        self.inner.act(action)
    }

    fn subscribe(&self) -> UnboundedReceiver<Update> {
        self.inner.subscribe()
    }
}

/// Sets the view globals, a recording backend for `screen` and the plan
/// model over `snapshot` (the screen's own unless a test changed it).
pub(crate) fn plan_globals(
    cx: &mut TestAppContext,
    screen: Screen,
    snapshot: Option<Snapshot>,
) -> (Rc<Recording>, Entity<PlanModel>) {
    super::test_support::init(cx);
    let recording = Rc::new(Recording {
        inner: FixtureBackend::for_screen(screen).unwrap(),
        acts: RefCell::new(Vec::new()),
    });
    let snapshot = snapshot.unwrap_or_else(|| recording.snapshot());
    let backend: Rc<dyn Backend> = recording.clone();
    let model = cx.update(|cx| {
        cx.set_global(BackendHandle(backend));
        PlanModel::install(&snapshot, cx).unwrap()
    });
    (recording, model)
}
