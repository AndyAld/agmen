//! A message body with its references drawn as chips (`rich_body` in
//! `docs/mockups/scripts/console.py`).
//!
//! The body is laid out word by word in a wrapping row, as the mockup does,
//! so a chip flows with the words around it. Short punctuation glued to a
//! reference stays beside its chip with no gap (mockup fix 54). A word or
//! chip wider than the column, such as a 500 character token, shrinks to
//! the column and wraps inside itself. A line of more than
//! `MAX_FLOW_WORDS` words draws its words as a few wrapping runs instead,
//! so a 32 KiB body stays cheap to lay out. Every slice is taken at a span
//! edge or at whitespace, so a character is never split.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div, relative,
};

use super::chat_context::{BodyText, ChatContext};
use super::components::ChangeHandler;
use super::ref_chip::{RefChip, RefEvent};
use super::tokens::{LINE_HEIGHT, SPACE_3, SPACE_4, TEXT_DEFAULT};
use crate::appearance::Appearance;
use crate::refs::Span;

/// The most characters of punctuation that stay glued to a chip; a longer
/// token beside a reference wraps as a word of its own.
const MAX_GLUE_CHARS: usize = 8;

/// Past this many words, a line draws its words as runs of text between its
/// chips rather than one element per word.
// shortcut: a run that wraps pushes the chip after it to a new line; flow
// runs into chips if long lines with references become common.
const MAX_FLOW_WORDS: usize = 256;

/// One piece of a laid out body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece<'a> {
    Word(&'a str),
    /// A reference with the punctuation glued to it on either side; `span`
    /// is its index in the spans.
    Ref {
        lead: &'a str,
        text: &'a str,
        tail: &'a str,
        span: usize,
    },
    LineBreak,
}

/// Whether `glue` is short enough to stay beside a chip.
fn short(glue: &str) -> bool {
    glue.chars().nth(MAX_GLUE_CHARS).is_none()
}

/// Splits `body` into words, line breaks and references. `spans` come from
/// `find_refs` (sorted, not overlapping, on character boundaries); a span
/// that breaks those rules is left as plain text rather than trusted.
fn pieces<'a>(body: &'a str, spans: &[Span]) -> Vec<Piece<'a>> {
    let mut out = Vec::new();
    let mut pos = 0;
    for (i, span) in spans.iter().enumerate() {
        let (start, end) = (span.range.start, span.range.end);
        let (Some(gap), Some(text)) = (body.get(pos..start), body.get(start..end)) else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        let words = gap.trim_end_matches(|c: char| !c.is_whitespace());
        let lead = gap.get(words.len()..).unwrap_or_default();
        let (words, lead) = if short(lead) {
            (words, lead)
        } else {
            (gap, "")
        };
        push_words(words, &mut out);
        // The tail runs to the next whitespace, or to the next chip.
        let rest = body.get(end..).unwrap_or_default();
        let to_space = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let to_next = spans
            .get(i + 1..)
            .and_then(|later| later.iter().map(|s| s.range.start).find(|s| *s >= end))
            .map_or(to_space, |next| next - end);
        let tail = rest
            .get(..to_space.min(to_next))
            .or_else(|| rest.get(..to_space))
            .unwrap_or_default();
        let tail = if short(tail) { tail } else { "" };
        out.push(Piece::Ref {
            lead,
            text,
            tail,
            span: i,
        });
        pos = end + tail.len();
    }
    push_words(body.get(pos..).unwrap_or_default(), &mut out);
    out
}

fn push_words<'a>(text: &'a str, out: &mut Vec<Piece<'a>>) {
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push(Piece::LineBreak);
        }
        out.extend(line.split_whitespace().map(Piece::Word));
    }
}

/// One element of a drawn line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item<'a> {
    Word(&'a str),
    /// Words joined by single spaces, drawn as one wrapping text.
    Run(String),
    Ref {
        lead: &'a str,
        text: &'a str,
        tail: &'a str,
        span: usize,
    },
}

