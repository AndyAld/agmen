use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{WorktreeInfo, chat_snapshot, fixture_id};
use crate::ui::scope_chip::ScopeOption;
use crate::ui::test_support::{click, init, redraw};

fn fx1() -> WorktreeInfo {
    chat_snapshot()
        .worktrees
        .into_iter()
        .find(|wt| wt.handle == "andy/fx1")
        .unwrap()
}

/// A second worktree, as Task 12's fixture will hold.
fn rv2() -> WorktreeInfo {
    WorktreeInfo {
        owner: fixture_id("andy/rv2"),
        handle: "andy/rv2".into(),
        head: crate::backend::WorktreeHead::Detached {
            at: "9e2a71c".parse().unwrap(),
            item: "C015".into(),
        },
        files: vec!["README.md".into()],
        changes: Vec::new(),
        commits: Vec::new(),
        state: crate::backend::WorktreeState::Reading,
    }
}

/// The panel on `worktrees`, in the mockup's state: main.rs selected and
/// `src/ui` open.
fn panel(
    cx: &mut TestAppContext,
    worktrees: Vec<WorktreeInfo>,
) -> (Entity<ProjectPanel>, &mut VisualTestContext) {
    init(cx);
    let agents = chat_snapshot().agents;
    let options = ScopeOption::for_worktrees(&worktrees, &agents);
    let (view, cx) = cx.add_window_view(|_, cx| {
        let mut panel = ProjectPanel::new(worktrees, options, cx);
        panel.reveal("src/main.rs");
        panel.open_folder("src/ui");
        panel
    });
    redraw(cx);
    (view, cx)
}

fn rows(view: &Entity<ProjectPanel>, cx: &mut VisualTestContext) -> Vec<(u8, String)> {
    view.read_with(cx, |panel, _| {
        panel
            .visible_rows()
            .map(|e| (e.depth, e.name.clone()))
            .collect()
    })
}

fn owned(list: &[(u8, &str)]) -> Vec<(u8, String)> {
    list.iter().map(|(d, n)| (*d, (*n).to_owned())).collect()
}

/// console.py's `PROJECT`, tests closed, with two rulings: folders come
/// before files and names sort case-insensitively at each level, as Zed's
/// panel does (the mockup lists main.rs before ui/ and Cargo.toml before
/// Cargo.lock), and src/app.rs is there (mockup fix 50).
const PROJECT: [(u8, &str); 15] = [
    (0, "fix-c015-main-conflict"),
    (1, ".github"),
    (1, "src"),
    (2, "ui"),
    (3, "mod.rs"),
    (3, "tabs.rs"),
    (2, "app.rs"),
    (2, "cylinder.rs"),
    (2, "document.rs"),
    (2, "main.rs"),
    (2, "minimap.rs"),
    (1, "tests"),
    (1, "Cargo.lock"),
    (1, "Cargo.toml"),
    (1, "README.md"),
];

#[gpui::test]
fn tree_matches_fixture_order(cx: &mut TestAppContext) {
    let (view, cx) = panel(cx, vec![fx1()]);
    assert_eq!(rows(&view, cx), owned(&PROJECT));
    assert!(cx.debug_bounds("tree:src/main.rs").is_some());
    assert!(cx.debug_bounds("tree:tests/it_main.rs").is_none());
    view.read_with(cx, |panel, _| {
        assert_eq!(panel.selected(), Some("src/main.rs"));
    });
}

#[gpui::test]
fn opening_tests_shows_two_files(cx: &mut TestAppContext) {
    let (view, cx) = panel(cx, vec![fx1()]);
    click(cx, "tree:tests");
    let after = rows(&view, cx);
    let tests = after.iter().position(|(_, n)| n == "tests").unwrap();
    assert_eq!(
        after[tests + 1..tests + 3],
        owned(&[(2, "it_main.rs"), (2, "it_tabs.rs")])
    );
    assert_eq!(after.len(), PROJECT.len() + 2);
    assert!(cx.debug_bounds("tree:tests/it_tabs.rs").is_some());
    click(cx, "tree:tests");
    assert_eq!(rows(&view, cx), owned(&PROJECT), "a second click folds it");
}

#[gpui::test]
fn change_marker_on_modified_files(cx: &mut TestAppContext) {
    let (_, cx) = panel(cx, vec![fx1()]);
    assert!(cx.debug_bounds("tree-marker:src/main.rs").is_some());
    assert!(cx.debug_bounds("tree-marker:Cargo.lock").is_some());
    assert!(cx.debug_bounds("tree-marker:Cargo.toml").is_none());
    assert!(cx.debug_bounds("tree-marker:src").is_none());
    let changes = fx1().changes;
    assert_eq!(
        change_marker(&changes, "src/main.rs"),
        Some(("M", Color::Warning))
    );
    assert_eq!(change_marker(&changes, "README.md"), None);
}

#[gpui::test]
fn clicking_a_file_opens_it(cx: &mut TestAppContext) {
    let (view, cx) = panel(cx, vec![fx1()]);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &ProjectPanelEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    click(cx, "tree:README.md");
    assert_eq!(
        *seen.borrow(),
        vec![ProjectPanelEvent::OpenFile {
            worktree: fixture_id("andy/fx1"),
            path: "README.md".into(),
        }]
    );
    view.read_with(cx, |panel, _| {
        assert_eq!(panel.selected(), Some("README.md"));
    });
    click(cx, "tree:src");
    assert_eq!(seen.borrow().len(), 1, "a folder folds and opens nothing");
}

#[gpui::test]
fn scope_chip_switches_worktree(cx: &mut TestAppContext) {
    let (view, cx) = panel(cx, vec![fx1(), rv2()]);
    assert!(cx.debug_bounds("scope-chip:1").is_none());
    click(cx, "scope-chip");
    assert!(cx.debug_bounds("scope-chip:1").is_some());
    click(cx, "scope-chip:1");
    assert!(
        cx.debug_bounds("scope-chip:1").is_none(),
        "picking closes it"
    );
    assert_eq!(rows(&view, cx), owned(&[(0, "C015"), (1, "README.md")]));
    view.read_with(cx, |panel, _| {
        assert_eq!(panel.worktree(), Some(fixture_id("andy/rv2")));
        assert_eq!(panel.selected(), None);
    });
}

#[test]
fn tree_puts_folders_first_and_bounds_depth() {
    let deep = format!("{}file.rs", "d/".repeat(usize::from(MAX_DEPTH) + 5));
    let files = vec!["b.rs".to_owned(), "a/x.rs".to_owned(), deep, "/".to_owned()];
    let tree = build_tree("root", &files);
    let names: Vec<_> = tree.iter().take(4).map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["root", "a", "x.rs", "d"]);
    assert!(tree.iter().all(|e| e.depth <= MAX_DEPTH));
    assert_eq!(tree.last().map(|e| e.name.as_str()), Some("b.rs"));
}

#[gpui::test]
fn file_view_marks_deletions(cx: &mut TestAppContext) {
    init(cx);
    let wt = fx1();
    let (view, cx) = cx.add_window_view(|_, cx| FileView::new(&wt, "src/main.rs", cx));
    redraw(cx);
    for selector in ["gutter:1:deleted", "gutter:3:deleted", "gutter:4:deleted"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    assert!(cx.debug_bounds("gutter:2:deleted").is_none());
    assert!(cx.debug_bounds("gutter:5:added").is_some());
    assert!(cx.debug_bounds("line:12").is_some());
    view.read_with(cx, |file, _| {
        assert_eq!(file.parts(), ["andy/fx1", "src", "main.rs"]);
    });
}
