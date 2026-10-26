//! Read-only code with line numbers and change marks in the gutter: files
//! in the Project panel (Task 11), role files (Task 10) and the lines of a
//! diff (Task 12, through [`layout_lines`] and [`styled_line`]). Highlights
//! are computed once, when the view is made.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Context, HighlightStyle, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, StyledText, Window, div, uniform_list,
};

use super::syntax::{self, Language};
use super::tokens::{
    DELETED_MARK_HEIGHT, GUTTER_MARK_WIDTH, MONO_CELL_HEIGHT, MONO_CELL_WIDTH, SPACE_10, SPACE_12,
    SPACE_16,
};
use crate::appearance::{Appearance, Theme};

/// A change mark beside a line. Line numbers start at 1; ranges are
/// end-exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GutterMark {
    Added(Range<u32>),
    Modified(Range<u32>),
    /// Lines were deleted just after this line.
    Deleted(u32),
}

/// One line of text and its highlights, as byte ranges within the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Line {
    pub(super) text: SharedString,
    highlights: Vec<(Range<usize>, &'static str)>,
}

/// Splits `text` into lines and clips each highlight to the lines it covers.
/// A final newline does not make an extra empty line, and a CRLF ending
/// counts as one line break.
pub(super) fn layout_lines(text: &str, language: Language) -> Vec<Line> {
    let highlights = syntax::highlight(text, language);
    let mut lines = Vec::new();
    let mut next = 0;
    let mut start = 0;
    for piece in text.split_inclusive('\n') {
        let end = start + piece.trim_end_matches('\n').trim_end_matches('\r').len();
        let mut own = Vec::new();
        // Highlights are sorted; skip those that end before this line.
        while let Some(h) = highlights.get(next) {
            if h.range.end <= start {
                next += 1;
                continue;
            }
            if h.range.start >= end {
                break;
            }
            let from = h.range.start.max(start) - start;
            let to = h.range.end.min(end) - start;
            if from < to {
                own.push((from..to, h.name));
            }
            if h.range.end <= end {
                next += 1;
            } else {
                break;
            }
        }
        lines.push(Line {
            text: SharedString::from(text[start..end].to_owned()),
            highlights: own,
        });
        start += piece.len();
    }
    lines
}

/// The theme's color for a Zed syntax name, or the nearest parent name's
/// (`keyword.function` falls back to `keyword`).
fn syntax_color(theme: &Theme, name: &str) -> Option<Hsla> {
    let mut name = name;
    loop {
        if let Some(color) = theme.syntax.get(name) {
            return Some(*color);
        }
        name = name.rsplit_once('.')?.0;
    }
}

/// `line`'s text in the theme's syntax colors.
pub(super) fn styled_line(line: &Line, theme: &Theme) -> StyledText {
    let highlights: Vec<(Range<usize>, HighlightStyle)> = line
        .highlights
        .iter()
        .filter_map(|(range, name)| {
            syntax_color(theme, name).map(|color| {
                (
                    range.clone(),
                    HighlightStyle {
                        color: Some(color),
                        ..HighlightStyle::default()
                    },
                )
            })
        })
        .collect();
    StyledText::new(line.text.clone()).with_highlights(highlights)
}

/// A read-only file view.
pub(crate) struct CodeView {
    lines: Rc<Vec<Line>>,
    marks: Rc<Vec<GutterMark>>,
}

impl CodeView {
    pub(crate) fn new(text: &str, language: Language, marks: Vec<GutterMark>) -> Self {
        Self {
            lines: Rc::new(layout_lines(text, language)),
            marks: Rc::new(marks),
        }
    }
}

/// What fills a line's gutter bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fill {
    Added,
    Modified,
}

