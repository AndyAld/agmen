//! Wrapping UI text with command names as monospace chips, as the add
//! agent dialog's "agmen looks for grok on PATH, runs grok --version, ..."
//! (node 2440, mockup fix 48). The source marks each code span with
//! backticks, which are not drawn. Punctuation touching a span stays with
//! its chip (fix 54).

use std::ops::Range;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, RenderOnce,
    SharedString, Styled as _, Window, div, px, relative,
};

use super::{Chip, Color, LabelSize};
use crate::appearance::Appearance;
use crate::ui::inline_flow::{FlowItem, InlineFlow};
use crate::ui::tokens::LINE_HEIGHT;

/// `source` without its backticks, and the byte ranges of the code spans in
/// the result. A backtick with no partner is drawn as itself.
#[must_use]
pub(crate) fn split_code(source: &str) -> (String, Vec<Range<usize>>) {
    let mut text = String::with_capacity(source.len());
    let mut spans = Vec::new();
    let mut rest = source;
    while let Some((before, after)) = rest.split_once('`')
        && let Some((code, tail)) = after.split_once('`')
    {
        text.push_str(before);
        let start = text.len();
        text.push_str(code);
        spans.push(start..text.len());
        rest = tail;
    }
    text.push_str(rest);
    (text, spans)
}

/// A part of the drawn text: words, or a code span with the punctuation
/// that touches it on either side.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece<'a> {
    /// Words joined by single spaces.
    Words(String),
    Code {
        lead: &'a str,
        code: &'a str,
        tail: &'a str,
    },
}

/// `text` split at its code `spans` (from [`split_code`]) into words and
/// code spans, each span with the punctuation that touches it (fix 54).
fn pieces<'a>(text: &'a str, spans: &[Range<usize>]) -> Vec<Piece<'a>> {
    let mut out = Vec::with_capacity(spans.len() * 2 + 1);
    let push_words = |out: &mut Vec<Piece<'a>>, words: &str| {
        let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
        if !words.is_empty() {
            out.push(Piece::Words(words));
        }
    };
    let mut at = 0;
    for (ix, span) in spans.iter().enumerate() {
        let before = text.get(at..span.start).unwrap_or_default();
        let words = before.trim_end_matches(|c: char| !c.is_whitespace());
        push_words(&mut out, words);
        let next = spans.get(ix + 1).map_or(text.len(), |next| next.start);
        let after = text.get(span.end..next).unwrap_or_default();
        let tail_len = after.find(char::is_whitespace).unwrap_or(after.len());
        out.push(Piece::Code {
            lead: before.get(words.len()..).unwrap_or_default(),
            code: text.get(span.clone()).unwrap_or_default(),
            tail: after.get(..tail_len).unwrap_or_default(),
        });
        at = span.end + tail_len;
    }
    push_words(&mut out, text.get(at..).unwrap_or_default());
    out
}

#[derive(Debug, IntoElement)]
pub(crate) struct CodeText {
    source: SharedString,
    size: LabelSize,
    color: Color,
}

impl CodeText {
    /// `source` with its code spans between backticks.
    #[must_use]
    pub(crate) fn new(source: impl Into<SharedString>) -> Self {
        Self {
            source: source.into(),
            size: LabelSize::Default,
            color: Color::Default,
        }
    }

    #[must_use]
    pub(crate) const fn size(mut self, size: LabelSize) -> Self {
        self.size = size;
        self
    }

    #[must_use]
    pub(crate) fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

impl RenderOnce for CodeText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.hsla(&cx.global::<Appearance>().theme);
        // The prose's line, snapped as the flow snaps it: a chip is no
        // taller, so the lines keep the mockup's pitch (node 2440).
        let line = window.pixel_snap(self.size.rems().to_pixels(window.rem_size()) * LINE_HEIGHT);
        let (text, spans) = split_code(&self.source);
        let items = pieces(&text, &spans)
            .into_iter()
            .map(|piece| match piece {
                Piece::Words(words) => FlowItem::Run(words.into()),
                Piece::Code { lead, code, tail } => FlowItem::Chip {
                    element: chip(lead, code, tail, self.color.clone(), line),
                    label: code.to_owned().into(),
                },
            })
            .collect();
        div()
            .min_w_0()
            .text_size(self.size.rems())
            .text_color(color)
            .line_height(relative(LINE_HEIGHT))
            .child(InlineFlow::new(items).line_gap(px(0.)))
    }
}

