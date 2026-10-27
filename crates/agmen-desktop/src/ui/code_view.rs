//! Read-only code with line numbers and change marks in the gutter: files
//! in the Project panel (Task 11), role files (Task 10), specs (Task 19)
//! and the lines of a diff (Task 12, through [`layout_lines`] and
//! [`styled_line`]). Highlights are computed once, when the view is made.

use std::ops::{Range, RangeInclusive};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, HighlightStyle, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Render, ScrollStrategy, SharedString, Styled as _, StyledText, UniformListScrollHandle,
    Window, div, uniform_list,
};

use super::syntax::{self, Language};
use super::tokens::{
    DELETED_MARK_HEIGHT, GUTTER_MARK_WIDTH, MONO_CELL_HEIGHT, MONO_CELL_WIDTH, SPACE_8,
};
use crate::appearance::{Appearance, Theme};

pub(crate) use crate::backend::GutterMark;

/// One line of text and its highlights, as byte ranges within the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Line {
    pub(super) text: SharedString,
    highlights: Vec<(Range<usize>, &'static str)>,
}

impl Line {
    /// `text` with no syntax colors, drawn in the text color.
    pub(super) fn plain(text: &str) -> Self {
        Self {
            text: SharedString::from(text.to_owned()),
            highlights: Vec::new(),
        }
    }
}

#[cfg(test)]
impl CodeView {
    /// The text shown, lines joined by newlines.
    pub(crate) fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.text.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The change marks shown.
    pub(crate) fn marks(&self) -> Vec<GutterMark> {
        self.marks.as_ref().clone()
    }
}