/// The Added or Modified mark that fills line `line`'s gutter.
fn fill_mark(marks: &[GutterMark], line: u32) -> Option<Fill> {
    marks.iter().find_map(|m| match m {
        GutterMark::Added(r) if r.contains(&line) => Some(Fill::Added),
        GutterMark::Modified(r) if r.contains(&line) => Some(Fill::Modified),
        GutterMark::Added(_) | GutterMark::Modified(_) | GutterMark::Deleted(_) => None,
    })
}

/// The deletion marks on line `line`'s row, as (mark line, at the top):
/// lines removed after it sit at its bottom edge, and lines removed above
/// the first line (`Deleted(0)`) at line 1's top edge. Line 1 can have both.
fn deleted_marks(marks: &[GutterMark], line: u32) -> impl Iterator<Item = (u32, bool)> + '_ {
    marks.iter().filter_map(move |m| match m {
        GutterMark::Deleted(at) if *at == line => Some((*at, false)),
        GutterMark::Deleted(0) if line == 1 => Some((0, true)),
        GutterMark::Added(_) | GutterMark::Modified(_) | GutterMark::Deleted(_) => None,
    })
}

fn render_line(
    ix: usize,
    line: &Line,
    marks: &[GutterMark],
    digits: usize,
    cx: &App,
) -> gpui::AnyElement {
    let appearance = cx.global::<Appearance>();
    let theme = &appearance.theme;
    let number = u32::try_from(ix + 1).unwrap_or(u32::MAX);
    let fill = fill_mark(marks, number).map(|fill| {
        let (kind, color) = match fill {
            Fill::Added => ("added", theme.created),
            Fill::Modified => ("modified", theme.modified),
        };
        div()
            .debug_selector(move || format!("gutter:{number}:{kind}"))
            .absolute()
            .inset_0()
            .bg(color)
    });
    let deleted = deleted_marks(marks, number).map(|(at, top)| {
        let edge = div()
            .debug_selector(move || format!("gutter:{at}:deleted"))
            .absolute()
            .left_0()
            .right_0()
            .h(DELETED_MARK_HEIGHT)
            .bg(theme.deleted);
        if top { edge.top_0() } else { edge.bottom_0() }
    });
    let gutter = div()
        .relative()
        .w(GUTTER_MARK_WIDTH)
        .h_full()
        .children(fill)
        .children(deleted);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a line count's digit count is tiny"
    )]
    let number_width = MONO_CELL_WIDTH * digits as f32;
    div()
        .debug_selector(move || format!("line:{number}"))
        .h(MONO_CELL_HEIGHT)
        .line_height(MONO_CELL_HEIGHT)
        .w_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(SPACE_10)
        .child(
            div()
                .w(number_width)
                .flex_none()
                .flex()
                .justify_end()
                .text_color(theme.line_number)
                .child(number.to_string()),
        )
        .child(gutter.flex_none())
        .child(
            // shortcut: long lines are clipped; sideways scroll when the file
            // and diff views need to show them whole.
            div()
                .debug_selector(move || format!("line:{number}:text"))
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(styled_line(line, theme)),
        )
        .into_any_element()
}

impl Render for CodeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        let fonts = &appearance.fonts;
        let digits = self.lines.len().max(1).to_string().len().max(2);
        let (lines, marks) = (self.lines.clone(), self.marks.clone());
        div()
            .size_full()
            .flex()
            .flex_col()
            .pt(SPACE_16)
            .pl(SPACE_12)
            .bg(appearance.theme.editor_background)
            .text_color(appearance.theme.text)
            .font_family(fonts.code_family.clone())
            .text_size(fonts.code_size)
            .child(
                uniform_list("code-lines", lines.len(), move |range, _window, cx| {
                    range
                        .filter_map(|ix| {
                            lines
                                .get(ix)
                                .map(|line| render_line(ix, line, &marks, digits, cx))
                        })
                        .collect::<Vec<_>>()
                })
                .flex_1(),
            )
    }
}

#[cfg(test)]
#[path = "code_view_tests.rs"]
mod tests;
