use std::path::PathBuf;

use super::super::Workspace;
use crate::backend::{Action, ManagedWork, Screen, Snapshot, chat_snapshot, fixture_id};
use crate::ui::plan_test_support::{Recording, plan_globals};
use crate::ui::test_support::{click, redraw};
use gpui::{Entity, TestAppContext, VisualTestContext};

fn open(
    cx: &mut TestAppContext,
    edit: impl FnOnce(&mut Snapshot),
) -> (
    std::rc::Rc<Recording>,
    Entity<Workspace>,
    &mut VisualTestContext,
) {
    let (backend, _) = plan_globals(cx, Screen::Plan, None);
    let mut snapshot = chat_snapshot();
    edit(&mut snapshot);
    let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(snapshot, window, cx));
    (backend, workspace, cx)
}

fn managed(snapshot: &mut Snapshot, handle: &str, state: &str) {
    snapshot
        .managed_agents
        .insert(fixture_id(handle), state.into());
}

/// Gives `handle`'s worktree a folder on disk, as agmend lists it.
fn folder(snapshot: &mut Snapshot, handle: &str, root: &str) {
    let id = fixture_id(handle);
    let worktree = snapshot
        .worktrees
        .iter_mut()
        .find(|worktree| worktree.owner == id)
        .unwrap();
    worktree.root = Some(root.into());
}

fn without_worktree(snapshot: &mut Snapshot, handle: &str) {
    let id = fixture_id(handle);
    snapshot.worktrees.retain(|worktree| worktree.owner != id);
}

#[gpui::test]
fn an_offline_agents_row_stays_inside_the_dock_and_clear_of_its_handle(cx: &mut TestAppContext) {
    // An offline managed row carries its state, a Stop and `…`; at the default
    // width they pushed `…` under the left dock's resize handle.
    let (_, _, cx) = open(cx, |s| managed(s, "andy/fx1", "offline"));
    redraw(cx);
    let more = cx.debug_bounds("agent:andy/fx1:more").unwrap();
    let handle = cx.debug_bounds("left-dock:resize").unwrap();
    assert!(
        more.right() <= handle.left(),
        "`…` ends at {:?}, under the handle from {:?}",
        more.right(),
        handle.left()
    );
}

#[gpui::test]
fn start_of_an_agent_agmend_knows_names_only_the_agent(cx: &mut TestAppContext) {
    let (backend, _, cx) = open(cx, |s| managed(s, "andy/fx1", "offline"));
    click(cx, "agent:andy/fx1:more");
    click(cx, "agent:andy/fx1:menu:start");
    assert_eq!(
        *backend.acts.borrow(),
        vec![Action::StartAgent {
            identity: fixture_id("andy/fx1"),
            root: None
        }]
    );
}

#[gpui::test]
fn start_of_a_hand_joined_agent_passes_its_folder(cx: &mut TestAppContext) {
    let (backend, _, cx) = open(cx, |s| folder(s, "andy/fx1", "/Users/andy/Marked-sol"));
    click(cx, "agent:andy/fx1:more");
    click(cx, "agent:andy/fx1:menu:start");
    assert_eq!(
        *backend.acts.borrow(),
        vec![Action::StartAgent {
            identity: fixture_id("andy/fx1"),
            root: Some(PathBuf::from("/Users/andy/Marked-sol"))
        }]
    );
}

#[gpui::test]
fn start_without_a_known_folder_asks_for_one(cx: &mut TestAppContext) {
    let (backend, _, cx) = open(cx, |s| without_worktree(s, "andy/fx1"));
    click(cx, "agent:andy/fx1:more");
    click(cx, "agent:andy/fx1:menu:start");
    assert!(cx.did_prompt_for_paths());
    assert!(backend.acts.borrow().is_empty());
    cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from("/picked/sol")]));
    cx.run_until_parked();
    assert_eq!(
        *backend.acts.borrow(),
        vec![Action::StartAgent {
            identity: fixture_id("andy/fx1"),
            root: Some(PathBuf::from("/picked/sol"))
        }]
    );
}

