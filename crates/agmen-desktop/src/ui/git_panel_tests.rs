use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::chat_snapshot;
use crate::ui::scope_chip::ScopeOption;
use crate::ui::test_support::{click, init, redraw};

fn panel(cx: &mut TestAppContext) -> (Entity<GitPanel>, &mut VisualTestContext) {
    init(cx);
    let snapshot = chat_snapshot();
    cx.add_window_view(move |_, cx| {
        let options = ScopeOption::for_worktrees(&snapshot.worktrees, &snapshot.agents);
        GitPanel::new(
            snapshot.worktrees.clone(),
            snapshot.git.branches.clone(),
            options,
            cx,
        )
    })
}

#[gpui::test]
fn scope_chip_scopes_the_git_panel(cx: &mut TestAppContext) {
    // The merge of Tasks 11 and 12: the chip switches which worktree's
    // changes and commits the panel shows.
    let (panel, cx) = panel(cx);
    redraw(cx);
    assert!(cx.debug_bounds("scope-chip").is_some());
    let t7k = crate::backend::fixture_id("andy/t7k");
    panel.update(cx, |p, cx| p.switch(&t7k, cx));
    panel.read_with(cx, |p, _| {
        assert_eq!(p.badge(GitSection::Changes), "0");
        assert_eq!(p.badge(GitSection::Commits), "test/b-415");
        assert_eq!(
            p.badge(GitSection::Worktrees),
            "5",
            "the list is not scoped"
        );
    });
}

#[gpui::test]
fn git_sections_counts(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    redraw(cx);
    panel.read_with(cx, |p, _| {
        assert_eq!(p.badge(GitSection::Changes), "2");
        assert_eq!(p.badge(GitSection::Worktrees), "5");
        assert_eq!(p.badge(GitSection::Branches), "14");
        // The mockup's Commits header names the branch instead of a count.
        assert_eq!(p.badge(GitSection::Commits), "fix/c015");
    });
    for sel in [
        "git-count:changes",
        "git-count:commits",
        "git-count:worktrees",
        "git-count:branches",
    ] {
        assert!(cx.debug_bounds(sel).is_some(), "{sel}");
    }
    for sel in [
        "git-change:src/main.rs",
        "git-change:Cargo.lock",
        "git-commit:9e2a71c",
        "git-commit:016f235",
        "git-person:andy",
        "git-worktree:andy/fx1",
        "git-worktree:andy/t7k",
        "git-worktree:andy/rv2",
        "git-person:sean",
    ] {
        assert!(cx.debug_bounds(sel).is_some(), "{sel}");
    }
}

#[gpui::test]
fn collapsed_state_hides_rows(cx: &mut TestAppContext) {
    let (_, cx) = panel(cx);
    redraw(cx);
    // Branches and other people's worktrees start folded, as in the mockup.
    assert!(cx.debug_bounds("git-branch:main").is_none());
    assert!(cx.debug_bounds("git-worktree:sean/a1").is_none());
    click(cx, "git-section:changes");
    assert!(cx.debug_bounds("git-change:src/main.rs").is_none());
    assert!(cx.debug_bounds("git-commit:9e2a71c").is_some());
    click(cx, "git-section:changes");
    assert!(cx.debug_bounds("git-change:src/main.rs").is_some());
    click(cx, "git-section:branches");
    assert!(cx.debug_bounds("git-branch:main").is_some());
    click(cx, "git-person:sean");
    assert!(cx.debug_bounds("git-worktree:sean/a1").is_some());
    assert!(cx.debug_bounds("git-worktree:sean/plan").is_some());
    click(cx, "git-section:worktrees");
    assert!(cx.debug_bounds("git-person:andy").is_none());
    assert!(cx.debug_bounds("git-worktree:sean/a1").is_none());
}

#[gpui::test]
fn selecting_change_opens_diff_tab(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &GitPanelEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    click(cx, "git-change:Cargo.lock");
    assert_eq!(
        *opened.borrow(),
        vec![GitPanelEvent::OpenDiff {
            worktree: "andy/fx1".into(),
            path: "Cargo.lock".into(),
        }]
    );
    panel.read_with(cx, |p, _| assert_eq!(p.selected(), Some("Cargo.lock")));
}

#[test]
fn worktree_rows_name_head_and_agent() {
    let worktrees = chat_snapshot().worktrees;
    let rv2 = worktrees.iter().find(|w| w.handle == "andy/rv2").unwrap();
    // Mockup fix 51: rv2 reads C015 detached at fx1's commit.
    assert_eq!(rv2.head.label(), "detached @9e2a71c");
    assert_eq!(rv2.state, WorktreeState::Reading);
    assert_eq!(agent_name(&rv2.handle), "rv2");
    assert_eq!(person(&rv2.handle), "andy");
}

#[test]
fn commit_tooltip_shows_author_and_local_time() {
    let fx1 = chat_snapshot().worktrees.into_iter().next().unwrap();
    let denver = jiff::tz::TimeZone::get("America/Denver").unwrap();
    let merge = fx1.commits.get(1).unwrap();
    assert_eq!(commit_tooltip(merge, &denver), "sean/a1 · 14:58");
    // The person's own agents drop the person, as the row does.
    assert_eq!(commit_author("andy/fx1", "andy/fx1"), "fx1");
    assert_eq!(commit_author("sean/a1", "andy/fx1"), "sean/a1");
}

#[test]
fn commits_header_cuts_branch_to_its_item() {
    assert_eq!(branch_item("fix/c015-main-conflict"), "fix/c015");
    assert_eq!(branch_item("test/b-415-replay-paging"), "test/b-415");
    assert_eq!(branch_item("plan/ridgeline"), "plan/ridgeline");
    assert_eq!(branch_item("main"), "main");
    assert_eq!(branch_item("fix/-12"), "fix/-12");
}
