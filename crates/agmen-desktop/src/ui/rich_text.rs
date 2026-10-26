//! A message body with its references drawn as chips (`rich_body` in
//! `docs/mockups/scripts/console.py`).
//!
//! The body is laid out word by word in a wrapping row, as the mockup does,
//! so a chip flows with the words around it. Punctuation glued to a
//! reference stays beside its chip with no gap (mockup fix 54). A word wider
//! than the column, such as a 500 character token, shrinks to the column and
//! wraps inside itself. Every slice is taken at a span edge or at
//! whitespace, so a character is never split.

use std::rc::Rc;

use agmen_proto::Ref;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div, relative,
};

use super::components::ChangeHandler;
use super::ref_chip::{ChatContext, RefChip, RefEvent};
use super::tokens::{LINE_HEIGHT, SPACE_3, SPACE_4, TEXT_DEFAULT};
use crate::appearance::Appearance;
use crate::refs::Span;

/// One piece of a laid out body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Piece<'a> {
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

/// Splits `body` into words, line breaks and references. `spans` come from
/// `find_refs` (sorted, not overlapping, on character boundaries); a span
/// that breaks those rules is left as plain text rather than trusted.
#[must_use]
pub(crate) fn pieces<'a>(body: &'a str, spans: &[Span]) -> Vec<Piece<'a>> {
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

/// A message body. Tests find it as `"{id}:body"` and its chips as
/// `"{id}:ref:{n}"`, `n` counting the body's references from 0.
#[derive(IntoElement)]
pub(crate) struct RichText {
    id: SharedString,
    body: SharedString,
    spans: Vec<Span>,
    context: Rc<ChatContext>,
    on_ref: Option<ChangeHandler<RefEvent>>,
}

impl std::fmt::Debug for RichText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RichText")
            .field("id", &self.id)
            .field("spans", &self.spans)
            .finish_non_exhaustive()
    }
}

impl RichText {
    /// `hub_refs` are the message's `refs`; the context confirms the git
    /// names in the text.
    #[must_use]
    pub(crate) fn new(
        id: impl Into<SharedString>,
        body: impl Into<SharedString>,
        hub_refs: &[Ref],
        context: Rc<ChatContext>,
    ) -> Self {
        let body = body.into();
        let spans = context.spans(&body, hub_refs);
        Self {
            id: id.into(),
            body,
            spans,
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
        let target = self.spans.get(span)?.target.clone();
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
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .when(!lead.is_empty(), |this| this.child(lead.to_owned()))
                .child(chip)
                .when(!tail.is_empty(), |this| this.child(tail.to_owned()))
                .into_any_element(),
        )
    }
}

fn word(text: &str) -> AnyElement {
    // min_w_0 lets a word wider than the column shrink to it and wrap.
    div().min_w_0().child(text.to_owned()).into_any_element()
}

impl RenderOnce for RichText {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let mut lines: Vec<Vec<AnyElement>> = vec![Vec::new()];
        let body = self.body.clone();
        let selector = format!("{}:body", self.id);
        for piece in pieces(&body, &self.spans) {
            let element = match piece {
                Piece::Word(text) => Some(word(text)),
                Piece::Ref {
                    lead,
                    text,
                    tail,
                    span,
                } => self.chip(span, lead, text, tail),
                Piece::LineBreak => {
                    lines.push(Vec::new());
                    None
                }
            };
            if let (Some(element), Some(line)) = (element, lines.last_mut()) {
                line.push(element);
            }
        }
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
            .children(lines.into_iter().map(|items| {
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(SPACE_4)
                    .gap_y(SPACE_3)
                    // An empty line keeps its height, as a blank line in the text.
                    .when(items.is_empty(), |this| this.child(" "))
                    .children(items)
            }))
    }
}

#[cfg(test)]
#[path = "rich_text_tests.rs"]
mod tests;
