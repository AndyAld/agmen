//! A file's diff in a center tab (`desktop-git.webp`): each hunk's header,
//! then its lines, removed on a red tint and added on a green one from the
//! theme's version control colors. Code keeps its syntax colors, through
//! the code view's line layout.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Render, SharedString, Styled as _, Window, div, uniform_list,
};

use super::breadcrumb::Breadcrumb;
use super::code_view::{Line, cell_width, layout_lines, styled_line};
use super::components::{Button, ButtonStyle, Color, Label};
use super::syntax::Language;
use super::tokens::{DIFF_TINT, MONO_CELL_HEIGHT, SPACE_8, SPACE_16};
use crate::appearance::Appearance;
use crate::backend::{DiffLineKind, Hunk, WorktreeInfo};

/// One row of the view.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    /// A hunk's `@@ -1,15 +1,12 @@` line.
    Header(SharedString),
    Line {
        kind: DiffLineKind,
        line: Line,
    },
}

/// Whether `text` is a merge conflict marker line: seven `<`, `=`, `|` or
/// `>`, alone or before a space.
fn is_conflict_marker(text: &str) -> bool {
    let bytes = text.as_bytes();
    let Some(&mark) = bytes.first() else {
        return false;
    };
    let run = bytes.iter().take_while(|&&b| b == mark).count();
    matches!(mark, b'<' | b'=' | b'|' | b'>')
        && run == 7
        && matches!(bytes.get(7), None | Some(b' '))
}

/// Every hunk's header and lines, highlighted as `language`. Each side of a
/// hunk is highlighted as one text, as Zed highlights the old and the new
/// buffer: context and added lines from the worktree side, removed lines
/// from the base side. Mixing the two would make one side's conflict
/// markers turn the rest of the hunk into types (V103). A conflict marker
/// line is not code, so it draws in the text color (V111).
fn layout_rows(hunks: &[Hunk], language: Language) -> Vec<Row> {
    let mut rows = Vec::new();
    for hunk in hunks {
        rows.push(Row::Header(hunk.header().into()));
        // One newline after every line, so an empty last line keeps its row.
        let side = |skip: DiffLineKind| -> String {
            hunk.lines
                .iter()
                .filter(|line| line.kind != skip)
                .flat_map(|line| [line.text.as_str(), "\n"])
                .collect()
        };
        let mut new = layout_lines(&side(DiffLineKind::Removed), language).into_iter();
        let mut old = layout_lines(&side(DiffLineKind::Added), language).into_iter();
        let before = rows.len();
        rows.extend(hunk.lines.iter().filter_map(|diff| {
            let line = match diff.kind {
                DiffLineKind::Context => {
                    old.next();
                    new.next()
                }
                DiffLineKind::Added => new.next(),
                DiffLineKind::Removed => old.next(),
            }?;
            let line = if is_conflict_marker(&line.text) {
                Line::plain(line.text)
            } else {
                line
            };
            Some(Row::Line {
                kind: diff.kind,
                line,
            })
        }));
        debug_assert_eq!(
            rows.len() - before,
            hunk.lines.len(),
            "one laid out line per diff line"
        );
    }
    rows
}

/// A read-only diff. Tests find each hunk header as `"diff-hunk:{n}"` from
/// 0, each row as `"diff-row:{row}:{kind}"` (rows from 1, kind `context`,
/// `added` or `removed`), a changed row's tint as `"diff-row:{row}:tint"`,
/// and an empty diff as `diff-empty`.
pub(crate) struct DiffView {
    rows: Rc<Vec<Row>>,
    /// Which hunk each row's header is, so selectors count hunks.
    headers: Rc<Vec<usize>>,
}

impl DiffView {
    pub(crate) fn new(hunks: Vec<Hunk>, language: Language) -> Self {
        let rows = layout_rows(&hunks, language);
        let headers = rows
            .iter()
            .scan(0, |seen, row| {
                let ix = *seen;
                if matches!(row, Row::Header(_)) {
                    *seen += 1;
                }
                Some(ix)
            })
            .collect();
        Self {
            rows: Rc::new(rows),
            headers: Rc::new(headers),
        }
    }
}

const fn kind_slug(kind: DiffLineKind) -> &'static str {
    match kind {
        DiffLineKind::Context => "context",
        DiffLineKind::Added => "added",
        DiffLineKind::Removed => "removed",
    }
}

