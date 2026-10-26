use gpui::TestAppContext;

use super::*;
use crate::backend::{DiffLine, chat_snapshot};
use crate::ui::test_support::{PlainButtonHost, fx1_worktree, init, redraw};

fn main_rs_hunks() -> Vec<Hunk> {
    chat_snapshot()
        .worktrees
        .into_iter()
        .next()
        .and_then(|wt| wt.changes.into_iter().find(|c| c.path == "src/main.rs"))
        .map(|c| c.hunks)
        .unwrap()
}

#[gpui::test]
fn diff_view_line_kinds(cx: &mut TestAppContext) {
    init(cx);
    let (_, cx) = cx.add_window_view(|_, _| DiffView::new(main_rs_hunks(), Language::Rust));
    redraw(cx);
    // Row 1 is the hunk header (mockup fix 41), then the lines in order.
    assert!(cx.debug_bounds("diff-hunk:0").is_some());
    for (row, kind) in [
        (2, "context"),
        (3, "removed"),
        (4, "context"),
        (6, "removed"),
        (8, "removed"),
        (9, "removed"),
        (10, "added"),
        (12, "removed"),
        (13, "added"),
        (18, "context"),
    ] {
        let sel: &'static str = Box::leak(format!("diff-row:{row}:{kind}").into_boxed_str());
        assert!(cx.debug_bounds(sel).is_some(), "{sel}");
    }
    assert!(cx.debug_bounds("diff-row:19:context").is_none(), "17 lines");
    // Changed lines carry a tint; context lines do not.
    assert!(cx.debug_bounds("diff-row:3:tint").is_some());
    assert!(cx.debug_bounds("diff-row:10:tint").is_some());
    assert!(cx.debug_bounds("diff-row:2:tint").is_none());
}

#[test]
fn diff_rows_follow_the_hunk() {
    let rows = layout_rows(&main_rs_hunks(), Language::Rust);
    let Some(Row::Header(header)) = rows.first() else {
        panic!("the first row is the hunk header");
    };
    assert_eq!(header.as_ref(), "@@ -1,15 +1,12 @@");
    let kinds: Vec<_> = rows
        .iter()
        .filter_map(|row| match row {
            Row::Line { kind, .. } => Some(*kind),
            Row::Header(_) => None,
        })
        .collect();
    assert_eq!(kinds.len(), 17);
    let count = |k| kinds.iter().filter(|kind| **kind == k).count();
    assert_eq!(count(DiffLineKind::Added), 2);
    assert_eq!(count(DiffLineKind::Removed), 5);
    let Some(Row::Line { line, .. }) = rows.get(2) else {
        panic!("row 2 is a line");
    };
    assert_eq!(line.text.as_ref(), "<<<<<<< HEAD");
    // An empty last line keeps its row.
    let last = Hunk {
        old_start: 1,
        new_start: 1,
        lines: vec![DiffLine {
            kind: DiffLineKind::Added,
            text: String::new(),
        }],
    };
    assert_eq!(layout_rows(&[last], Language::Plain).len(), 2);
}

#[gpui::test]
fn diff_view_without_hunks_says_so(cx: &mut TestAppContext) {
    init(cx);
    let (_, cx) = cx.add_window_view(|_, _| DiffView::new(Vec::new(), Language::Plain));
    redraw(cx);
    assert!(cx.debug_bounds("diff-empty").is_some());
}

#[gpui::test]
fn diff_columns_use_the_code_font_cell(cx: &mut TestAppContext) {
    // V84: console.py puts the sign at column 2 and code at column 4, in
    // code font cells, so the columns follow the person's code font size.
    init(cx);
    cx.update(|cx| {
        let mut appearance = cx.global::<crate::appearance::Appearance>().clone();
        appearance.fonts.code_size = gpui::px(26.);
        cx.set_global(appearance);
    });
    let (_, cx) = cx.add_window_view(|_, _| DiffView::new(main_rs_hunks(), Language::Rust));
    redraw(cx);
    let cell = cx.update(|window, cx| {
        cell_width(window, &cx.global::<crate::appearance::Appearance>().fonts)
    });
    let close = |a: gpui::Pixels, b: gpui::Pixels| (a - b).abs() < gpui::px(0.5);
    let sign = cx.debug_bounds("diff-row:3:sign").unwrap();
    assert!(close(sign.left(), cell * 2.), "{sign:?} {cell:?}");
    let text = cx.debug_bounds("diff-row:3:text").unwrap();
    assert!(close(text.left(), cell * 4.), "{text:?} {cell:?}");
}

