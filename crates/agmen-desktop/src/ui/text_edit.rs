//! The text, selection and IME state behind a text input, with no GPUI.
//!
//! Offsets are UTF-8 byte offsets on grapheme boundaries. macOS speaks UTF-16
//! to input methods, so the `*_utf16` helpers convert at that edge.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation as _;

/// Text with a selection (the cursor when empty) and an optional marked
/// range, which is the IME composition in progress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TextBuffer {
    text: String,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
}

impl TextBuffer {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub(crate) const fn is_reversed(&self) -> bool {
        self.reversed
    }

    pub(crate) fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub(crate) const fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    /// Clamps `range` to the text and to char boundaries.
    fn clamp(&self, range: &Range<usize>) -> Range<usize> {
        let fix = |mut i: usize| {
            i = i.min(self.text.len());
            while !self.text.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        let (start, end) = (fix(range.start), fix(range.end));
        start.min(end)..start.max(end)
    }

    /// Replaces `range` (or the marked text, or the selection) with `new`,
    /// ends any composition, and puts the cursor after the new text.
    pub(crate) fn replace(&mut self, range: Option<Range<usize>>, new: &str) {
        let range = self.target(range);
        self.text.replace_range(range.clone(), new);
        let end = range.start + new.len();
        self.selection = end..end;
        self.reversed = false;
        self.marked = None;
    }

    /// Like [`Self::replace`], but marks `new` as composing text and selects
    /// `selected` within it (in bytes of `new`), as an input method asks.
    pub(crate) fn replace_and_mark(
        &mut self,
        range: Option<Range<usize>>,
        new: &str,
        selected: Option<Range<usize>>,
    ) {
        let range = self.target(range);
        self.text.replace_range(range.clone(), new);
        let start = range.start;
        self.marked = (!new.is_empty()).then(|| start..start + new.len());
        self.selection = selected.map_or(start + new.len()..start + new.len(), |s| {
            self.clamp(&(start + s.start.min(new.len())..start + s.end.min(new.len())))
        });
        self.reversed = false;
    }

    fn target(&self, range: Option<Range<usize>>) -> Range<usize> {
        let range = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection.clone());
        self.clamp(&range)
    }

    pub(crate) const fn unmark(&mut self) {
        self.marked = None;
    }

    pub(crate) fn set_selection(&mut self, range: Range<usize>, reversed: bool) {
        self.selection = self.clamp(&range);
        self.reversed = reversed;
    }

    pub(crate) fn move_to(&mut self, offset: usize) {
        let at = self.clamp(&(offset..offset)).start;
        self.selection = at..at;
        self.reversed = false;
    }

    /// Extends the selection so the cursor lands on `offset`.
    pub(crate) fn select_to(&mut self, offset: usize) {
        let offset = self.clamp(&(offset..offset)).start;
        if self.reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }
    }

    pub(crate) fn select_all(&mut self) {
        self.selection = 0..self.text.len();
        self.reversed = false;
    }

    pub(crate) fn move_left(&mut self) {
        if self.selection.is_empty() {
            self.move_to(self.previous_boundary(self.cursor()));
        } else {
            self.move_to(self.selection.start);
        }
    }

    pub(crate) fn move_right(&mut self) {
        if self.selection.is_empty() {
            self.move_to(self.next_boundary(self.cursor()));
        } else {
            self.move_to(self.selection.end);
        }
    }

    pub(crate) fn select_left(&mut self) {
        self.select_to(self.previous_boundary(self.cursor()));
    }

    pub(crate) fn select_right(&mut self) {
        self.select_to(self.next_boundary(self.cursor()));
    }

    /// Deletes the selection, or the grapheme before the cursor. Returns
    /// false when there was nothing to delete.
    pub(crate) fn backspace(&mut self) -> bool {
        if self.selection.is_empty() {
            let prev = self.previous_boundary(self.cursor());
            if prev == self.cursor() {
                return false;
            }
            self.select_to(prev);
        }
        self.replace(None, "");
        true
    }

    /// Deletes the selection, or the grapheme after the cursor.
    pub(crate) fn delete(&mut self) -> bool {
        if self.selection.is_empty() {
            let next = self.next_boundary(self.cursor());
            if next == self.cursor() {
                return false;
            }
            self.select_to(next);
        }
        self.replace(None, "");
        true
    }

    /// Returns the text and leaves the buffer empty.
    pub(crate) fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        *self = Self::default();
        text
    }

    pub(crate) fn selected_text(&self) -> &str {
        self.text.get(self.selection.clone()).unwrap_or_default()
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .find_map(|(i, _)| (i < offset).then_some(i))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .find_map(|(i, _)| (i > offset).then_some(i))
            .unwrap_or(self.text.len())
    }

    pub(crate) fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.text.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    pub(crate) fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.text.chars() {
            if utf16 + ch.len_utf16() > offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    pub(crate) fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    pub(crate) fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    pub(crate) fn len_utf16(&self) -> usize {
        self.text.encode_utf16().count()
    }
}

#[cfg(test)]
#[path = "text_edit_tests.rs"]
mod tests;
