//! One line of text with some ranges drawn in another color: a palette
//! row's label with the characters the query matched.

use std::ops::Range;

use gpui::{
    App, FontWeight, HighlightStyle, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, StyledText, Window, div, relative,
};

use super::Color;
use crate::appearance::Appearance;
use crate::ui::tokens::{LINE_HEIGHT, TEXT_DEFAULT};

/// Text in the default size and `text` color, with `ranges` in the accent
/// color and semibold (node 11485). Truncates with an ellipsis.
#[derive(Debug, IntoElement)]
pub(crate) struct HighlightedLabel {
    text: SharedString,
    ranges: Vec<Range<usize>>,
}

impl HighlightedLabel {
    /// `ranges` are byte ranges in `text`, in order. A range that is out of
    /// order, past the end or off a character boundary is dropped.
    #[must_use]
    pub(crate) fn new(text: impl Into<SharedString>, ranges: Vec<Range<usize>>) -> Self {
        let text = text.into();
        let mut end = 0;
        let ranges = ranges
            .into_iter()
            .filter(|r| {
                let ok = r.start >= end
                    && r.start < r.end
                    && text.is_char_boundary(r.start)
                    && text.is_char_boundary(r.end)
                    && r.end <= text.len();
                if ok {
                    end = r.end;
                }
                ok
            })
            .collect();
        Self { text, ranges }
    }

    /// The ranges drawn highlighted.
    #[cfg(test)]
    pub(crate) fn ranges(&self) -> &[Range<usize>] {
        &self.ranges
    }
}

impl RenderOnce for HighlightedLabel {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let style = HighlightStyle {
            color: Some(Color::Accent.hsla(theme)),
            font_weight: Some(FontWeight::SEMIBOLD),
            ..HighlightStyle::default()
        };
        let text =
            StyledText::new(self.text).with_highlights(self.ranges.into_iter().map(|r| (r, style)));
        div()
            .min_w_0()
            .text_color(Color::Default.hsla(theme))
            .text_size(TEXT_DEFAULT)
            .line_height(relative(LINE_HEIGHT))
            .truncate()
            .child(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::TestAppContext;

    #[test]
    fn highlighted_label_keeps_ordered_ranges_on_char_boundaries() {
        let label = HighlightedLabel::new("agmen: spawn é", vec![7..12, 13..14, 3..4, 13..15]);
        // 13..14 splits "é"; 3..4 is out of order; 13..15 is "é" itself.
        assert_eq!(label.ranges(), [7..12, 13..15]);
        let past_end = HighlightedLabel::new("abc", std::iter::once(1..9).collect());
        assert!(past_end.ranges().is_empty());
    }

    #[gpui::test]
    fn highlighted_label_renders_one_line(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            HighlightedLabel::new(
                "agmen: spawn worker with codex",
                std::iter::once(7..12).collect(),
            )
            .into_any_element()
        });
        assert!(size.width > gpui::px(0.));
        let mut cx2 = TestAppContext::single();
        let plain = rendered_size(&mut cx2, || {
            HighlightedLabel::new("agmen", vec![]).into_any_element()
        });
        assert_eq!(size.height, plain.height);
    }
}
