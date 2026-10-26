use gpui::TestAppContext;

use super::*;
use crate::ui::syntax::Language;
use crate::ui::test_support::{init, redraw};

const MAIN_RS: &str = "mod app;\nmod cylinder;\nmod document;\nmod minimap;\nmod ui;\n\nuse ui::tabs::Tabs;\n\nfn main() -> anyhow::Result<()> {\n    let tabs = Tabs::default();\n    app::run(tabs)\n}\n";

#[gpui::test]
fn code_view_marks_gutter(cx: &mut TestAppContext) {
    init(cx);
    let marks = vec![
        GutterMark::Added(5..6),
        GutterMark::Added(7..8),
        GutterMark::Deleted(4),
    ];
    let (_, cx) = cx.add_window_view(|_, cx| CodeView::new(MAIN_RS, Language::Rust, marks, cx));
    redraw(cx);
    assert!(cx.debug_bounds("line:1").is_some());
    assert!(cx.debug_bounds("line:12").is_some());
    assert!(
        cx.debug_bounds("line:13").is_none(),
        "12 lines, no phantom last line"
    );
    assert!(cx.debug_bounds("gutter:5:added").is_some());
    assert!(cx.debug_bounds("gutter:7:added").is_some());
    assert!(cx.debug_bounds("gutter:4:deleted").is_some());
    assert!(
        cx.debug_bounds("gutter:6:added").is_none(),
        "ranges are end-exclusive"
    );
}

#[test]
fn lines_split_highlights_at_line_breaks() {
    let lines = layout_lines("fn a() {\n    1\n}\n", Language::Rust);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].text, "fn a() {");
    assert!(
        lines[0]
            .highlights
            .iter()
            .any(|(r, name)| &lines[0].text[r.clone()] == "fn" && name.starts_with("keyword"))
    );
    for line in &lines {
        for (r, _) in &line.highlights {
            assert!(r.end <= line.text.len());
        }
    }
}

#[test]
fn syntax_color_falls_back_to_the_parent_name() {
    let theme = crate::appearance::Appearance::builtin().theme;
    assert_eq!(
        syntax_color(&theme, "keyword.function"),
        theme.syntax.get("keyword").copied()
    );
    assert_eq!(
        syntax_color(&theme, "string"),
        theme.syntax.get("string").copied()
    );
    assert_eq!(syntax_color(&theme, "nothing.like.this"), None);
}
