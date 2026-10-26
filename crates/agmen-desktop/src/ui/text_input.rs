//! A text input that works with macOS input methods: one line, or several
//! wrapped lines with Shift-Enter for a newline.
//!
//! The view follows the pattern of gpui's `examples/input.rs`: an element
//! shapes the text, paints the selection and cursor, and registers an
//! [`ElementInputHandler`] so the platform can send composed text. A single
//! line scrolls sideways to keep the cursor in view; several lines wrap,
//! grow up to `max_lines`, then scroll down.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, Font, GlobalElementId, Hsla, InspectorElementId, InteractiveElement, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, ParentElement,
    Pixels, Point, Render, SharedString, Size, Style, Styled, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine, actions, div, fill, point, px, relative, size,
};

use super::text_edit::TextBuffer;
use crate::appearance::Appearance;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Submit,
        Newline,
        Paste,
        Cut,
        Copy,
    ]
);

/// Shaped layouts kept per input; a frame uses at most two widths.
const MAX_CACHED_LAYOUTS: usize = 3;

/// Text past this length is not inserted, so shaping stays bounded. The hub
/// takes bodies of at most 32 KiB, so the cap leaves room for that.
const MAX_TEXT_BYTES: usize = 64 * 1024;

/// Binds the input's keys, which `src/keymap.rs` lists; the app binds
/// them with the rest through `keymap::bind_all`.
#[cfg(test)]
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys(crate::keymap::text_input_bindings());
}

/// What the input tells its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextInputEvent {
    /// Enter with non-empty text; the input is cleared.
    Submit(String),
}

