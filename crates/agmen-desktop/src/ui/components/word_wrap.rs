//! Text that breaks lines only at spaces (V77, V131, C321): gpui's line
//! wrapper also breaks before '/', which splits a handle such as
//! "andy/fx1" or a path such as "~/.config/zed/themes".

use gpui::prelude::Refineable as _;
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, RenderOnce,
    SharedString, StyleRefinement, Styled, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::SPACE_3;

/// One flex item per word, so a line breaks only at a space, while a word
/// wider than the line still wraps inside itself. The gap is one space of
/// the words' own font, which comes from the Appearance fonts rather than
/// the caller's text style, for the words as for the gap, and flex leaves
/// it out at a line's end, so text that fits exactly stays on one line
/// (C219). Tests find word `i` as
/// `"{selector}:{i}"`. Callers lay the box out through [`Styled`].
#[derive(Debug, IntoElement)]
pub(crate) struct WordWrap {
    selector: SharedString,
    text: SharedString,
    size: Option<LabelSize>,
    code: bool,
    color: Color,
    line_height: Option<Pixels>,
    style: StyleRefinement,
}

impl WordWrap {
    #[must_use]
    pub(crate) fn new(selector: impl Into<SharedString>, text: impl Into<SharedString>) -> Self {
        Self {
            selector: selector.into(),
            text: text.into(),
            size: None,
            code: false,
            color: Color::Default,
            line_height: None,
            style: StyleRefinement::default(),
        }
    }

    /// As [`Label::size`].
    #[must_use]
    pub(crate) const fn size(mut self, size: LabelSize) -> Self {
        self.size = Some(size);
        self
    }

    /// As [`Label::code`]: the code font, at its own size unless
    /// [`WordWrap::size`] sets one.
    #[must_use]
    pub(crate) const fn code(mut self) -> Self {
        self.code = true;
        self
    }

    #[must_use]
    pub(crate) fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    #[must_use]
    pub(crate) const fn line_height(mut self, height: Pixels) -> Self {
        self.line_height = Some(height);
        self
    }

    /// The words' family, as [`Label`] picks it but with the UI family
    /// set rather than inherited, and one space of it at their size.
    fn word_font(&self, window: &Window, cx: &App) -> (SharedString, Pixels) {
        let fonts = &cx.global::<Appearance>().fonts;
        let family = if self.code {
            fonts.code_family.clone()
        } else {
            fonts.ui_family.clone()
        };
        let size = match (self.code, self.size) {
            (true, None) => fonts.code_size,
            (_, size) => size.unwrap_or_default().rems().to_pixels(window.rem_size()),
        };
        let text = window.text_system();
        let space = text
            .advance(text.resolve_font(&gpui::font(family.clone())), size, ' ')
            .map_or(SPACE_3, |advance| advance.width);
        (family, space)
    }
}

impl Styled for WordWrap {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for WordWrap {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (family, space) = self.word_font(window, cx);
        let selector = &self.selector;
        // Runs of spaces and edge spaces draw no empty words; tabs and
        // newlines also split (C240).
        let words = self.text.split_whitespace().enumerate().map(|(i, word)| {
            let selector = format!("{selector}:{i}");
            let mut label = Label::new(word.to_owned()).color(self.color.clone()).wrap();
            if let Some(size) = self.size {
                label = label.size(size);
            }
            if self.code {
                label = label.code();
            }
            if let Some(height) = self.line_height {
                label = label.line_height(height);
            }
            div().debug_selector(|| selector).max_w_full().child(label)
        });
        let mut wrap = div().font_family(family).flex().flex_wrap().gap_x(space);
        // The caller's styles win over the defaults.
        wrap.style().refine(&self.style);
        wrap.children(words)
    }
}

#[cfg(test)]
#[path = "word_wrap_tests.rs"]
mod tests;
