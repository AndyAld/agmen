use gpui::{AppContext as _, TestAppContext};

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
    let (_, cx) = cx.add_window_view(|_, _| CodeView::new(MAIN_RS, Language::Rust, marks));
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

/// Hosts a code view in a 200 px wide box, narrower than a long line.
struct Narrow(gpui::Entity<CodeView>);

impl gpui::Render for Narrow {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .w(gpui::px(200.))
            .h(gpui::px(300.))
            .child(self.0.clone())
    }
}

#[gpui::test]
fn long_lines_stay_on_one_row(cx: &mut TestAppContext) {
    // Review C149: a line wider than the pane must not wrap over the next.
    init(cx);
    let text = format!("{}\nshort\n", "x".repeat(400));
    let (_, cx) = cx.add_window_view(|_, cx| {
        let code = cx.new(|_| CodeView::new(&text, Language::Plain, Vec::new()));
        Narrow(code)
    });
    redraw(cx);
    let first = cx.debug_bounds("line:1:text").unwrap();
    let second = cx.debug_bounds("line:2").unwrap();
    assert!(first.size.height <= MONO_CELL_HEIGHT, "{first:?}");
    assert!(
        first.size.width <= gpui::px(200.),
        "clipped to the pane: {first:?}"
    );
    assert!(first.bottom() <= second.top(), "{first:?} {second:?}");
}

#[gpui::test]
fn deleted_marks_sit_inside_their_row(cx: &mut TestAppContext) {
    // Review C153: Deleted(0) marks lines removed above line 1, and every
    // deleted mark stays inside its line's row and gutter.
    init(cx);
    let marks = vec![
        GutterMark::Deleted(0),
        GutterMark::Deleted(4),
        GutterMark::Modified(2..3),
    ];
    let (_, cx) = cx.add_window_view(|_, _| CodeView::new(MAIN_RS, Language::Rust, marks));
    redraw(cx);
    let above = cx.debug_bounds("gutter:0:deleted").unwrap();
    let line1 = cx.debug_bounds("line:1").unwrap();
    assert!(above.top() >= line1.top() && above.bottom() <= line1.bottom());
    let after4 = cx.debug_bounds("gutter:4:deleted").unwrap();
    let line4 = cx.debug_bounds("line:4").unwrap();
    assert!(after4.top() >= line4.top() && after4.bottom() <= line4.bottom());
    assert!(after4.size.width <= gpui::px(4.), "{after4:?}");
    // Review C157: a Modified mark shows.
    assert!(cx.debug_bounds("gutter:2:modified").is_some());
    assert!(cx.debug_bounds("gutter:3:modified").is_none());
}

#[test]
fn crlf_lines_drop_the_carriage_return() {
    // Review C158.
    let lines = layout_lines("a\r\nb\r\n", Language::Plain);
    let texts: Vec<&str> = lines.iter().map(|l| l.text.as_ref()).collect();
    assert_eq!(texts, ["a", "b"]);
}

#[gpui::test]
fn line_one_shows_deletions_above_and_below(cx: &mut TestAppContext) {
    // Review C161: Deleted(0) and Deleted(1) both mark line 1.
    init(cx);
    let marks = vec![GutterMark::Deleted(0), GutterMark::Deleted(1)];
    let (_, cx) = cx.add_window_view(|_, _| CodeView::new(MAIN_RS, Language::Rust, marks));
    redraw(cx);
    let above = cx.debug_bounds("gutter:0:deleted").unwrap();
    let below = cx.debug_bounds("gutter:1:deleted").unwrap();
    assert!(above.top() < below.top(), "{above:?} {below:?}");
}

fn number_column_width(cx: &mut TestAppContext, code_size: f32) -> gpui::Pixels {
    init(cx);
    cx.update(|cx| {
        let mut appearance = cx.global::<crate::appearance::Appearance>().clone();
        appearance.fonts.code_size = gpui::px(code_size);
        cx.set_global(appearance);
    });
    let (_, cx) = cx.add_window_view(|_, _| CodeView::new(MAIN_RS, Language::Rust, Vec::new()));
    redraw(cx);
    cx.debug_bounds("line:10:number").unwrap().size.width
}

#[gpui::test]
fn line_numbers_fit_the_code_font(cx: &mut TestAppContext) {
    // Task 11 found "10" drawn as "l0" at the default 15 px code font: the
    // number column used a fixed 7.62 px cell instead of the font's digits.
    let small = number_column_width(cx, 10.);
    let large = number_column_width(cx, 30.);
    assert!(large > small * 2., "{small:?} {large:?}");
}