#[gpui::test]
fn attach_and_detach_reach_the_backend(cx: &mut TestAppContext) {
    let (backend, _, cx) = open(cx, |s| {
        managed(s, "andy/fx1", "idle");
        managed(s, "andy/rv2", "attached");
    });
    click(cx, "agent:andy/fx1:more");
    click(cx, "agent:andy/fx1:menu:attach");
    click(cx, "agent:andy/rv2:more");
    click(cx, "agent:andy/rv2:menu:detach");
    assert_eq!(
        *backend.acts.borrow(),
        vec![
            Action::AttachAgent {
                identity: fixture_id("andy/fx1"),
                root: None
            },
            Action::DetachAgent(fixture_id("andy/rv2")),
        ]
    );
}

#[gpui::test]
fn remove_asks_first_and_names_the_kept_folder_and_branch(cx: &mut TestAppContext) {
    let (backend, workspace, cx) = open(cx, |s| {
        managed(s, "andy/fx1", "idle");
        s.managed_work.insert(
            fixture_id("andy/fx1"),
            ManagedWork {
                worktree: Some("/repo/.worktrees/agent/fx1-1234".into()),
                branch: Some("agent/fx1-1234".into()),
                joined: false,
            },
        );
    });
    click(cx, "agent:andy/fx1:more");
    click(cx, "agent:andy/fx1:menu:remove");
    assert!(cx.debug_bounds("remove-agent").is_some());
    assert!(
        backend.acts.borrow().is_empty(),
        "nothing happens before Remove"
    );
    let message = workspace
        .read_with(cx, |w, cx| w.remove_message(cx))
        .unwrap();
    assert!(message.contains("andy/fx1"), "{message}");
    assert!(
        message.contains("/repo/.worktrees/agent/fx1-1234") && message.contains("agent/fx1-1234"),
        "{message}"
    );
    click(cx, "remove-agent:cancel");
    assert!(cx.debug_bounds("remove-agent").is_none());
    assert!(backend.acts.borrow().is_empty());

    click(cx, "agent:andy/fx1:more");
    click(cx, "agent:andy/fx1:menu:remove");
    click(cx, "remove-agent:confirm");
    // The fixture refuses managed agents, so the dialog stays with its error.
    assert!(cx.debug_bounds("remove-agent").is_some());
    assert_eq!(
        *backend.acts.borrow(),
        vec![Action::RemoveAgent {
            identity: fixture_id("andy/fx1"),
            root: None
        }]
    );
}

#[gpui::test]
fn remove_of_a_hand_joined_agent_says_its_folder_is_not_deleted(cx: &mut TestAppContext) {
    let (backend, workspace, cx) = open(cx, |s| folder(s, "andy/fx1", "/Users/andy/Marked-sol"));
    click(cx, "agent:andy/fx1:more");
    click(cx, "agent:andy/fx1:menu:remove");
    let message = workspace
        .read_with(cx, |w, cx| w.remove_message(cx))
        .unwrap();
    assert!(message.contains("/Users/andy/Marked-sol"), "{message}");
    assert!(message.contains("not deleted"), "{message}");
    click(cx, "remove-agent:confirm");
    assert_eq!(
        *backend.acts.borrow(),
        vec![Action::RemoveAgent {
            identity: fixture_id("andy/fx1"),
            root: Some(PathBuf::from("/Users/andy/Marked-sol"))
        }]
    );
}

#[gpui::test]
fn remove_without_a_known_folder_asks_for_one_first(cx: &mut TestAppContext) {
    let (backend, workspace, cx) = open(cx, |s| without_worktree(s, "andy/fx1"));
    click(cx, "agent:andy/fx1:more");
    click(cx, "agent:andy/fx1:menu:remove");
    assert!(cx.did_prompt_for_paths());
    assert!(cx.debug_bounds("remove-agent").is_none());
    cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from("/picked/sol")]));
    cx.run_until_parked();
    let message = workspace
        .read_with(cx, |w, cx| w.remove_message(cx))
        .unwrap();
    assert!(message.contains("/picked/sol"), "{message}");
    click(cx, "remove-agent:confirm");
    assert_eq!(
        *backend.acts.borrow(),
        vec![Action::RemoveAgent {
            identity: fixture_id("andy/fx1"),
            root: Some(PathBuf::from("/picked/sol"))
        }]
    );
}
