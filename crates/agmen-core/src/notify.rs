//! Project wakes and the committed outbox signal.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use agmen_proto::ProjectId;
use tokio::sync::{Notify, watch};

/// A watch per project that readers sleep on until a commit in their project.
#[derive(Debug, Default)]
pub struct Wakes {
    by_project: Mutex<HashMap<ProjectId, watch::Sender<()>>>,
}

impl Wakes {
    /// Wakes `project`'s streams. A project nobody watches any more loses
    /// its entry here, so the map holds only watched projects.
    pub fn wake(&self, project: &ProjectId) {
        let mut by_project = self
            .by_project
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(sender) = by_project.get(project)
            && sender.send(()).is_err()
        {
            by_project.remove(project);
        }
    }

    /// A receiver that sees a change after each [`Wakes::wake`] of `project`.
    #[must_use]
    pub fn watch(&self, project: &ProjectId) -> watch::Receiver<()> {
        self.by_project
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(project.clone())
            .or_default()
            .subscribe()
    }

    /// How many projects have an entry.
    #[must_use]
    pub fn watched(&self) -> usize {
        self.by_project
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// Wakes the outbox publisher and the readers of a committed project's messages.
#[derive(Debug, Default)]
pub struct Notifier {
    outbox_ready: Notify,
    stored: Wakes,
    #[cfg(test)]
    stored_count: std::sync::atomic::AtomicUsize,
}

impl Notifier {
    #[cfg(test)]
    pub(crate) fn stored_count(&self) -> usize {
        self.stored_count.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Signalled after each commit that added outbox rows.
    #[must_use]
    pub fn outbox_ready(&self) -> &Notify {
        &self.outbox_ready
    }

    /// Wakes the publisher and `project`'s readers after a message commit.
    pub fn stored(&self, project: &ProjectId) {
        #[cfg(test)]
        self.stored_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.outbox_ready.notify_one();
        self.stored.wake(project);
    }

    /// A receiver that sees a change after each [`Notifier::stored`] for `project`.
    #[must_use]
    pub fn watch_stored(&self, project: &ProjectId) -> watch::Receiver<()> {
        self.stored.watch(project)
    }
}

#[cfg(test)]
#[path = "notify_tests.rs"]
mod tests;