/// `cell` is the code font's cell; console.py puts the sign at column 2
/// and code at column 4.
fn render_row(ix: usize, header_ix: usize, row: &Row, cell: Pixels, cx: &App) -> gpui::AnyElement {
    let theme = &cx.global::<Appearance>().theme;
    let number = ix + 1;
    let base = div()
        .h(MONO_CELL_HEIGHT)
        .line_height(MONO_CELL_HEIGHT)
        .w_full()
        .relative()
        .flex()
        .items_center()
        .overflow_hidden()
        .whitespace_nowrap();
    match row {
        Row::Header(text) => base
            .debug_selector(move || format!("diff-hunk:{header_ix}"))
            .pl(cell * 2.)
            .text_color(theme.text_accent)
            .child(text.clone())
            .into_any_element(),
        Row::Line { kind, line } => {
            let (sign, sign_color, tint) = match kind {
                DiffLineKind::Context => (" ", theme.line_number, None),
                DiffLineKind::Added => ("+", theme.created, Some(theme.created)),
                DiffLineKind::Removed => ("-", theme.deleted, Some(theme.deleted)),
            };
            let slug = kind_slug(*kind);
            base.debug_selector(move || format!("diff-row:{number}:{slug}"))
                .when_some(tint, |this, color| {
                    this.child(
                        div()
                            .debug_selector(move || format!("diff-row:{number}:tint"))
                            .absolute()
                            .inset_0()
                            .bg(color.opacity(DIFF_TINT)),
                    )
                })
                .child(
                    div()
                        .debug_selector(move || format!("diff-row:{number}:sign"))
                        .flex_none()
                        .w(cell * 2.)
                        .ml(cell * 2.)
                        .font_weight(FontWeight::BOLD)
                        .text_color(sign_color)
                        .child(sign),
                )
                .child(
                    // shortcut: long lines are clipped, as in the code view;
                    // sideways scroll comes with it.
                    div()
                        .debug_selector(move || format!("diff-row:{number}:text"))
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(styled_line(line, theme)),
                )
                .into_any_element()
        }
    }
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        let fonts = &appearance.fonts;
        let cell = cell_width(window, fonts);
        let body = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(appearance.theme.editor_background)
            .text_color(appearance.theme.text)
            .font_family(fonts.code_family.clone())
            .text_size(fonts.code_size);
        if self.rows.is_empty() {
            return body.p(SPACE_16).child(
                div()
                    .debug_selector(|| "diff-empty".into())
                    .child(Label::new("No changes to show").color(Color::Placeholder)),
            );
        }
        let (rows, headers) = (self.rows.clone(), self.headers.clone());
        // The mockup leaves one blank row above the first header.
        body.pt(MONO_CELL_HEIGHT + SPACE_8).child(
            uniform_list("diff-rows", rows.len(), move |range, _window, cx| {
                range
                    .filter_map(|ix| {
                        let header = headers.get(ix).copied().unwrap_or(0);
                        rows.get(ix)
                            .map(|row| render_row(ix, header, row, cell, cx))
                    })
                    .collect::<Vec<_>>()
            })
            .flex_1(),
        )
    }
}

/// A change's diff in a center tab: the worktree, `changes` and the path in
/// the breadcrumb with Open file at its end, then the diff
/// (`desktop-git.webp`). Tests find it as `diff-tab` and the button as
/// `diff-open-file`.
pub(crate) struct DiffTab {
    parts: Vec<SharedString>,
    diff: Entity<DiffView>,
}

impl DiffTab {
    /// `path`'s change in `worktree`; a path without a change shows an
    /// empty diff.
    pub(crate) fn new(worktree: &WorktreeInfo, path: &str, cx: &mut Context<Self>) -> Self {
        let hunks = worktree
            .changes
            .iter()
            .find(|change| change.path == path)
            .map(|change| change.hunks.clone())
            .unwrap_or_default();
        let language = Language::for_path(std::path::Path::new(path));
        let diff = cx.new(|_| DiffView::new(hunks, language));
        let parts = vec![
            SharedString::from(worktree.handle.clone()),
            "changes".into(),
            SharedString::from(path.to_owned()),
        ];
        Self { parts, diff }
    }

    #[cfg(test)]
    pub(crate) fn parts(&self) -> Vec<&str> {
        self.parts.iter().map(SharedString::as_ref).collect()
    }
}

impl Render for DiffTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        // shortcut: Open file does nothing until the workspace hosts the
        // Project panel's file tabs (Task 11 and agmen-c7's wiring).
        let crumb = Breadcrumb::new(self.parts.clone())
            .end(Button::new("diff-open-file", "Open file").style(ButtonStyle::Plain));
        div()
            .debug_selector(|| "diff-tab".into())
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.editor_background)
            .child(crumb)
            .child(div().flex_1().min_h_0().child(self.diff.clone()))
    }
}

#[cfg(test)]
#[path = "diff_view_tests.rs"]
mod tests;
