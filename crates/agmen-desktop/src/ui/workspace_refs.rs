//! Task 12 Step 1b: where a reference chip opens (spec 9.3). A child of
//! `workspace`, so it shows the workspace's panels and tabs.

use std::ops::RangeInclusive;

use agmen_proto::{GitSha, IdentityId};
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
    /// split. `lines` (1-based) scrolls a file tab to them and highlights
    /// them.
    Tab {
        item: TabItem,
        lines: Option<RangeInclusive<u32>>,
    },
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
    /// The Git panel scoped to this owner's worktree, with this commit's
    /// row selected.
    Git {
        owner: IdentityId,
        commit: Option<GitSha>,
    },
}

/// Where `target` opens, from what `snapshot` knows, for a chip in a
/// message `from` sent (the hub stamps `from`, so a sender can only point
/// at its own worktree). A target the snapshot cannot place shows its
/// hover card only, rather than an empty tab.
#[must_use]
pub(crate) fn route(snapshot: &Snapshot, target: &RefTarget, from: &IdentityId) -> RefRoute {
    let tab = |item| RefRoute::Tab { item, lines: None };
    let found = match target {
        // An agent with no session here has no view to open (ruling d):
        // spec 9.3's remote agent view is not built.
        RefTarget::Agent(handle) => snapshot
            .roster
            .iter()
            .flat_map(|group| &group.agents)
            .find(|agent| agent.handle == *handle && snapshot.sessions.contains_key(&agent.id))
            .map(|agent| tab(TabItem::Agent(agent.id.clone()))),
        // While there is a plan the Agents tab shows the planning room, so
        // folding the Agents panel would show nothing (ruling b).
        RefTarget::Person(name) => (snapshot.plan.is_none()
            && snapshot.roster.iter().any(|group| group.person == *name))
        .then(|| RefRoute::Panel(PanelRoute::Agents(name.clone()))),
        RefTarget::Role(name) => snapshot
            .roles
            .iter()
            .any(|role| role.name == *name)
            .then(|| tab(TabItem::RoleFile(name.clone()))),
        RefTarget::Branch { name, at } => {
            let on_branch =
                |wt: &&WorktreeInfo| matches!(&wt.head, WorktreeHead::Branch(b) if b == name);
            // `@sha` selects the commit, in the worktree on the branch when
            // it holds it (ruling a).
            at.as_ref()
                .and_then(|sha| commit_route(snapshot, sha, from, Some(name)))
                .or_else(|| {
                    snapshot
                        .worktrees
                        .iter()
                        .find(on_branch)
                        .map(|wt| git(wt, None))
                })
        }
        RefTarget::Commit(sha) => commit_route(snapshot, sha, from, None),
        RefTarget::File { path, lines } => {
            let has = |wt: &&WorktreeInfo| wt.files.contains(path);
            let mut holding = snapshot.worktrees.iter().filter(has);
            // shortcut: a sender with no worktree here (a person, another
            // machine's agent) opens the reader's own copy, which may be at
            // another version; spec 9.3 opens the file at the commit the
            // message names, which needs the live backend (ADR-0011) to read
            // a file at a commit.
            holding
                .clone()
                .find(|wt| wt.owner == *from)
                .or_else(|| holding.find(|wt| person(&wt.handle) == snapshot.me))
                .map(|wt| RefRoute::Tab {
                    item: TabItem::File {
                        worktree: wt.handle.clone(),
                        path: path.clone(),
                    },
                    lines: lines.map(|(first, last)| first..=first.max(last)),
                })
        }
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

fn git(wt: &WorktreeInfo, commit: Option<GitSha>) -> RefRoute {
    RefRoute::Panel(PanelRoute::Git {
        owner: wt.owner.clone(),
        commit,
    })
}

/// A commit in the Git panel of a worktree that holds it, its row
/// selected (ruling a). Of several, the one on `branch` wins, then the
/// sender's, then the reader's own, then snapshot order. A stored sha and
/// the named one match when either starts with the other, as git
/// abbreviates.
// shortcut: no per-commit diff; the worktree's change is not the commit's
// (9e2a71c and 4d9fdc8 would show the same diff), so a commit shows its
// row instead. Once the backend gives a commit its own hunks, a commit
// opens those.
fn commit_route(
    snapshot: &Snapshot,
    sha: &GitSha,
    from: &IdentityId,
    branch: Option<&str>,
) -> Option<RefRoute> {
    let named = sha.as_str();
    let (wt, stored) = snapshot
        .worktrees
        .iter()
        .filter_map(|wt| {
            let held = wt.commits.iter().find(|c| {
                let stored = c.sha.as_str();
                stored.starts_with(named) || named.starts_with(stored)
            })?;
            Some((wt, &held.sha))
        })
        .min_by_key(|(wt, _)| {
            let on_branch =
                branch.is_some_and(|b| matches!(&wt.head, WorktreeHead::Branch(h) if h == b));
            (
                !on_branch,
                wt.owner != *from,
                person(&wt.handle) != snapshot.me,
            )
        })?;
    Some(git(wt, Some(stored.clone())))
}

impl Workspace {
    /// Opens a reference chip's target where [`route`] says: a tab in the
    /// active pane on a click, in a split on a cmd-click, or a left dock
    /// panel either way.
    pub(super) fn open_ref(&mut self, event: &RefEvent, cx: &mut Context<Self>) {
        let (target, from, in_split) = match event {
            RefEvent::OpenRef { target, from } => (target, from, false),
            RefEvent::OpenRefInSplit { target, from } => (target, from, true),
        };
        match route(&self.snapshot, target, from) {
            RefRoute::Tab { item, lines } => {
                self.pane_group.update(cx, |group, cx| {
                    if in_split {
                        group.open_in_split(item.clone(), cx);
                    } else {
                        group.open(item.clone(), cx);
                    }
                    if let Some(lines) = lines {
                        group.show_active_lines(lines, cx);
                    }
                });
                // The right dock shows what the tab is about, as frames 479
                // (an agent) and 11514 (a role) do.
                match &item {
                    TabItem::Agent(id) => self.select_agent(id, cx),
                    // route found the role, so it has a panel to show.
                    TabItem::RoleFile(name) => {
                        self.show_role(name, cx);
                    }
                    _ => {}
                }
            }
            RefRoute::Panel(PanelRoute::Agents(person)) => {
                self.show_left_tab(LeftDockTab::Agents, cx);
                self.agents_panel
                    .update(cx, |panel, cx| panel.show_only(&person, cx));
            }
            RefRoute::Panel(PanelRoute::Git { owner, commit }) => {
                self.show_left_tab(LeftDockTab::Git, cx);
                self.left.git.update(cx, |panel, cx| {
                    panel.switch(&owner, cx);
                    if let Some(commit) = &commit {
                        panel.select_commit(commit, cx);
                    }
                });
            }
            RefRoute::HoverOnly => {}
        }
    }
}

#[cfg(test)]
#[path = "workspace_refs_tests.rs"]
mod tests;
