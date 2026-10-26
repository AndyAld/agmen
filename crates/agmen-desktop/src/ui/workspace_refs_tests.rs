use gpui::{Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{FileText, chat_snapshot, fixture_id};
use crate::ui::left_dock::LeftDockTab;
use crate::ui::test_support::{init, redraw};

fn open_with(
    snapshot: Snapshot,
    cx: &mut TestAppContext,
) -> (Entity<Workspace>, &mut VisualTestContext) {
    init(cx);
    let (workspace, cx) = cx.add_window_view(|window, cx| Workspace::new(snapshot, window, cx));
    redraw(cx);
    (workspace, cx)
}

fn click_ref(workspace: &Entity<Workspace>, cx: &mut VisualTestContext, target: RefTarget) {
    workspace.update(cx, |w, cx| w.open_ref(&RefEvent::OpenRef(target), cx));
    redraw(cx);
}

/// Which `RefTarget` variant a target is. The match has no wildcard, so a
/// new variant does not compile until this test routes it.
const fn kind(target: &RefTarget) -> &'static str {
    match target {
        RefTarget::Agent(_) => "agent",
        RefTarget::Person(_) => "person",
        RefTarget::Role(_) => "role",
        RefTarget::Branch { .. } => "branch",
        RefTarget::Commit(_) => "commit",
        RefTarget::File { .. } => "file",
        RefTarget::Item(_) => "item",
        RefTarget::Lane(_) => "lane",
        RefTarget::Pr(_) => "pr",
    }
}

fn tab(item: TabItem) -> RefRoute {
    RefRoute::Tab { item, line: None }
}

fn git(handle: &str) -> RefRoute {
    RefRoute::Panel(PanelRoute::Git(fixture_id(handle)))
}

/// Every kind of reference, and each one the fixture cannot place.
fn cases() -> Vec<(RefTarget, RefRoute)> {
    let branch = |name: &str, at: Option<&str>| RefTarget::Branch {
        name: name.into(),
        at: at.map(Into::into),
    };
    vec![
        (
            RefTarget::Agent("andy/fx1".into()),
            tab(TabItem::Agent(fixture_id("andy/fx1"))),
        ),
        // An agent the roster does not have has no tab to open.
        (RefTarget::Agent("zed/x9".into()), RefRoute::HoverOnly),
        (
            RefTarget::Person("sean".into()),
            RefRoute::Panel(PanelRoute::Agents("sean".into())),
        ),
        (RefTarget::Person("nobody".into()), RefRoute::HoverOnly),
        (
            RefTarget::Role("reviewer".into()),
            tab(TabItem::RoleFile("reviewer".into())),
        ),
        (RefTarget::Role("no-such-role".into()), RefRoute::HoverOnly),
        (branch("test/b-415-replay-paging", None), git("andy/t7k")),
        (
            branch("fix/c015-main-conflict", Some("9e2a71c")),
            git("andy/fx1"),
        ),
        // A branch no local worktree has checked out.
        (branch("main", None), RefRoute::HoverOnly),
        // fx1's commit: fx1's first change's diff.
        (
            RefTarget::Commit("4d9fdc8".into()),
            tab(TabItem::Diff {
                worktree: "andy/fx1".into(),
                path: "src/main.rs".into(),
            }),
        ),
        // t7k's commit, in a worktree with no changes: its Commits list.
        (RefTarget::Commit("7c8835e".into()), git("andy/t7k")),
        (RefTarget::Commit("0123abc".into()), RefRoute::HoverOnly),
        (
            RefTarget::File {
                path: "src/main.rs".into(),
                lines: Some((2, 8)),
            },
            RefRoute::Tab {
                item: TabItem::File {
                    worktree: "andy/fx1".into(),
                    path: "src/main.rs".into(),
                },
                line: Some(2),
            },
        ),
        (
            RefTarget::File {
                path: "src/nowhere.rs".into(),
                lines: None,
            },
            RefRoute::HoverOnly,
        ),
        (RefTarget::Item("C015".into()), RefRoute::HoverOnly),
        (RefTarget::Lane("L-CORE".into()), RefRoute::HoverOnly),
        (RefTarget::Pr(9), RefRoute::HoverOnly),
    ]
}