#[test]
fn conflict_markers_do_not_bend_the_code_around_them() {
    // Frame 5057 node 5215: "mod cylinder;" after "<<<<<<< HEAD" keeps its
    // keyword color, and ">>>>>>> 4d9fdc8" is plain text.
    let rows = layout_rows(&main_rs_hunks(), Language::Rust);
    let line = |text: &str| {
        rows.iter()
            .find_map(|row| match row {
                Row::Line { line, .. } if line.text.as_ref() == text => Some(line.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no row {text}"))
    };
    let alone = |text: &str, language| layout_lines(&format!("{text}\n"), language).remove(0);
    assert_eq!(
        line("mod cylinder;"),
        alone("mod cylinder;", Language::Rust)
    );
    assert_eq!(line("mod minimap;"), alone("mod minimap;", Language::Rust));
    for marker in ["<<<<<<< HEAD", "=======", ">>>>>>> 4d9fdc8"] {
        assert_eq!(line(marker), alone(marker, Language::Plain), "{marker}");
    }
}

#[test]
fn diff_code_keeps_function_colors_past_conflict_markers() {
    // V103: the base side's conflict markers made tree-sitter read the rest
    // of the hunk as types, so `main`, `default` and `run` drew in the type
    // color. Each side is highlighted on its own, as Zed does.
    let rows = layout_rows(&main_rs_hunks(), Language::Rust);
    let name_of = |needle: &str| {
        rows.iter()
            .find_map(|row| match row {
                Row::Line { line, .. } => line.text.find(needle).map(|at| {
                    line.highlights()
                        .iter()
                        .find(|(range, _)| range.contains(&at))
                        .map(|(_, name)| *name)
                }),
                Row::Header(_) => None,
            })
            .flatten()
    };
    assert_eq!(name_of("main"), Some("function"));
    assert_eq!(name_of("default"), Some("function"));
    assert_eq!(name_of("run"), Some("function"));
    assert_eq!(name_of("Result"), Some("type"));
}

#[test]
fn conflict_marker_lines_are_not_highlighted() {
    // V111: parsing the base side alone read ">>>>>>> 4d9fdc8" as a number
    // and a name, so its "4" drew in the number color. A conflict marker
    // is not code; console.py draws its sha in text.
    let rows = layout_rows(&main_rs_hunks(), Language::Rust);
    let marked: Vec<_> = rows
        .iter()
        .filter_map(|row| match row {
            Row::Line { line, .. } if is_conflict_marker(&line.text) => Some(line),
            _ => None,
        })
        .collect();
    let texts: Vec<&str> = marked.iter().map(|line| line.text.as_ref()).collect();
    assert_eq!(texts, ["<<<<<<< HEAD", "=======", ">>>>>>> 4d9fdc8"]);
    for line in marked {
        assert_eq!(line.highlights(), [], "{:?}", line.text);
    }
    // Code lines next to them keep their colors.
    let Some(Row::Line { line, .. }) = rows.get(7) else {
        panic!("row 7 is a line");
    };
    assert_eq!(line.text.as_ref(), "mod tabs;");
    assert!(!line.highlights().is_empty());
}

#[test]
fn conflict_markers_are_seven_marks_alone_or_before_a_space() {
    for marker in [
        "<<<<<<< HEAD",
        "=======",
        ">>>>>>> 4d9fdc8",
        "||||||| base",
        ">>>>>>>",
    ] {
        assert!(is_conflict_marker(marker), "{marker}");
    }
    for code in [
        "== x",
        "<<<<<<<< too many",
        "a <<<<<<< b",
        "=======x",
        "<<= 2",
        // C246: a banner line is code, not the base and worktree divider.
        "======= Results =======",
    ] {
        assert!(!is_conflict_marker(code), "{code}");
    }
}

#[gpui::test]
fn diff_tab_holds_its_breadcrumb_open_file_and_diff(cx: &mut TestAppContext) {
    // V95: one view the workspace can host, as FileView is for a file.
    init(cx);
    let wt = fx1_worktree();
    let (tab, cx) = cx.add_window_view(|_, cx| DiffTab::new(&wt, "src/main.rs", cx));
    redraw(cx);
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.parts(), ["andy/fx1", "changes", "src/main.rs"]);
    });
    for selector in ["diff-tab", "breadcrumb", "diff-open-file", "diff-hunk:0"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    let crumb = cx.debug_bounds("breadcrumb").unwrap();
    let hunk = cx.debug_bounds("diff-hunk:0").unwrap();
    assert!(hunk.top() > crumb.bottom(), "{hunk:?} {crumb:?}");
}

#[gpui::test]
fn diff_tab_without_the_change_says_so(cx: &mut TestAppContext) {
    init(cx);
    let wt = fx1_worktree();
    let (_, cx) = cx.add_window_view(|_, cx| DiffTab::new(&wt, "README.md", cx));
    redraw(cx);
    assert!(cx.debug_bounds("diff-empty").is_some());
}

#[gpui::test]
fn diff_tab_open_file_button_is_plain(cx: &mut TestAppContext) {
    // V92: console.py 1729 draws Open file "plain": elevated surface,
    // padding 3/9, radius 5, 20 px tall.
    init(cx);
    let wt = fx1_worktree();
    let (_, cx) = cx.add_window_view(|_, cx| {
        let tab = cx.new(|cx| DiffTab::new(&wt, "src/main.rs", cx));
        PlainButtonHost::new(tab, "Open file")
    });
    redraw(cx);
    let want = cx.debug_bounds("want-plain").unwrap();
    let open = cx.debug_bounds("diff-open-file").unwrap();
    assert_eq!(open.size, want.size, "{open:?} {want:?}");
}