/// A text input view.
pub(crate) struct TextInput {
    focus_handle: FocusHandle,
    buffer: TextBuffer,
    placeholder: SharedString,
    /// 1 for a single line; more lets the text wrap and grow to that many.
    max_lines: usize,
    layout: Option<Rc<TextLayout>>,
    /// The last few shaped layouts and what each was shaped for: layout
    /// asks with the measured width and paint with the final one.
    cache: Vec<LayoutKey>,
    /// How many times the text was shaped, for tests.
    #[cfg(test)]
    pub(super) shapes: usize,
    pub(super) last_bounds: Option<Bounds<Pixels>>,
    /// Where the cursor was painted, for IME placement and tests.
    pub(super) last_cursor: Option<Bounds<Pixels>>,
    /// How many visual lines the text took in the last frame.
    pub(super) visual_lines: usize,
    /// How far the text is scrolled: sideways for one line, down for several.
    scroll: Point<Pixels>,
    /// The x the cursor keeps while moving up and down.
    preferred_x: Option<Pixels>,
    selecting: bool,
    paint_failed: bool,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            buffer: TextBuffer::default(),
            placeholder: SharedString::default(),
            max_lines: 1,
            layout: None,
            cache: Vec::new(),
            #[cfg(test)]
            shapes: 0,
            last_bounds: None,
            last_cursor: None,
            visual_lines: 1,
            scroll: Point::default(),
            preferred_x: None,
            selecting: false,
            paint_failed: false,
        }
    }

    #[must_use]
    pub(crate) fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// Task 19: what the empty input shows, for tests.
    #[cfg(test)]
    pub(crate) fn placeholder_text(&self) -> &str {
        &self.placeholder
    }

    /// Lets the text wrap and grow to `max_lines` before it scrolls, with
    /// Shift-Enter for a newline.
    #[must_use]
    pub(crate) fn multiline(mut self, max_lines: usize) -> Self {
        self.max_lines = max_lines.max(1);
        self
    }

    const fn is_multiline(&self) -> bool {
        self.max_lines > 1
    }

    pub(crate) fn text(&self) -> &str {
        self.buffer.text()
    }

    /// Replaces all the text and puts the cursor at the end.
    pub(crate) fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut text = self.normalize(text);
        let mut end = text.len().min(MAX_TEXT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        self.buffer = TextBuffer::default();
        self.buffer.replace(None, &text);
        self.preferred_x = None;
        self.changed(window, cx);
    }

    /// `text` as this input takes it: one line has no newlines.
    fn normalize(&self, text: &str) -> String {
        if self.is_multiline() {
            text.replace("\r\n", "\n")
        } else {
            text.replace(['\r', '\n'], " ")
        }
    }

    /// `text` ready to replace `range` (or the marked text, or the
    /// selection), or `None` when it would take the text past
    /// [`MAX_TEXT_BYTES`]; then nothing changes and the bell rings.
    fn fit(&self, text: &str, range: Option<Range<usize>>) -> Option<String> {
        let text = self.normalize(text);
        let room = MAX_TEXT_BYTES
            .saturating_sub(self.buffer.text().len())
            .saturating_add(self.buffer.target_len(range));
        (text.len() <= room).then_some(text)
    }

    fn changed(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TextBuffer) -> bool,
    ) {
        self.preferred_x = None;
        if f(&mut self.buffer) {
            self.changed(window, cx);
        } else {
            window.play_system_bell();
        }
    }

    fn select(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TextBuffer),
    ) {
        self.preferred_x = None;
        f(&mut self.buffer);
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.fit(text, None) else {
            window.play_system_bell();
            return;
        };
        self.buffer.replace(None, &text);
        self.preferred_x = None;
        self.changed(window, cx);
    }

    fn on_submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.text().is_empty() || self.buffer.marked().is_some() {
            return;
        }
        let text = self.buffer.take();
        self.scroll = Point::default();
        self.changed(window, cx);
        cx.emit(TextInputEvent::Submit(text));
    }

    fn on_newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_multiline() {
            self.insert("\n", window, cx);
        }
    }

    /// Moves the cursor one visual line up (`rows` = -1) or down (1).
    fn move_vertically(&mut self, rows: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let at = layout.position(self.buffer.cursor());
        let x = *self.preferred_x.get_or_insert(at.x);
        let target = point(
            x,
            at.y + layout.line_height * rows + layout.line_height / 2.,
        );
        let offset = if target.y < px(0.) {
            0
        } else if target.y >= layout.height {
            self.buffer.text().len()
        } else {
            layout.index_at(target)
        };
        let keep = self.preferred_x;
        self.buffer.move_to(offset);
        self.preferred_x = keep;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn on_paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.insert(&text, window, cx);
        }
    }

    fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.buffer.selected_text();
        if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected.to_owned()));
        }
    }

    fn on_cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.buffer.selected_text().to_owned();
        if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected));
            self.buffer.replace(None, "");
            self.changed(window, cx);
        }
    }

    /// The text offset under a window position, from the last layout.
    fn index_for_point(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(layout)) = (self.last_bounds.as_ref(), self.layout.as_ref()) else {
            return 0;
        };
        let local = position - bounds.origin + self.scroll;
        layout.index_at(local)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selecting = true;
        let at = self.index_for_point(event.position);
        self.select(window, cx, |b| {
            if event.modifiers.shift {
                b.select_to(at);
            } else {
                b.move_to(at);
            }
        });
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selecting {
            let at = self.index_for_point(event.position);
            self.select(window, cx, |b| b.select_to(at));
        }
    }

    /// The text runs for `text`: one run, with the marked range underlined.
    fn runs(&self, text_len: usize, color: Hsla, font: &Font) -> Vec<TextRun> {
        let run = TextRun {
            len: text_len,
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        match self.buffer.marked() {
            Some(marked) if !self.buffer.text().is_empty() => [
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: text_len - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            _ => vec![run],
        }
    }
}

/// What a cached layout was shaped for. The text is compared with the
/// buffer, so it is copied only when it changes.
struct LayoutKey {
    text: String,
    empty: bool,
    width: Option<Pixels>,
    font: Font,
    /// The color drawn: the text's, or the placeholder's while empty.
    color: Hsla,
    font_size: Pixels,
    line_height: Pixels,
    marked: Option<Range<usize>>,
    layout: Rc<TextLayout>,
}

/// The inherited text style an input shapes with. It is read where the
/// element tree's style is in effect (request_layout and prepaint), never in
/// a measure closure, which GPUI runs outside it (review C108).
#[derive(Clone)]
struct Shaping {
    font: Font,
    color: Hsla,
    font_size: Pixels,
    line_height: Pixels,
}

impl Shaping {
    fn current(window: &Window) -> Self {
        let style = window.text_style();
        Self {
            font: style.font(),
            color: style.color,
            font_size: style.font_size.to_pixels(window.rem_size()),
            line_height: window.line_height(),
        }
    }
}

impl TextInput {
    /// The layout for `width` (`None` for one line), shaped only when the
    /// text, width, font or composition changed since the last call.
    fn layout_for(
        &mut self,
        width: Option<Pixels>,
        shaping: &Shaping,
        window: &Window,
        cx: &App,
    ) -> Option<Rc<TextLayout>> {
        let (font_size, line_height) = (shaping.font_size, shaping.line_height);
        let empty = self.buffer.text().is_empty();
        // The placeholder stays on one line and is clipped at the edge, as
        // Zed's; only typed text wraps.
        let width = if empty { None } else { width };
        let drawn = drawn_color(self, cx, shaping.color);
        let marked = self.buffer.marked();
        if let Some(key) = self.cache.iter().find(|key| {
            key.text == self.buffer.text()
                && key.empty == empty
                && key.width == width
                && key.font == shaping.font
                && key.color == drawn
                && key.font_size == font_size
                && key.line_height == line_height
                && key.marked == marked
        }) {
            return Some(key.layout.clone());
        }
        let (text, color) = display(self, cx, shaping.color);
        let runs = self.runs(text.len(), color, &shaping.font);
        let layout = Rc::new(TextLayout::shape(
            &text,
            &runs,
            font_size,
            width,
            line_height,
            window,
        )?);
        #[cfg(test)]
        {
            self.shapes += 1;
        }
        if self.cache.len() >= MAX_CACHED_LAYOUTS {
            self.cache.remove(0);
        }
        self.cache.push(LayoutKey {
            text: self.buffer.text().to_owned(),
            empty,
            width,
            font: shaping.font.clone(),
            color: drawn,
            font_size,
            line_height,
            marked,
            layout: layout.clone(),
        });
        Some(layout)
    }
}

/// The shaped text: one [`WrappedLine`] per hard line, stacked.
struct TextLayout {
    lines: Vec<WrappedLine>,
    /// Byte offset where each hard line starts.
    starts: Vec<usize>,
    /// Where each hard line's top is.
    tops: Vec<Pixels>,
    height: Pixels,
    line_height: Pixels,
}

impl TextLayout {
    fn shape(
        text: &SharedString,
        runs: &[TextRun],
        font_size: Pixels,
        wrap: Option<Pixels>,
        line_height: Pixels,
        window: &Window,
    ) -> Option<Self> {
        let lines = window
            .text_system()
            .shape_text(text.clone(), font_size, runs, wrap, None)
            .ok()?;
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        let mut tops = Vec::with_capacity(lines.len());
        let mut height = px(0.);
        for line in &lines {
            tops.push(height);
            height += line.size(line_height).height.max(line_height);
        }
        Some(Self {
            lines: lines.into_vec(),
            starts,
            tops,
            height: height.max(line_height),
            line_height,
        })
    }

    fn visual_lines(&self) -> usize {
        let rows = (self.height / self.line_height).round();
        if rows.is_finite() && rows >= 1. {
            // At most one row per byte, so the cast cannot overflow a usize.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "rows is finite and at least 1"
            )]
            let rows = rows as usize;
            rows
        } else {
            1
        }
    }

    /// The hard line holding `offset`.
    fn line_of(&self, offset: usize) -> usize {
        self.starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1)
            .min(self.lines.len().saturating_sub(1))
    }

    /// Where `offset` sits, relative to the text's top left.
    fn position(&self, offset: usize) -> Point<Pixels> {
        let i = self.line_of(offset);
        let (Some(line), Some(&start), Some(&top)) =
            (self.lines.get(i), self.starts.get(i), self.tops.get(i))
        else {
            return Point::default();
        };
        let local = line
            .position_for_index(offset.saturating_sub(start), self.line_height)
            .unwrap_or_default();
        point(local.x, top + local.y)
    }

    /// The offset nearest a point relative to the text's top left.
    fn index_at(&self, at: Point<Pixels>) -> usize {
        let i = self
            .tops
            .partition_point(|&top| top <= at.y)
            .saturating_sub(1);
        let (Some(line), Some(&start), Some(&top)) =
            (self.lines.get(i), self.starts.get(i), self.tops.get(i))
        else {
            return 0;
        };
        let local = point(at.x.max(px(0.)), (at.y - top).max(px(0.)));
        let ix = match line.closest_index_for_position(local, self.line_height) {
            Ok(ix) | Err(ix) => ix,
        };
        start + ix.min(line.len())
    }

    /// The selection as rectangles, one per visual row it covers.
    fn selection_rects(&self, range: &Range<usize>, width: Pixels) -> Vec<Bounds<Pixels>> {
        let (a, b) = (self.position(range.start), self.position(range.end));
        let lh = self.line_height;
        if (a.y - b.y).abs() < px(0.5) {
            return vec![Bounds::from_corners(a, point(b.x, b.y + lh))];
        }
        let mut rects = vec![Bounds::from_corners(a, point(width, a.y + lh))];
        if b.y > a.y + lh {
            rects.push(Bounds::from_corners(
                point(px(0.), a.y + lh),
                point(width, b.y),
            ));
        }
        rects.push(Bounds::from_corners(
            point(px(0.), b.y),
            point(b.x, b.y + lh),
        ));
        rects
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.buffer.range_from_utf16(&range_utf16);
        actual_range.replace(self.buffer.range_to_utf16(&range));
        self.buffer.text().get(range).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.buffer.range_to_utf16(&self.buffer.selection()),
            reversed: self.buffer.is_reversed(),
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.buffer.range_from_utf16(&range_utf16);
        self.select(window, cx, |b| b.set_selection(range, false));
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.buffer.len_utf16())
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.buffer
            .marked()
            .map(|range| self.buffer.range_to_utf16(&range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.unmark();
        self.changed(window, cx);
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16.map(|r| self.buffer.range_from_utf16(&r));
        let Some(text) = self.fit(text, range.clone()) else {
            window.play_system_bell();
            return;
        };
        self.buffer.replace(range, &text);
        self.preferred_x = None;
        self.changed(window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16.map(|r| self.buffer.range_from_utf16(&r));
        let Some(text) = self.fit(text, range.clone()) else {
            window.play_system_bell();
            return;
        };
        // The selection arrives in UTF-16 units of `text`; convert within it.
        let selected = selected_utf16
            .map(|s| utf16_to_utf8_in(&text, s.start)..utf16_to_utf8_in(&text, s.end));
        self.buffer.replace_and_mark(range, &text, selected);
        self.changed(window, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.buffer.range_from_utf16(&range_utf16);
        let origin = bounds.origin - self.scroll;
        let (a, b) = (layout.position(range.start), layout.position(range.end));
        Some(Bounds::from_corners(
            origin + a,
            origin + point(b.x.max(a.x), b.y + layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let layout = self.layout.as_ref()?;
        let local = position - bounds.origin + self.scroll;
        Some(self.buffer.offset_to_utf16(layout.index_at(local)))
    }
}

fn utf16_to_utf8_in(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, ch) in text.char_indices() {
        if units >= utf16 {
            return i;
        }
        units += ch.len_utf16();
    }
    text.len()
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .key_context(crate::keymap::TEXT_INPUT_CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(
                cx.listener(|this, _: &Backspace, w, cx| this.edit(w, cx, TextBuffer::backspace)),
            )
            .on_action(cx.listener(|this, _: &Delete, w, cx| this.edit(w, cx, TextBuffer::delete)))
            .on_action(
                cx.listener(|this, _: &Left, w, cx| this.select(w, cx, TextBuffer::move_left)),
            )
            .on_action(
                cx.listener(|this, _: &Right, w, cx| this.select(w, cx, TextBuffer::move_right)),
            )
            .on_action(cx.listener(|this, _: &Up, w, cx| this.move_vertically(-1., w, cx)))
            .on_action(cx.listener(|this, _: &Down, w, cx| this.move_vertically(1., w, cx)))
            .on_action(cx.listener(|this, _: &SelectLeft, w, cx| {
                this.select(w, cx, TextBuffer::select_left)
            }))
            .on_action(cx.listener(|this, _: &SelectRight, w, cx| {
                this.select(w, cx, TextBuffer::select_right)
            }))
            .on_action(
                cx.listener(|this, _: &SelectAll, w, cx| {
                    this.select(w, cx, TextBuffer::select_all)
                }),
            )
            .on_action(cx.listener(|this, _: &Home, w, cx| this.select(w, cx, |b| b.move_to(0))))
            .on_action(
                cx.listener(|this, _: &End, w, cx| this.select(w, cx, |b| b.move_to(usize::MAX))),
            )
            .on_action(cx.listener(Self::on_submit))
            .on_action(cx.listener(Self::on_newline))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextArea { input: cx.entity() })
    }
}

/// The element that shapes and paints the input's text.
struct TextArea {
    input: Entity<TextInput>,
}

struct AreaPaint {
    layout: Rc<TextLayout>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

/// The text to shape and its color: the placeholder while empty.
fn display(input: &TextInput, cx: &App, color: Hsla) -> (SharedString, Hsla) {
    let drawn = drawn_color(input, cx, color);
    if input.buffer.text().is_empty() {
        (input.placeholder.clone(), drawn)
    } else {
        (input.buffer.text().to_owned().into(), drawn)
    }
}

/// The color the input draws its text in: the placeholder's while empty.
/// The layout cache keys on this, so the two never drift apart (C141).
fn drawn_color(input: &TextInput, cx: &App, color: Hsla) -> Hsla {
    if input.buffer.text().is_empty() {
        cx.global::<Appearance>().theme.text_placeholder
    } else {
        color
    }
}

impl IntoElement for TextArea {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextArea {
    type RequestLayoutState = ();
    type PrepaintState = Option<AreaPaint>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let line_height = window.line_height();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if !input.is_multiline() {
            style.size.height = line_height.into();
            return (window.request_layout(style, [], cx), ());
        }
        // Several lines: as tall as the wrapped text, from 1 to max_lines rows.
        let max_lines = input.max_lines;
        let entity = self.input.clone();
        let shaping = Shaping::current(window);
        let measure = move |known: Size<Option<Pixels>>,
                            available: Size<AvailableSpace>,
                            window: &mut Window,
                            cx: &mut App| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(w) => Some(w),
                AvailableSpace::MinContent | AvailableSpace::MaxContent => None,
            });
            let rows = entity
                .update(cx, |input, cx| {
                    input.layout_for(width, &shaping, window, cx)
                })
                .map_or(1, |layout| layout.visual_lines())
                .clamp(1, max_lines);
            // rows is at most max_lines, a small number.
            #[expect(clippy::cast_precision_loss, reason = "rows is a small count")]
            let rows = rows as f32;
            size(width.unwrap_or_default(), line_height * rows)
        };
        (window.request_measured_layout(style, measure), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let line_height = window.line_height();
        let multiline = self.input.read(cx).is_multiline();
        let wrap = multiline.then_some(bounds.size.width);
        let layout = self.input.update(cx, |input, cx| {
            input.layout_for(wrap, &Shaping::current(window), window, cx)
        })?;
        let input = self.input.read(cx);

        let empty = input.buffer.text().is_empty();
        let selection = input.buffer.selection();
        let cursor_at = if empty {
            Point::default()
        } else {
            layout.position(input.buffer.cursor())
        };
        // Scroll so the cursor stays in view.
        let mut scroll = input.scroll;
        if multiline {
            let view = bounds.size.height;
            if cursor_at.y < scroll.y {
                scroll.y = cursor_at.y;
            } else if cursor_at.y + line_height > scroll.y + view {
                scroll.y = cursor_at.y + line_height - view;
            }
            scroll.y = scroll.y.clamp(px(0.), (layout.height - view).max(px(0.)));
            scroll.x = px(0.);
        } else {
            let view = bounds.size.width - px(2.);
            if cursor_at.x < scroll.x {
                scroll.x = cursor_at.x;
            } else if cursor_at.x > scroll.x + view {
                scroll.x = cursor_at.x - view;
            }
            scroll.x = scroll.x.max(px(0.));
            scroll.y = px(0.);
        }
        let origin = bounds.origin - scroll;
        let theme = &cx.global::<Appearance>().theme;
        let (selection, cursor) = if selection.is_empty() || empty {
            let quad = fill(
                Bounds::new(origin + cursor_at, size(px(2.), line_height)),
                theme.text_accent,
            );
            (Vec::new(), Some(quad))
        } else {
            let rects = layout
                .selection_rects(&selection, bounds.size.width)
                .into_iter()
                .map(|r| {
                    fill(
                        Bounds::new(origin + r.origin, r.size),
                        theme.element_selected,
                    )
                })
                .collect();
            (rects, None)
        };
        let visual_lines = layout.visual_lines();
        let cursor_bounds = cursor.as_ref().map(|q| q.bounds);
        self.input.update(cx, |input, _| {
            input.scroll = scroll;
            input.visual_lines = visual_lines;
            input.last_cursor = cursor_bounds;
        });
        Some(AreaPaint {
            layout,
            cursor,
            selection,
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let Some(paint) = prepaint.take() else {
            return;
        };
        let scroll = self.input.read(cx).scroll;
        let mut failed = false;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in paint.selection {
                window.paint_quad(quad);
            }
            for (line, top) in paint.layout.lines.iter().zip(&paint.layout.tops) {
                let at = bounds.origin - scroll + point(px(0.), *top);
                failed |= line
                    .paint(
                        at,
                        paint.layout.line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .is_err();
            }
            if focus.is_focused(window)
                && let Some(cursor) = paint.cursor
            {
                window.paint_quad(cursor);
            }
        });
        self.input.update(cx, |input, _| {
            if failed && !input.paint_failed {
                // Logged once: a failing paint repeats every frame.
                eprintln!("agmen-desktop: a text input line failed to paint");
            }
            input.paint_failed = failed;
            input.layout = Some(paint.layout);
            input.last_bounds = Some(bounds);
        });
    }
}

#[cfg(test)]
#[path = "text_input_tests.rs"]
mod tests;