#[cfg(test)]
impl Line {
    /// The line's highlights, as byte ranges within it and syntax names.
    pub(super) fn highlights(&self) -> &[(Range<usize>, &'static str)] {
        &self.highlights
    }
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
/// (`keyword.function` falls back to `keyword`); the line number color for
/// agmen's [`syntax::LINE_NUMBER`].
fn syntax_color(theme: &Theme, name: &str) -> Option<Hsla> {
    if name == syntax::LINE_NUMBER {
        return Some(theme.line_number);
    }
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

/// Whether lines have a change mark column between number and text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gutter {
    /// Numbers end at cell 4, the mark is cell 5 and text starts at cell 7
    /// (the file view's mockup).
    Marks,
    /// Numbers end at cell 4 and text starts at cell 6 (the spec's mockup,
    /// node 9193).
    NumbersOnly,
}

/// A read-only file view.
pub(crate) struct CodeView {
    lines: Rc<Vec<Line>>,
    marks: Rc<Vec<GutterMark>>,
    gutter: Gutter,
    scroll: UniformListScrollHandle,
    /// Lines (1-based) a reference points at, drawn highlighted.
    highlighted: Option<RangeInclusive<u32>>,
}

impl CodeView {
    pub(crate) fn new(text: &str, language: Language, marks: Vec<GutterMark>) -> Self {
        Self {
            lines: Rc::new(layout_lines(text, language)),
            marks: Rc::new(marks),
            gutter: Gutter::Marks,
            scroll: UniformListScrollHandle::new(),
            highlighted: None,
        }
    }

    /// Scrolls so `lines`' first line (1-based) sits in the middle and
    /// highlights `lines`, as Zed's go-to-line does (go_to_line.rs at
    /// 08cbb88, :198 to 207); a line past the end scrolls to the last.
    pub(crate) fn show_lines(&mut self, lines: RangeInclusive<u32>, cx: &mut Context<Self>) {
        let line = *lines.start();
        let last = self.lines.len().saturating_sub(1);
        let ix = usize::try_from(line.saturating_sub(1)).map_or(last, |ix| ix.min(last));
        self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        self.highlighted = Some(lines);
        cx.notify();
    }

    /// Drops the change mark column, for text that never has marks (a
    /// spec), so text starts one cell nearer the numbers.
    #[must_use]
    pub(crate) fn without_marks(mut self) -> Self {
        self.gutter = Gutter::NumbersOnly;
        self
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

#[expect(
    clippy::too_many_arguments,
    reason = "one row's parts, each from the view's render"
)]
fn render_line(
    ix: usize,
    line: &Line,
    highlighted: Option<&RangeInclusive<u32>>,
    marks: &[GutterMark],
    gutter_kind: Gutter,
    number_width: Pixels,
    cell: Pixels,
    cx: &App,
) -> gpui::AnyElement {
    let appearance = cx.global::<Appearance>();
    let theme = &appearance.theme;
    let number = u32::try_from(ix + 1).unwrap_or(u32::MAX);
    // Behind the row's text: an absolute child takes no part in its flex.
    let highlight = highlighted
        .filter(|lines| lines.contains(&number))
        .map(|_| {
            div()
                .debug_selector(move || format!("line:{number}:highlight"))
                .absolute()
                .inset_0()
                .bg(theme.highlighted_line)
        });
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
    // The mark column is one cell wide; its bar is Zed's narrow git bar.
    // Without it, text keeps the same two cells after the numbers.
    let gutter = (gutter_kind == Gutter::Marks).then(|| {
        div()
            .debug_selector(move || format!("line:{number}:mark"))
            .w(cell)
            .h_full()
            .child(
                div()
                    .relative()
                    .w(GUTTER_MARK_WIDTH)
                    .h_full()
                    .children(fill)
                    .children(deleted),
            )
            .flex_none()
    });
    div()
        .debug_selector(move || format!("line:{number}"))
        .relative()
        .h(MONO_CELL_HEIGHT)
        .line_height(MONO_CELL_HEIGHT)
        .w_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(cell)
        .children(highlight)
        .child(
            div()
                .debug_selector(move || format!("line:{number}:number"))
                .w(number_width)
                .flex_none()
                .flex()
                .justify_end()
                .text_color(theme.line_number)
                .child(number.to_string()),
        )
        .children(gutter)
        .child(
            // shortcut: long lines are clipped; sideways scroll when the file
            // and diff views need to show them whole.
            div()
                .debug_selector(move || format!("line:{number}:text"))
                .when(gutter_kind == Gutter::NumbersOnly, |text| text.ml(cell))
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(styled_line(line, theme)),
        )
        .into_any_element()
}

/// The code font's cell, its digit width, so line numbers fit at any size;
/// the mockup's 7.62 px cell when the font cannot be measured. The code and
/// diff views lay out their columns in these cells, as console.py's grid.
pub(super) fn cell_width(window: &Window, fonts: &crate::appearance::Fonts) -> Pixels {
    let text = window.text_system();
    let font_id = text.resolve_font(&gpui::font(fonts.code_family.clone()));
    text.advance(font_id, fonts.code_size, '0')
        .map_or(MONO_CELL_WIDTH, |size| size.width)
}

impl Render for CodeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        let fonts = &appearance.fonts;
        // console.py's numbers end at column 4 with the mark at 5 and text
        // at 7; longer numbers widen the column and keep one cell of margin.
        let cells = (self.lines.len().max(1).to_string().len() + 1).max(4);
        let cell = cell_width(window, fonts);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a line count's digit count is tiny"
        )]
        let number_width = cell * cells as f32;
        let (lines, marks, gutter) = (self.lines.clone(), self.marks.clone(), self.gutter);
        let highlighted = self.highlighted.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            // console.py draws the grid 8 px down from row 1, so line 1
            // sits one row and 8 px below the breadcrumb.
            .pt(MONO_CELL_HEIGHT + SPACE_8)
            .bg(appearance.theme.editor_background)
            .text_color(appearance.theme.text)
            .font_family(fonts.code_family.clone())
            .text_size(fonts.code_size)
            .child(
                uniform_list("code-lines", lines.len(), move |range, _window, cx| {
                    range
                        .filter_map(|ix| {
                            lines.get(ix).map(|line| {
                                render_line(
                                    ix,
                                    line,
                                    highlighted.as_ref(),
                                    &marks,
                                    gutter,
                                    number_width,
                                    cell,
                                    cx,
                                )
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .flex_1(),
            )
    }
}

#[cfg(test)]
#[path = "code_view_tests.rs"]
mod tests;