/// The pieces as lines of items: word by word, or as runs between chips on
/// a line of more than `MAX_FLOW_WORDS` words.
fn lines(pieces: Vec<Piece<'_>>) -> Vec<Vec<Item<'_>>> {
    let mut lines = Vec::new();
    let mut line = Vec::new();
    for piece in pieces {
        if piece == Piece::LineBreak {
            lines.push(std::mem::take(&mut line));
        } else {
            line.push(piece);
        }
    }
    lines.push(line);
    lines.into_iter().map(items).collect()
}

fn items(line: Vec<Piece<'_>>) -> Vec<Item<'_>> {
    let words = line.iter().filter(|p| matches!(p, Piece::Word(_))).count();
    let flow = words <= MAX_FLOW_WORDS;
    let mut out = Vec::new();
    let mut run = String::new();
    for piece in line {
        match piece {
            Piece::Word(word) if flow => out.push(Item::Word(word)),
            Piece::Word(word) => {
                if !run.is_empty() {
                    run.push(' ');
                }
                run.push_str(word);
            }
            Piece::Ref {
                lead,
                text,
                tail,
                span,
            } => {
                if !run.is_empty() {
                    out.push(Item::Run(std::mem::take(&mut run)));
                }
                out.push(Item::Ref {
                    lead,
                    text,
                    tail,
                    span,
                });
            }
            Piece::LineBreak => {}
        }
    }
    if !run.is_empty() {
        out.push(Item::Run(run));
    }
    out
}

/// A message body. Tests find it as `"{id}:body"` and its chips as
/// `"{id}:ref:{n}"`, `n` counting the body's references from 0.
#[derive(IntoElement)]
pub(crate) struct RichText {
    id: SharedString,
    text: Rc<BodyText>,
    context: Rc<ChatContext>,
    on_ref: Option<ChangeHandler<RefEvent>>,
}

impl std::fmt::Debug for RichText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RichText")
            .field("id", &self.id)
            .field("spans", &self.text.spans)
            .finish_non_exhaustive()
    }
}

impl RichText {
    /// `text` from [`ChatContext::body_of`], so the references are found
    /// once per snapshot rather than once per frame.
    #[must_use]
    pub(crate) fn new(
        id: impl Into<SharedString>,
        text: Rc<BodyText>,
        context: Rc<ChatContext>,
    ) -> Self {
        Self {
            id: id.into(),
            text,
            context,
            on_ref: None,
        }
    }

    /// Called when a chip is clicked or ⌘-clicked.
    #[must_use]
    pub(crate) fn on_ref(mut self, handler: ChangeHandler<RefEvent>) -> Self {
        self.on_ref = Some(handler);
        self
    }

    fn chip(&self, span: usize, lead: &str, text: &str, tail: &str) -> Option<AnyElement> {
        let target = self.text.spans.get(span)?.target.clone();
        let color = self.context.chip_color(&target);
        let id = SharedString::from(format!("{}:ref:{span}", self.id));
        let handler = self.on_ref.clone();
        let chip = RefChip::new(id, text.to_owned(), target, color)
            .card(self.context.clone())
            .when(!lead.is_empty(), RefChip::flush_start)
            .when(!tail.is_empty(), RefChip::flush_end)
            .when_some(handler, |chip, handler| {
                chip.on_ref(move |event, window, cx| handler(event, window, cx))
            });
        // min_w_0 lets a chip wider than the column shrink to it; the glue
        // is short, so it never needs to.
        Some(
            div()
                .flex()
                .min_w_0()
                .items_center()
                .when(!lead.is_empty(), |this| this.child(lead.to_owned()))
                .child(chip)
                .when(!tail.is_empty(), |this| this.child(tail.to_owned()))
                .into_any_element(),
        )
    }
}

fn word(text: impl Into<SharedString>) -> AnyElement {
    // min_w_0 lets a word wider than the column shrink to it and wrap.
    div().min_w_0().child(text.into()).into_any_element()
}

impl RenderOnce for RichText {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let text = self.text.clone();
        let selector = format!("{}:body", self.id);
        let lines = lines(pieces(&text.body, &text.spans))
            .into_iter()
            .map(|items| {
                let elements: Vec<AnyElement> = items
                    .into_iter()
                    .filter_map(|item| match item {
                        Item::Word(text) => Some(word(text.to_owned())),
                        Item::Run(run) => Some(word(run)),
                        Item::Ref {
                            lead,
                            text,
                            tail,
                            span,
                        } => self.chip(span, lead, text, tail),
                    })
                    .collect();
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(SPACE_4)
                    .gap_y(SPACE_3)
                    // An empty line keeps its height, as a blank line in the text.
                    .when(elements.is_empty(), |this| this.child(" "))
                    .children(elements)
            });
        div()
            .debug_selector(|| selector)
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_y(SPACE_3)
            .text_size(TEXT_DEFAULT)
            .text_color(theme.text)
            .line_height(relative(LINE_HEIGHT))
            .children(lines)
    }
}

#[cfg(test)]
#[path = "rich_text_tests.rs"]
mod tests;