/// A command as a monospace chip in `color`'s reference fill (fix 48),
/// `line` tall, with the punctuation that touches it (fix 54). Tests find
/// it as `"code-text:chip"`.
fn chip(lead: &str, code: &str, tail: &str, color: Color, line: Pixels) -> AnyElement {
    let chip = Chip::new(code.to_owned())
        .reference()
        .code()
        .tinted(color)
        .line_height(line);
    let chip = if lead.is_empty() {
        chip
    } else {
        chip.flush_start()
    };
    let chip = if tail.is_empty() {
        chip
    } else {
        chip.flush_end()
    };
    // As a message's reference chip: the flow lays this out as a root,
    // capped at the column's width.
    div()
        .debug_selector(|| "code-text:chip".into())
        .flex()
        .max_w_full()
        .min_w_0()
        .items_center()
        .when(!lead.is_empty(), |this| this.child(lead.to_owned()))
        .child(chip)
        .when(!tail.is_empty(), |this| this.child(tail.to_owned()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[test]
    fn backticks_mark_code_spans_and_are_dropped() {
        let (text, spans) = split_code("runs `grok --version`, and `x`");
        assert_eq!(text, "runs grok --version, and x");
        assert_eq!(spans, [5..19, 25..26]);
        assert_eq!(&text[spans[0].clone()], "grok --version");
    }

    #[test]
    fn a_lone_backtick_stays() {
        let (text, spans) = split_code("a `b` c ` d");
        assert_eq!(text, "a b c ` d");
        assert_eq!((spans.len(), spans.first()), (1, Some(&(2..3))));
        assert_eq!(split_code("plain").0, "plain");
    }

    #[test]
    fn code_spans_become_chips_with_their_punctuation() {
        // Fix 54: punctuation that touches a span stays with its chip.
        let source = "runs `grok --version`, then (`agmen agents scan`) and `x` y";
        let (text, spans) = split_code(source);
        assert_eq!(
            pieces(&text, &spans),
            [
                Piece::Words("runs".into()),
                Piece::Code {
                    lead: "",
                    code: "grok --version",
                    tail: ","
                },
                Piece::Words("then".into()),
                Piece::Code {
                    lead: "(",
                    code: "agmen agents scan",
                    tail: ")"
                },
                Piece::Words("and".into()),
                Piece::Code {
                    lead: "",
                    code: "x",
                    tail: ""
                },
                Piece::Words("y".into()),
            ]
        );
        assert_eq!(
            pieces("plain words", &[]),
            [Piece::Words("plain words".into())]
        );
    }

    #[gpui::test]
    fn a_command_draws_as_a_chip(cx: &mut TestAppContext) {
        // Fix 48: command names are monospace chips, padded and filled.
        let cx = crate::ui::test_support::draw(cx, || {
            div()
                .w(px(600.))
                .child(CodeText::new("runs `grok --version`, and checks it."))
                .into_any_element()
        });
        let chip = cx.debug_bounds("code-text:chip").expect("a chip");
        assert!(chip.size.width > px(0.));
    }

    #[gpui::test]
    fn chips_keep_the_prose_line_pitch(cx: &mut TestAppContext) {
        // Node 2440: 12 px prose lines 14 px apart. A chip sits in the
        // line, so the lines that hold one are no taller and no further
        // apart than plain ones.
        let plain = "agmen looks for grok on PATH, runs grok --version, and checks that it starts.";
        let chips =
            "agmen looks for `grok` on PATH, runs `grok --version`, and checks that it starts.";
        let cx = crate::ui::test_support::draw(cx, move || {
            let block = |id: &'static str, source: &'static str, width: f32| {
                div()
                    .debug_selector(move || id.into())
                    .w(px(width))
                    .child(CodeText::new(source).size(LabelSize::Small))
            };
            div()
                .flex()
                .flex_col()
                .items_start()
                .child(block("plain-one", plain, 900.))
                .child(block("chips-one", chips, 900.))
                .child(block("plain-two", plain, 330.))
                .child(block("chips-two", chips, 330.))
                .into_any_element()
        });
        let [plain_one, chips_one, plain_two, chips_two] =
            ["plain-one", "chips-one", "plain-two", "chips-two"]
                .map(|id| cx.debug_bounds(id).expect(id).size.height);
        assert_eq!(chips_one, plain_one);
        assert_eq!(plain_two, plain_one * 2., "the plain text wraps once");
        assert_eq!(chips_two, plain_two);
    }

    #[gpui::test]
    fn code_text_wraps_in_a_narrow_box(cx: &mut TestAppContext) {
        let source = "agmen looks for `grok` on PATH, runs `grok --version`, and checks it.";
        let wide = rendered_size(cx, move || {
            div()
                .w(px(600.))
                .child(CodeText::new(source))
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let narrow = rendered_size(&mut cx2, move || {
            div()
                .w(px(120.))
                .child(CodeText::new(source))
                .into_any_element()
        });
        assert!(narrow.height > wide.height * 2., "{narrow:?} {wide:?}");
    }
}