#[test]
fn open_ref_routes_each_kind() {
    let snapshot = chat_snapshot();
    let cases = cases();
    let kinds: std::collections::BTreeSet<_> = cases.iter().map(|(t, _)| kind(t)).collect();
    assert_eq!(kinds.len(), 9, "every RefTarget variant has a case");
    for (target, expected) in cases {
        assert_eq!(route(&snapshot, &target), expected, "{target:?}");
    }
}

#[gpui::test]
fn open_ref_shows_each_route(cx: &mut TestAppContext) {
    let (workspace, cx) = open_with(chat_snapshot(), cx);
    let center = |cx: &mut VisualTestContext| {
        workspace.read_with(cx, |w, cx| {
            let group = w.pane_group.read(cx);
            (
                group.active_item(cx),
                group.panes()[0].read(cx).tabs().len(),
            )
        })
    };
    for (target, expected) in cases() {
        let (before_item, before_tabs) = center(cx);
        let before_left = workspace.read_with(cx, |w, _| w.shown_left_tab());
        click_ref(&workspace, cx, target.clone());
        match expected {
            RefRoute::Tab { item, .. } => {
                assert_eq!(center(cx).0, Some(item), "{target:?}");
            }
            RefRoute::Panel(PanelRoute::Agents(person)) => {
                workspace.read_with(cx, |w, cx| {
                    assert_eq!(w.shown_left_tab(), Some(LeftDockTab::Agents));
                    let panel = w.agents_panel.read(cx);
                    let mut shown = panel.visible_agents().peekable();
                    assert!(shown.peek().is_some(), "{person} has agents");
                    for agent in shown {
                        assert!(agent.handle.starts_with(&format!("{person}/")));
                    }
                });
            }
            RefRoute::Panel(PanelRoute::Git(owner)) => {
                workspace.read_with(cx, |w, cx| {
                    assert_eq!(w.shown_left_tab(), Some(LeftDockTab::Git));
                    let panel = w.left.git.read(cx);
                    assert_eq!(panel.scoped_owner(), Some(&owner), "{target:?}");
                    assert_eq!(panel.chip_owner(cx), Some(owner.clone()), "{target:?}");
                });
            }
            RefRoute::HoverOnly => {
                assert_eq!(center(cx), (before_item, before_tabs), "{target:?}");
                let left = workspace.read_with(cx, |w, _| w.shown_left_tab());
                assert_eq!(left, before_left, "{target:?}");
            }
        }
    }
}

#[gpui::test]
fn file_ref_opens_scrolled_to_its_line(cx: &mut TestAppContext) {
    let mut snapshot = chat_snapshot();
    let text: String = (1..=300).map(|n| format!("// line {n}\n")).collect();
    snapshot.worktrees[0].texts.insert(
        "src/main.rs".into(),
        FileText {
            text,
            marks: Vec::new(),
        },
    );
    let (workspace, cx) = open_with(snapshot, cx);
    let target = RefTarget::File {
        path: "src/main.rs".into(),
        lines: Some((150, 152)),
    };
    // A cmd-click: the split's new pane scrolls too.
    workspace.update(cx, |w, cx| {
        w.open_ref(&RefEvent::OpenRefInSplit(target), cx)
    });
    redraw(cx);
    redraw(cx);
    assert!(cx.debug_bounds("line:150").is_some(), "line 150 is drawn");
    assert!(
        cx.debug_bounds("line:149").is_none(),
        "line 150 is the top line"
    );
    let panes = workspace.read_with(cx, |w, cx| w.pane_group.read(cx).panes().len());
    assert_eq!(panes, 2, "a cmd-click opens a split");
}
