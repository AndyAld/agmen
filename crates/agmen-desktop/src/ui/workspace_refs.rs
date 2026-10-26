//! Task 12 Step 1b: where a reference chip opens (spec 9.3). A child of
//! `workspace`, so it shows the workspace's panels and tabs.

use agmen_proto::IdentityId;
use gpui::Context;

use super::Workspace;
use crate::backend::{Snapshot, WorktreeHead, WorktreeInfo};
use crate::refs::RefTarget;
use crate::ui::left_dock::LeftDockTab;
use crate::ui::pane::TabItem;
use crate::ui::ref_chip::RefEvent;

/// Where a reference chip opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefRoute {
    /// A center tab: a click opens it in the active pane, a cmd-click in a
    /// split. `line` (1-based) scrolls a file tab so that line is on top.
    Tab { item: TabItem, line: Option<u32> },
    /// A left dock panel.
    Panel(PanelRoute),
    /// Nothing opens: the chip's hover card is all it shows.
    HoverOnly,
}

/// The left dock panel a reference shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PanelRoute {
    /// The Agents panel showing only this person's agents.
    Agents(String),
    /// The Git panel scoped to this owner's worktree.
    Git(IdentityId),
}

/// Where `target` opens, from what `snapshot` knows. A target the
/// snapshot cannot place shows its hover card only, rather than an empty
/// tab.
#[must_use]
pub(crate) fn route(snapshot: &Snapshot, target: &RefTarget) -> RefRoute {
    let tab = |item| RefRoute::Tab { item, line: None };
    let found = match target {
        RefTarget::Agent(handle) => snapshot
            .roster
            .iter()
            .flat_map(|group| &group.agents)
            .find(|agent| agent.handle == *handle)
            .map(|agent| tab(TabItem::Agent(agent.id.clone()))),
        RefTarget::Person(name) => snapshot
            .roster
            .iter()
            .any(|group| group.person == *name)
            .then(|| RefRoute::Panel(PanelRoute::Agents(name.clone()))),
        RefTarget::Role(name) => snapshot
            .roles
            .iter()
            .any(|role| role.name == *name)
            .then(|| tab(TabItem::RoleFile(name.clone()))),
        // The branch's name finds its worktree; a `@sha` after it does not
        // change which one.
        RefTarget::Branch { name, .. } => snapshot
            .worktrees
            .iter()
            .find(|wt| matches!(&wt.head, WorktreeHead::Branch(b) if b == name))
            .map(|wt| RefRoute::Panel(PanelRoute::Git(wt.owner.clone()))),
        RefTarget::Commit(sha) => commit_route(&snapshot.worktrees, sha),
        RefTarget::File { path, lines } => snapshot
            .worktrees
            .iter()
            .filter(|wt| person(&wt.handle) == snapshot.me)
            .find(|wt| wt.files.contains(path))
            .map(|wt| RefRoute::Tab {
                item: TabItem::File {
                    worktree: wt.handle.clone(),
                    path: path.clone(),
                },
                line: lines.map(|(from, _)| from),
            }),
        // shortcut: items, lanes and PRs show their hover card only; the
        // board (items and lanes) and the GitHub App (PR cards, spec 10)
        // are later plans, and each gives its kind a place to open.
        RefTarget::Item(_) | RefTarget::Lane(_) | RefTarget::Pr(_) => None,
    };
    found.unwrap_or(RefRoute::HoverOnly)
}

/// The person part of a handle: `andy` of `andy/fx1`.
fn person(handle: &str) -> &str {
    handle.split_once('/').map_or(handle, |(person, _)| person)
}

/// A commit's diff: the first worktree whose branch has it (the person's
/// own come first), on its first change's diff. The fixture's hunks are
/// per changed file, not per commit, so that diff is the worktree's
/// change, which may hold more than the commit did.
// shortcut: no per-commit diff; once the backend gives a commit its own
// hunks, a commit opens those instead.
fn commit_route(worktrees: &[WorktreeInfo], sha: &str) -> Option<RefRoute> {
    let wt = worktrees.iter().find(|wt| {
        wt.commits.iter().any(|c| {
            let full = c.sha.as_str();
            full.starts_with(sha) || sha.starts_with(full)
        })
    })?;
    Some(wt.changes.first().map_or_else(
        // A worktree with no changes shows the commit in its Commits list.
        || RefRoute::Panel(PanelRoute::Git(wt.owner.clone())),
        |change| RefRoute::Tab {
            item: TabItem::Diff {
                worktree: wt.handle.clone(),
                path: change.path.clone(),
            },
            line: None,
        },
    ))
}

impl Workspace {
    /// Opens a reference chip's target where [`route`] says: a tab in the
    /// active pane on a click, in a split on a cmd-click, or a left dock
    /// panel either way.
    pub(super) fn open_ref(&mut self, event: &RefEvent, cx: &mut Context<Self>) {
        let (target, in_split) = match event {
            RefEvent::OpenRef(target) => (target, false),
            RefEvent::OpenRefInSplit(target) => (target, true),
        };
        match route(&self.snapshot, target) {
            RefRoute::Tab { item, line } => self.pane_group.update(cx, |group, cx| {
                if in_split {
                    group.open_in_split(item, cx);
                } else {
                    group.open(item, cx);
                }
                if let Some(line) = line {
                    group.scroll_active_to_line(line, cx);
                }
            }),
            RefRoute::Panel(PanelRoute::Agents(person)) => {
                self.show_left_tab(LeftDockTab::Agents, cx);
                self.agents_panel
                    .update(cx, |panel, cx| panel.show_only(&person, cx));
            }
            RefRoute::Panel(PanelRoute::Git(owner)) => {
                self.show_left_tab(LeftDockTab::Git, cx);
                self.left
                    .git
                    .update(cx, |panel, cx| panel.switch(&owner, cx));
            }
            RefRoute::HoverOnly => {}
        }
    }
}

#[cfg(test)]
#[path = "workspace_refs_tests.rs"]
mod tests;
