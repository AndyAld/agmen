use gpui::{Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{Backend as _, FileText, FixtureBackend, Screen, chat_snapshot, fixture_id};
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

fn click_ref(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
    target: RefTarget,
    from: IdentityId,
) {
    workspace.update(cx, |w, cx| {
        w.open_ref(&RefEvent::OpenRef { target, from }, cx)
    });
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
    RefRoute::Tab { item, lines: None }
}

fn sha(text: &str) -> GitSha {
    text.parse().unwrap()
}

fn git(handle: &str, commit: Option<&str>) -> RefRoute {
    RefRoute::Panel(PanelRoute::Git {
        owner: fixture_id(handle),
        commit: commit.map(sha),
    })
}

fn file(path: &str, lines: Option<(u32, u32)>) -> RefTarget {
    RefTarget::File {
        path: path.into(),
        lines,
    }
}

fn file_tab(worktree: &str, path: &str, lines: Option<RangeInclusive<u32>>) -> RefRoute {
    RefRoute::Tab {
        item: TabItem::File {
            worktree: worktree.into(),
            path: path.into(),
        },
        lines,
    }
}

/// `9e2a71c` as the live backend would store it: 40 digits.
const FULL_9E2A71C: &str = "9e2a71c0123456789abcdef0123456789abcdef0";

/// Every kind of reference, who sent it, and where it opens; and each one
/// the fixture cannot place.
fn cases() -> Vec<(RefTarget, IdentityId, RefRoute)> {
    // andy/a1 posts the fixture's ALERT and has no worktree here.
    let (a1, t7k, rv2, sean) = (
        fixture_id("andy/a1"),
        fixture_id("andy/t7k"),
        fixture_id("andy/rv2"),
        fixture_id("sean"),
    );
    let mut cases = vec![
        (
            RefTarget::Agent("andy/fx1".into()),
            a1.clone(),
            tab(TabItem::Agent(fixture_id("andy/fx1"))),
        ),
        // Ruling (d): a roster agent with no session here has no view, so
        // it shows its hover card rather than an empty tab (C291).
        (
            RefTarget::Agent("sean/a1".into()),
            a1.clone(),
            RefRoute::HoverOnly,
        ),
        // An agent the roster does not have has no tab to open.
        (
            RefTarget::Agent("zed/x9".into()),
            a1.clone(),
            RefRoute::HoverOnly,
        ),
        (
            RefTarget::Person("sean".into()),
            a1.clone(),
            RefRoute::Panel(PanelRoute::Agents("sean".into())),
        ),
        (
            RefTarget::Person("nobody".into()),
            a1.clone(),
            RefRoute::HoverOnly,
        ),
        (
            RefTarget::Role("reviewer".into()),
            a1.clone(),
            tab(TabItem::RoleFile("reviewer".into())),
        ),
        (
            RefTarget::Role("no-such-role".into()),
            a1.clone(),
            RefRoute::HoverOnly,
        ),
        // C288: a file opens in the sender's worktree...
        (
            file("src/main.rs", Some((2, 8))),
            t7k.clone(),
            file_tab("andy/t7k", "src/main.rs", Some(2..=8)),
        ),
        // ...and a sender with none here, a person or an agent elsewhere,
        // opens the reader's own copy.
        (
            file("src/main.rs", Some((2, 8))),
            sean.clone(),
            file_tab("andy/fx1", "src/main.rs", Some(2..=8)),
        ),
        (
            file("src/main.rs", None),
            a1.clone(),
            file_tab("andy/fx1", "src/main.rs", None),
        ),
        // C313: a reversed range (src/main.rs:8-2) highlights its first
        // line rather than nothing.
        (
            file("src/main.rs", Some((8, 2))),
            t7k.clone(),
            file_tab("andy/t7k", "src/main.rs", Some(8..=8)),
        ),
        (file("src/nowhere.rs", None), t7k, RefRoute::HoverOnly),
        (
            RefTarget::Item("C015".into()),
            a1.clone(),
            RefRoute::HoverOnly,
        ),
        (
            RefTarget::Lane("L-CORE".into()),
            a1.clone(),
            RefRoute::HoverOnly,
        ),
        (RefTarget::Pr(9), a1.clone(), RefRoute::HoverOnly),
    ];
    cases.extend(git_cases(&a1, &rv2));
    cases
}

/// Branches and commits, from andy/a1 unless a case says otherwise.
fn git_cases(a1: &IdentityId, rv2: &IdentityId) -> Vec<(RefTarget, IdentityId, RefRoute)> {
    let branch = |name: &str, at: Option<&str>| RefTarget::Branch {
        name: name.into(),
        at: at.map(sha),
    };
    let commit = |text: &str| RefTarget::Commit(sha(text));
    vec![
        (
            branch("test/b-415-replay-paging", None),
            a1.clone(),
            git("andy/t7k", None),
        ),
        // Ruling (a): branch@sha selects the commit in the worktree on the
        // branch.
        (
            branch("fix/c015-main-conflict", Some("9e2a71c")),
            a1.clone(),
            git("andy/fx1", Some("9e2a71c")),
        ),
        // C313: fx1 is on the branch and rv2 sent it; both hold 9e2a71c,
        // and the branch outranks the sender.
        (
            branch("fix/c015-main-conflict", Some("9e2a71c")),
            rv2.clone(),
            git("andy/fx1", Some("9e2a71c")),
        ),
        // A branch no local worktree has checked out.
        (branch("main", None), a1.clone(), RefRoute::HoverOnly),
        // Ruling (a): a commit shows in the Git panel of the worktree that
        // holds it, its row selected, not as the worktree's whole change.
        (
            commit("4d9fdc8"),
            a1.clone(),
            git("andy/fx1", Some("4d9fdc8")),
        ),
        (
            commit("7c8835e"),
            a1.clone(),
            git("andy/t7k", Some("7c8835e")),
        ),
        // fx1 and rv2 both hold 9e2a71c: the sender's worktree wins (C289).
        (
            commit("9e2a71c"),
            rv2.clone(),
            git("andy/rv2", Some("9e2a71c")),
        ),
        (
            commit("9e2a71c"),
            a1.clone(),
            git("andy/fx1", Some("9e2a71c")),
        ),
        // A full sha finds the 7-digit one git abbreviated (C292).
        (
            commit(FULL_9E2A71C),
            a1.clone(),
            git("andy/fx1", Some("9e2a71c")),
        ),
        (commit("0123abc"), a1.clone(), RefRoute::HoverOnly),
    ]
}

#[test]
fn open_ref_routes_each_kind() {
    let snapshot = chat_snapshot();
    let cases = cases();
    let kinds: std::collections::BTreeSet<_> = cases.iter().map(|(t, _, _)| kind(t)).collect();
    assert_eq!(kinds.len(), 9, "every RefTarget variant has a case");
    for (target, from, expected) in cases {
        assert_eq!(
            route(&snapshot, &target, &from),
            expected,
            "{target:?} from {from}"
        );
    }
}

#[test]
fn file_only_another_persons_worktree_has_opens_for_its_sender_only() {
    // C292: sean/a1's own file opens in sean/a1's copy when sean/a1 sent
    // it; from sean, who has no worktree, the reader's own copies are the
    // fallback, and none of them has it.
    let mut snapshot = chat_snapshot();
    let sean_a1 = snapshot
        .worktrees
        .iter_mut()
        .find(|wt| wt.handle == "sean/a1")
        .unwrap();
    sean_a1.files.push("src/sean_only.rs".into());
    let target = file("src/sean_only.rs", Some((3, 3)));
    assert_eq!(
        route(&snapshot, &target, &fixture_id("sean/a1")),
        file_tab("sean/a1", "src/sean_only.rs", Some(3..=3)),
    );
    assert_eq!(
        route(&snapshot, &target, &fixture_id("sean")),
        RefRoute::HoverOnly,
    );
}

#[test]
fn short_commit_ref_finds_a_full_stored_sha() {
    // C292: the live backend stores 40 digits; a message names 7.
    let mut snapshot = chat_snapshot();
    let fx1 = &mut snapshot.worktrees[0];
    assert_eq!(fx1.handle, "andy/fx1");
    fx1.commits[0].sha = sha(FULL_9E2A71C);
    let rv2 = snapshot
        .worktrees
        .iter_mut()
        .find(|wt| wt.handle == "andy/rv2")
        .unwrap();
    rv2.commits.clear();
    assert_eq!(
        route(
            &snapshot,
            &RefTarget::Commit(sha("9e2a71c")),
            &fixture_id("andy/a1")
        ),
        git("andy/fx1", Some(FULL_9E2A71C)),
    );
}

#[test]
fn commit_prefers_my_worktree_over_another_persons() {
    // V129: with another person's worktree listed first, the reader's own
    // copy of a commit still wins when the sender has none.
    let mut snapshot = chat_snapshot();
    snapshot.worktrees.reverse();
    // sean/a1 holds a41c0d2; give andy/t7k a copy too.
    let a41 = sha("a41c0d2");
    let held = |wt: &crate::backend::WorktreeInfo| wt.commits.iter().any(|c| c.sha == a41);
    let commit = snapshot
        .worktrees
        .iter()
        .find(|wt| wt.handle == "sean/a1" && held(wt))
        .map(|wt| wt.commits[0].clone())
        .unwrap();
    let t7k = snapshot
        .worktrees
        .iter_mut()
        .find(|wt| wt.handle == "andy/t7k")
        .unwrap();
    t7k.commits.push(commit);
    let target = RefTarget::Commit(a41);
    assert_eq!(
        route(&snapshot, &target, &fixture_id("andy/a1")),
        git("andy/t7k", Some("a41c0d2")),
    );
    assert_eq!(
        route(&snapshot, &target, &fixture_id("sean/a1")),
        git("sean/a1", Some("a41c0d2")),
    );
}

#[test]
fn person_ref_during_a_plan_shows_its_card_only() {
    // Ruling (b): the Agents tab shows the planning room while there is a
    // plan, so folding the Agents panel would change nothing visible.
    let plan = FixtureBackend::for_screen(Screen::Plan).unwrap().snapshot();
    assert!(plan.plan.is_some());
    let person = plan.roster[0].person.clone();
    let from = fixture_id("andy/a1");
    assert_eq!(
        route(&plan, &RefTarget::Person(person), &from),
        RefRoute::HoverOnly
    );
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
    for (target, from, expected) in cases() {
        // Every route starts from the Project tab, so a panel route must
        // switch the tab itself (C292: Agents is the default tab).
        workspace.update(cx, |w, cx| w.show_left_tab(LeftDockTab::Project, cx));
        redraw(cx);
        let (before_item, before_tabs) = center(cx);
        let before_left = workspace.read_with(cx, |w, _| w.shown_left_tab());
        click_ref(&workspace, cx, target.clone(), from);
        match expected {
            RefRoute::Tab { item, .. } => {
                assert_eq!(center(cx).0, Some(item.clone()), "{target:?}");
                workspace.read_with(cx, |w, cx| match &item {
                    // C291: the right dock shows the agent, as frame 479.
                    // C319: on the Agents tab, so its row's highlight shows.
                    TabItem::Agent(id) => {
                        assert_eq!(w.shown_agent().map(|a| &a.id), Some(id), "{target:?}");
                        assert_eq!(w.shown_left_tab(), Some(LeftDockTab::Agents));
                    }
                    // C290: and the role's panel, as frame 11514, with the
                    // Roles tab showing its row selected (C319).
                    TabItem::RoleFile(name) => {
                        let shown = w.left.shown_role.as_ref().map(|r| r.name.as_str());
                        assert_eq!(shown, Some(name.as_str()), "{target:?}");
                        assert_eq!(w.shown_left_tab(), Some(LeftDockTab::Roles));
                        let row = w.left.roles.read(cx).selected().map(str::to_owned);
                        assert_eq!(row.as_deref(), Some(name.as_str()), "{target:?}");
                    }
                    _ => {}
                });
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
            RefRoute::Panel(PanelRoute::Git { owner, commit }) => {
                workspace.read_with(cx, |w, cx| {
                    assert_eq!(w.shown_left_tab(), Some(LeftDockTab::Git));
                    let panel = w.left.git.read(cx);
                    assert_eq!(panel.scoped_owner(), Some(&owner), "{target:?}");
                    assert_eq!(panel.chip_owner(cx), Some(owner.clone()), "{target:?}");
                    assert_eq!(panel.selected_commit(), commit.as_ref(), "{target:?}");
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

/// Lines of `view`'s code drawn above and below line `line`.
fn drawn_around(cx: &mut VisualTestContext, line: u32, last: u32) -> (u32, u32) {
    let mut drawn = |n: u32| {
        let selector: &'static str = Box::leak(format!("line:{n}").into_boxed_str());
        cx.debug_bounds(selector).is_some()
    };
    let above = (1..line).filter(|&n| drawn(n)).count();
    let below = (line + 1..=last).filter(|&n| drawn(n)).count();
    (u32::try_from(above).unwrap(), u32::try_from(below).unwrap())
}

#[gpui::test]
fn file_ref_opens_centered_on_its_highlighted_lines(cx: &mut TestAppContext) {
    let mut snapshot = chat_snapshot();
    let text: String = (1..=300).map(|n| format!("// line {n}\n")).collect();
    let t7k = snapshot
        .worktrees
        .iter_mut()
        .find(|wt| wt.handle == "andy/t7k")
        .unwrap();
    t7k.texts.insert(
        "src/main.rs".into(),
        FileText {
            text,
            marks: Vec::new(),
        },
    );
    let (workspace, cx) = open_with(snapshot, cx);
    let target = file("src/main.rs", Some((150, 152)));
    // A cmd-click from andy/t7k: the split's new pane shows t7k's copy.
    workspace.update(cx, |w, cx| {
        let from = fixture_id("andy/t7k");
        w.open_ref(&RefEvent::OpenRefInSplit { target, from }, cx);
    });
    redraw(cx);
    redraw(cx);
    let active = workspace.read_with(cx, |w, cx| w.pane_group.read(cx).active_item(cx));
    assert_eq!(
        active,
        Some(TabItem::File {
            worktree: "andy/t7k".into(),
            path: "src/main.rs".into(),
        })
    );
    // C295: centered and highlighted, as Zed's go-to-line does.
    let (above, below) = drawn_around(cx, 150, 300);
    assert!(above > 1, "line 150 is not the top line");
    assert!(above.abs_diff(below) <= 1, "{above} above, {below} below");
    for n in 150..=152 {
        let selector: &'static str = Box::leak(format!("line:{n}:highlight").into_boxed_str());
        assert!(
            cx.debug_bounds(selector).is_some(),
            "line {n} is highlighted"
        );
    }
    assert!(cx.debug_bounds("line:149:highlight").is_none());
    assert!(cx.debug_bounds("line:153:highlight").is_none());
    let panes = workspace.read_with(cx, |w, cx| w.pane_group.read(cx).panes().len());
    assert_eq!(panes, 2, "a cmd-click opens a split");
}

#[gpui::test]
fn refs_route_against_the_chats_latest_snapshot(cx: &mut TestAppContext) {
    // C294: the backend replaces the chat's snapshot whole; the workspace
    // routes against the same one, so a new worktree's chips open and a
    // removed one's do not.
    let (workspace, cx) = open_with(chat_snapshot(), cx);
    let mut later = chat_snapshot();
    later.worktrees.retain(|wt| wt.handle != "andy/t7k");
    let mut added = later.worktrees[0].clone();
    added.owner = fixture_id("andy/nw1");
    added.handle = "andy/nw1".into();
    added.head = crate::backend::WorktreeHead::Branch("feat/new-thing".into());
    later.worktrees.push(added);
    workspace.update(cx, |w, cx| {
        w.chat.update(cx, |chat, cx| {
            chat.apply(crate::backend::Update::Snapshot(Box::new(later)), cx);
        });
    });
    redraw(cx);
    let from = fixture_id("andy/a1");
    let routes = workspace.read_with(cx, |w, _| {
        let new = RefTarget::Branch {
            name: "feat/new-thing".into(),
            at: None,
        };
        let gone = RefTarget::Commit(sha("7c8835e"));
        (
            route(&w.snapshot, &new, &from),
            route(&w.snapshot, &gone, &from),
        )
    });
    assert_eq!(routes, (git("andy/nw1", None), RefRoute::HoverOnly));
}
