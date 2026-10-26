//! Read-only code with line numbers and change marks in the gutter: files
//! in the Project panel (Task 11), role files (Task 10) and the lines of a
//! diff (Task 12). Highlights are computed once, when the view is made.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Context, HighlightStyle, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, StyledText, Window, div, uniform_list,
};

use super::syntax::{self, Language};
use super::tokens::{MONO_CELL_HEIGHT, MONO_CELL_WIDTH, SPACE_10, SPACE_12, SPACE_16};
use crate::appearance::{Appearance, Theme};

/// A change mark beside a line. Line numbers start at 1; ranges are
/// end-exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the Project panel's file view marks added and deleted lines in Task 11"
    )
)]
pub(crate) enum GutterMark {
    Added(Range<u32>),
    Modified(Range<u32>),
    /// Lines were deleted just after this line.
    Deleted(u32),
}

/// One line of text and its highlights, as byte ranges within the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) text: SharedString,
    pub(crate) highlights: Vec<(Range<usize>, &'static str)>,
}

/// Splits `text` into lines and clips each highlight to the lines it covers.
/// A final newline does not make an extra empty line.
pub(crate) fn layout_lines(text: &str, language: Language) -> Vec<Line> {
    let highlights = syntax::highlight(text, language);
    let mut lines = Vec::new();
    let mut next = 0;
    let mut start = 0;
    for piece in text.split_inclusive('\n') {
        let end = start + piece.trim_end_matches('\n').len();
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
pub(crate) fn syntax_color(theme: &Theme, name: &str) -> Option<Hsla> {
    let mut name = name;
    loop {
        if let Some(color) = theme.syntax.get(name) {
            return Some(*color);
        }
        name = name.rsplit_once('.')?.0;
    }
}

/// A read-only file view.
pub(crate) struct CodeView {
    lines: Rc<Vec<Line>>,
    marks: Rc<Vec<GutterMark>>,
}

impl CodeView {
    pub(crate) fn new(
        text: &str,
        language: Language,
        marks: Vec<GutterMark>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            lines: Rc::new(layout_lines(text, language)),
            marks: Rc::new(marks),
        }
    }
}

fn mark_for(marks: &[GutterMark], line: u32) -> Option<(&'static str, bool)> {
    marks.iter().find_map(|m| match m {
        GutterMark::Added(r) if r.contains(&line) => Some(("added", true)),
        GutterMark::Modified(r) if r.contains(&line) => Some(("modified", true)),
        GutterMark::Deleted(at) if *at == line => Some(("deleted", false)),
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
    let mark = mark_for(marks, number);
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
    let gutter = match mark {
        Some((kind, true)) => {
            let color = if kind == "added" {
                theme.created
            } else {
                theme.modified
            };
            div()
                .debug_selector(move || format!("gutter:{number}:{kind}"))
                .w(gpui::px(4.))
                .h_full()
                .bg(color)
        }
        Some((kind, false)) => div()
            .debug_selector(move || format!("gutter:{number}:{kind}"))
            .w(gpui::px(4.))
            .text_color(theme.deleted)
            .child("▾"),
        None => div().w(gpui::px(4.)),
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "a line count's digit count is tiny"
    )]
    let number_width = MONO_CELL_WIDTH * digits as f32;
    div()
        .debug_selector(move || format!("line:{number}"))
        .h(MONO_CELL_HEIGHT)
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
        .child(StyledText::new(line.text.clone()).with_highlights(highlights))
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
