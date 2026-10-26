//! Wrapping UI text with command names in the code font, as the add agent
//! dialog's "agmen looks for grok on PATH, runs grok --version, ..." (node
//! 2440, mockup fix 48). The source marks each code span with backticks,
//! which are not drawn. Punctuation after a span touches it (fix 54).

use std::ops::Range;

use gpui::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, StyledText,
    TextRun, Window, div, font, relative,
};

use super::{Color, LabelSize};
use crate::appearance::Appearance;
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
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        let color = self.color.hsla(&appearance.theme);
        let (text, spans) = split_code(&self.source);
        let run = |len: usize, family: &SharedString| TextRun {
            len,
            font: font(family.clone()),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let (ui, code) = (&appearance.fonts.ui_family, &appearance.fonts.code_family);
        let mut runs = Vec::with_capacity(spans.len() * 2 + 1);
        let mut at = 0;
        for span in spans {
            if span.start > at {
                runs.push(run(span.start - at, ui));
            }
            runs.push(run(span.len(), code));
            at = span.end;
        }
        if text.len() > at {
            runs.push(run(text.len() - at, ui));
        }
        // Code spans take the text's size, so a line keeps one height.
        div()
            .min_w_0()
            .text_size(self.size.rems())
            .line_height(relative(LINE_HEIGHT))
            .child(StyledText::new(text).with_runs(runs))
    }
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
