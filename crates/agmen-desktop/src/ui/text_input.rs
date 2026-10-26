//! A single-line text input that works with macOS input methods.
//!
//! The view follows the pattern of gpui's `examples/input.rs`: an element
//! shapes one line, paints the selection and cursor, and registers an
//! [`ElementInputHandler`] so the platform can send composed text.
// shortcut: single line; multi-line wrap and Shift-Enter come when a screen needs them.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId,
    InspectorElementId, InteractiveElement, IntoElement, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, ParentElement, Pixels, Point, Render,
    ShapedLine, SharedString, Style, Styled, TextAlign, TextRun, UTF16Selection, UnderlineStyle,
    Window, actions, div, fill, point, px, relative, size,
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
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Submit,
        Paste,
        Cut,
        Copy,
    ]
);

const CONTEXT: &str = "TextInput";

/// Binds the input's keys; call once at startup.
pub(crate) fn bind_keys(cx: &mut App) {
    let ctx = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("cmd-a", SelectAll, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("cmd-left", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new("cmd-right", End, ctx),
        KeyBinding::new("enter", Submit, ctx),
        KeyBinding::new("cmd-v", Paste, ctx),
        KeyBinding::new("cmd-x", Cut, ctx),
        KeyBinding::new("cmd-c", Copy, ctx),
    ]);
}

/// What the input tells its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextInputEvent {
    /// Enter with non-empty text; the input is cleared.
    Submit(String),
}

/// A single-line text input view.
pub(crate) struct TextInput {
    focus_handle: FocusHandle,
    buffer: TextBuffer,
    placeholder: SharedString,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl TextInput {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            buffer: TextBuffer::default(),
            placeholder: SharedString::default(),
            last_layout: None,
            last_bounds: None,
            selecting: false,
        }
    }

    #[must_use]
    pub(crate) fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the composer reads it in Task 9 of the desktop plan"
        )
    )]
    pub(crate) fn text(&self) -> &str {
        self.buffer.text()
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
        f(&mut self.buffer);
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn on_submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.text().is_empty() || self.buffer.marked().is_some() {
            return;
        }
        let text = self.buffer.take();
        self.changed(window, cx);
        cx.emit(TextInputEvent::Submit(text));
    }

    fn on_paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // One line: newlines in pasted text become spaces.
            self.buffer.replace(None, &text.replace(['\r', '\n'], " "));
            self.changed(window, cx);
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

    fn index_for_point(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.x <= bounds.left() {
            return 0;
        }
        if position.x >= bounds.right() {
            return self.buffer.text().len();
        }
        line.closest_index_for_x(position.x - bounds.left())
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
        self.buffer.replace(range, text);
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
        // The selection arrives in UTF-16 units of `text`; convert within it.
        let selected =
            selected_utf16.map(|s| utf16_to_utf8_in(text, s.start)..utf16_to_utf8_in(text, s.end));
        self.buffer.replace_and_mark(range, text, selected);
        self.changed(window, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        if line.text.as_ref() != self.buffer.text() {
            return None;
        }
        let range = self.buffer.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(range.start), bounds.top()),
            point(bounds.left() + line.x_for_index(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let local = self.last_bounds?.localize(&position)?;
        let line = self.last_layout.as_ref()?;
        if line.text.as_ref() != self.buffer.text() {
            return None;
        }
        Some(
            self.buffer
                .offset_to_utf16(line.closest_index_for_x(local.x)),
        )
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
            .key_context(CONTEXT)
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
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextLine { input: cx.entity() })
    }
}

/// The element that shapes and paints the input's one line.
struct TextLine {
    input: Entity<TextInput>,
}

struct LinePaint {
    line: ShapedLine,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextLine {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextLine {
    type RequestLayoutState = ();
    type PrepaintState = LinePaint;

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
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
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
        let theme = &cx.global::<Appearance>().theme;
        let input = self.input.read(cx);
        let buffer = &input.buffer;
        let style = window.text_style();
        let (text, color): (SharedString, _) = if buffer.text().is_empty() {
            (input.placeholder.clone(), theme.text_placeholder)
        } else {
            (buffer.text().to_owned().into(), style.color)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match buffer.marked() {
            Some(marked) if !buffer.text().is_empty() => [
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
                    len: text.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            _ => vec![run],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(text, font_size, &runs, None);

        let selection = buffer.selection();
        let height = bounds.bottom() - bounds.top();
        let (selection, cursor) = if selection.is_empty() {
            let x = if buffer.text().is_empty() {
                px(0.)
            } else {
                line.x_for_index(buffer.cursor())
            };
            let quad = fill(
                Bounds::new(point(bounds.left() + x, bounds.top()), size(px(2.), height)),
                theme.text_accent,
            );
            (None, Some(quad))
        } else {
            let quad = fill(
                Bounds::from_corners(
                    point(
                        bounds.left() + line.x_for_index(selection.start),
                        bounds.top(),
                    ),
                    point(
                        bounds.left() + line.x_for_index(selection.end),
                        bounds.bottom(),
                    ),
                ),
                theme.element_selected,
            );
            (Some(quad), None)
        };
        LinePaint {
            line,
            cursor,
            selection,
        }
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
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let line = prepaint.line.clone();
        // A failed paint only leaves the line blank this frame.
        if line
            .paint(
                bounds.origin,
                window.line_height(),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .is_err()
        {
            window.refresh();
        }
        if focus.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.input.update(cx, |input, _| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

#[cfg(test)]
#[path = "text_input_tests.rs"]
mod tests;
