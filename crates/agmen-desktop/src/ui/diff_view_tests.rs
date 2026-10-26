use gpui::TestAppContext;

use super::*;
use crate::backend::{DiffLine, chat_snapshot};
use crate::ui::test_support::{init, redraw};

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
