//! The agent menu's actions: Start, Attach Claude Code…, Detach and
//! Remove…. A child of `workspace`, so it sets the workspace's state
//! directly.
//!
//! agmend finds the folder of an agent it made. For a folder joined by
//! hand the workspace passes the folder the snapshot's worktrees name, or
//! asks the person to pick it; agmend checks its identity file either way.

use std::path::PathBuf;

use agmen_proto::IdentityId;
use gpui::{AppContext as _, Context, PathPromptOptions};

use super::Workspace;
use crate::backend::{Action, BackendHandle};
use crate::ui::remove_agent_dialog::{Closed, Kept, RemoveAgentDialog};

impl Workspace {
    pub(super) fn start_agent(&mut self, identity: &IdentityId, cx: &mut Context<Self>) {
        self.with_folder(identity, cx, |this, identity, root, cx| {
            this.act_on_agent(Action::StartAgent { identity, root }, cx);
        });
    }

    pub(super) fn attach_agent(&mut self, identity: &IdentityId, cx: &mut Context<Self>) {
        self.with_folder(identity, cx, |this, identity, root, cx| {
            this.act_on_agent(Action::AttachAgent { identity, root }, cx);
        });
    }

    pub(super) fn detach_agent(&mut self, identity: &IdentityId, cx: &mut Context<Self>) {
        self.act_on_agent(Action::DetachAgent(identity.clone()), cx);
    }

    /// Opens Remove…'s confirmation, which names the folder and branch
    /// that are kept.
    /// Unlike Start, never asks for a folder: agmend finds a hand-joined
    /// one itself, and removes a local agent whose folder is gone from its
    /// own record.
    pub(super) fn ask_remove(&mut self, identity: &IdentityId, cx: &mut Context<Self>) {
        let identity = identity.clone();
        let root = if self.snapshot.managed_agents.contains_key(&identity) {
            None
        } else {
            self.listed_root(&identity)
        };
        let kept = self.kept(&identity, root.as_ref());
        let handle = self
            .snapshot
            .roster
            .iter()
            .flat_map(|group| &group.agents)
            .find(|agent| agent.id == identity)
            .map_or_else(|| identity.to_string(), |agent| agent.handle.clone());
        let dialog = cx.new(|_| RemoveAgentDialog::new(identity, handle, root, kept));
        self.remove_agent_events = Some(cx.subscribe(&dialog, |this, _, _: &Closed, cx| {
            this.remove_agent = None;
            this.remove_agent_events = None;
            cx.notify();
        }));
        self.remove_agent = Some(dialog);
        cx.notify();
    }

    /// The folder the project's worktree list gives `identity`, if any.
    fn listed_root(&self, identity: &IdentityId) -> Option<PathBuf> {
        self.snapshot
            .worktrees
            .iter()
            .find(|worktree| worktree.owner == *identity)
            .and_then(|worktree| worktree.root.clone())
            .map(PathBuf::from)
    }

    /// What Remove keeps for `identity`, whose hand-joined folder is `root`.
    fn kept(&self, identity: &IdentityId, root: Option<&PathBuf>) -> Kept {
        let picked = root.map(|root| root.display().to_string());
        match self.snapshot.managed_work.get(identity) {
            Some(work) if work.joined => Kept::Joined {
                folder: work.worktree.clone().or(picked).unwrap_or_default(),
            },
            Some(work) => Kept::Launched {
                folder: work.worktree.clone(),
                branch: work.branch.clone(),
            },
            None => Kept::Joined {
                folder: picked.unwrap_or_default(),
            },
        }
    }

    /// Calls `then` with the folder agmend needs for `identity`: none for an
    /// agent it manages, else the folder its worktree lists, else one the
    /// person picks. Nothing happens when they cancel the picker.
    fn with_folder(
        &mut self,
        identity: &IdentityId,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, IdentityId, Option<PathBuf>, &mut Context<Self>) + 'static,
    ) {
        if self.snapshot.managed_agents.contains_key(identity) {
            return then(self, identity.clone(), None, cx);
        }
        if let Some(root) = self.listed_root(identity) {
            return then(self, identity.clone(), Some(root), cx);
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Pick the agent's folder".into()),
        });
        let identity = identity.clone();
        cx.spawn(async move |this, cx| {
            let folder = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    let why = format!("Opening the folder picker failed: {error}");
                    drop(this.update(cx, |this, cx| this.show_agent_error(why, cx)));
                    None
                }
                Err(_) => None,
            };
            if let Some(folder) = folder {
                // The workspace may have closed meanwhile.
                drop(this.update(cx, |this, cx| then(this, identity, Some(folder), cx)));
            }
        })
        .detach();
    }

    fn act_on_agent(&mut self, action: Action, cx: &mut Context<Self>) {
        let result = match cx.try_global::<BackendHandle>() {
            Some(backend) => backend.0.act(action),
            None => Err(crate::backend::BackendError::Unsupported(
                "agent actions without a backend",
            )),
        };
        if let Err(error) = result {
            self.show_agent_error(error.to_string(), cx);
        }
    }

    fn show_agent_error(&mut self, error: String, cx: &mut Context<Self>) {
        let managed = self.snapshot.managed_agents.clone();
        self.agents_panel
            .update(cx, |panel, cx| panel.set_managed(managed, Some(error), cx));
    }

    /// The open Remove… dialog's text.
    #[cfg(test)]
    pub(super) fn remove_message(&self, cx: &gpui::App) -> Option<String> {
        self.remove_agent
            .as_ref()
            .map(|dialog| dialog.read(cx).message())
    }
}

#[cfg(test)]
#[path = "workspace_agent_actions_tests.rs"]
mod tests;
