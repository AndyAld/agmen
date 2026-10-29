//! The command palette (⌘⇧P): a query over every action the focused view
//! can run, fuzzy filtered as you type, each row with its key hint.

use std::ops::Range;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Action, AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ListSizingBehavior, ParentElement as _, Render,
    ScrollStrategy, SharedString, Styled as _, Subscription, UniformListScrollHandle, Window, div,
    uniform_list,
};

use super::components::{
    Card, Color, HighlightedLabel, Icon, IconSize, Indent, KeyHint, Label, LabelSize, ListItem,
};
use super::palette_filter::{Match, filter};
use super::text_input::TextInput;
use super::tokens::{
    BORDER_WIDTH, ICON_MD, ICON_SM, PALETTE_INPUT_HEIGHT, PALETTE_INPUT_LINE, PALETTE_MAX_HEIGHT,
    PALETTE_QUERY_TEXT, PALETTE_ROW_HEIGHT, PALETTE_WIDTH, RADIUS_MD, SPACE_4, SPACE_8, SPACE_14,
};
use crate::appearance::Appearance;
use crate::icons::UiIcon;
use crate::keymap::{Cancel, Confirm, PALETTE_CONTEXT, SelectNext, SelectPrevious};

/// The palette shows at most this many entries; the keymap has about 30
/// and one row per installed agent joins them.
const MAX_ENTRIES: usize = 256;

/// One row the palette can show.
pub(crate) struct PaletteEntry {
    pub(crate) label: SharedString,
    pub(crate) action: Box<dyn Action>,
    /// The key hint, as `⌘⇧P`.
    pub(crate) keys: Option<SharedString>,
    /// Why the row's action will be refused, drawn where a key hint goes:
    /// an outdated agent's spawn row says what to update (C261).
    pub(crate) note: Option<(SharedString, Color)>,
}

impl PaletteEntry {
    #[must_use]
    pub(crate) fn new(label: impl Into<SharedString>, action: Box<dyn Action>) -> Self {
        Self {
            label: label.into(),
            action,
            keys: None,
            note: None,
        }
    }

    #[must_use]
    pub(crate) fn keys(mut self, keys: Option<SharedString>) -> Self {
        self.keys = keys;
        self
    }

    #[must_use]
    pub(crate) fn note(mut self, note: Option<(impl Into<SharedString>, Color)>) -> Self {
        self.note = note.map(|(text, color)| (text.into(), color));
        self
    }
}

impl std::fmt::Debug for PaletteEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaletteEntry")
            .field("label", &self.label)
            .field("action", &self.action.name())
            .field("keys", &self.keys)
            .field("note", &self.note)
            .finish()
    }
}

/// What the palette asks its owner to do.
pub(crate) enum PaletteEvent {
    /// Close it: Escape, a click outside, or Enter with nothing matching.
    Dismissed,
    /// Close it and run this action where the focus was.
    Confirmed(Box<dyn Action>),
}

impl std::fmt::Debug for PaletteEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dismissed => f.write_str("Dismissed"),
            Self::Confirmed(action) => write!(f, "Confirmed({})", action.name()),
        }
    }
}

/// The palette view. Its owner places it and closes it on a
/// [`PaletteEvent`].
pub(crate) struct CommandPalette {
    focus: FocusHandle,
    input: Entity<TextInput>,
    entries: Vec<PaletteEntry>,
    labels: Vec<SharedString>,
    /// The query the matches were computed for.
    query: String,
    matches: Vec<Match>,
    /// An index into `matches`; 0 when there are none.
    selected: usize,
    scroll: UniformListScrollHandle,
    _input_changes: Subscription,
}

impl std::fmt::Debug for CommandPalette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandPalette")
            .field("query", &self.query)
            .field("matches", &self.matches.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<PaletteEvent> for CommandPalette {}

impl CommandPalette {
    /// The palette over `entries` with an empty query, its input focused.
    pub(crate) fn new(
        mut entries: Vec<PaletteEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        entries.truncate(MAX_ENTRIES);
        let input = cx.new(|cx| {
            TextInput::new(cx)
                .placeholder("Run a command\u{2026}")
                .line_height(PALETTE_INPUT_LINE)
        });
        let input_changes = cx.observe(&input, |this, input, cx| {
            let text = input.read(cx).text();
            if text != this.query {
                this.query = text.to_owned();
                this.refilter();
                cx.notify();
            }
        });
        window.focus(&input.focus_handle(cx), cx);
        let labels = entries.iter().map(|e| e.label.clone()).collect();
        let mut palette = Self {
            focus: cx.focus_handle(),
            input,
            entries,
            labels,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            _input_changes: input_changes,
        };
        palette.refilter();
        palette
    }

    /// Replaces the query, as ⌘N's "spawn worker with ".
    pub(crate) fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_text(query, window, cx));
    }

    /// The query the shown rows match.
    #[cfg(test)]
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// The labels of the shown rows, best match first.
    #[cfg(test)]
    pub(crate) fn shown(&self) -> impl Iterator<Item = &str> {
        self.matches
            .iter()
            .filter_map(|m| self.labels.get(m.index))
            .map(SharedString::as_ref)
    }

    /// The selected row's label, `None` when nothing matches.
    #[cfg(test)]
    pub(crate) fn selected_label(&self) -> Option<&str> {
        let m = self.matches.get(self.selected)?;
        self.labels.get(m.index).map(SharedString::as_ref)
    }

    /// The matched ranges of the shown row labelled `label`.
    #[cfg(test)]
    pub(crate) fn ranges_of(&self, label: &str) -> Option<&[Range<usize>]> {
        self.matches
            .iter()
            .find(|m| self.labels.get(m.index).is_some_and(|l| l == label))
            .map(|m| m.ranges.as_slice())
    }

    /// Recomputes the matches for `query`, keeping the selected index and
    /// clamping it to the last match, as Zed does.
    fn refilter(&mut self) {
        self.matches = filter(&self.query, &self.labels);
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
    }

    /// Moves the selection by one, wrapping at either end (Zed's
    /// `menu::SelectNext` and `SelectPrevious`).
    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.matches.len();
        if count == 0 {
            return;
        }
        self.selected = if down {
            (self.selected + 1) % count
        } else {
            self.selected.checked_sub(1).unwrap_or(count - 1)
        };
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Runs the selected row; with nothing matching, closes (as Zed).
    fn confirm(&mut self, cx: &mut Context<Self>) {
        let entry = self
            .matches
            .get(self.selected)
            .and_then(|m| self.entries.get(m.index));
        cx.emit(match entry {
            Some(entry) => PaletteEvent::Confirmed(entry.action.boxed_clone()),
            None => PaletteEvent::Dismissed,
        });
    }

    fn row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some((m, entry)) = self
            .matches
            .get(ix)
            .and_then(|m| Some((m, self.entries.get(m.index)?)))
        else {
            return div().into_any_element();
        };
        ListItem::new(
            SharedString::from(format!("palette-row:{ix}")),
            Indent::Group,
        )
        .height(PALETTE_ROW_HEIGHT)
        .radius(RADIUS_MD)
        .selected(ix == self.selected)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.selected = ix;
            this.confirm(cx);
        }))
        .child(HighlightedLabel::new(entry.label.clone(), m.ranges.clone()))
        .when_some(entry.keys.clone(), |row, keys| {
            row.end(KeyHint::new(keys).color(Color::Placeholder))
        })
        .when_some(entry.note.clone(), |row, (note, color)| {
            row.end(
                div()
                    .debug_selector(move || format!("palette-row:{ix}:note"))
                    .child(Label::new(note).size(LabelSize::Small).color(color)),
            )
        })
        .into_any_element()
    }

    fn list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.matches.is_empty() {
            // Zed's picker shows one muted row and no footer.
            return ListItem::new("palette-empty", Indent::Group)
                .height(PALETTE_ROW_HEIGHT)
                .child(Label::new("No matches").color(Color::TextMuted))
                .into_any_element();
        }
        uniform_list(
            "palette-list",
            self.matches.len(),
            cx.processor(|this, range: Range<usize>, _window, cx| {
                range.map(|ix| this.row(ix, cx)).collect::<Vec<_>>()
            }),
        )
        // Infer: as tall as its rows, up to the max height, then it scrolls.
        .with_sizing_behavior(ListSizingBehavior::Infer)
        .max_h(PALETTE_MAX_HEIGHT)
        .track_scroll(&self.scroll)
        .into_any_element()
    }
}

impl Focusable for CommandPalette {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.global::<Appearance>().theme.border;
        let query = div()
            .h(PALETTE_INPUT_HEIGHT)
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_8)
            .px(SPACE_14)
            .border_b(BORDER_WIDTH)
            .border_color(border)
            // The input shapes text at the inherited size (critic 9, V78).
            .text_size(PALETTE_QUERY_TEXT)
            // Node 11475: Lucide's glyph spans 20 of its 24 units, so at
            // 12 px it draws the node's 10 px ring and handle, inset to
            // keep the node's 14 px box.
            .child(
                div()
                    .debug_selector(|| "command-palette:search".into())
                    .flex_none()
                    .p((ICON_MD - ICON_SM) / 2.)
                    .child(
                        Icon::new(UiIcon::Search)
                            .size(IconSize::Small)
                            .color(Color::Placeholder),
                    ),
            )
            .child(div().flex_1().min_w_0().child(self.input.clone()));
        div()
            .debug_selector(|| "command-palette".into())
            .track_focus(&self.focus)
            .key_context(PALETTE_CONTEXT)
            .w(PALETTE_WIDTH)
            .occlude()
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Dismissed)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(PaletteEvent::Dismissed)))
            .child(
                Card::new()
                    .elevated()
                    .w_full()
                    .p_0()
                    .gap_0()
                    .overflow_hidden()
                    .child(query)
                    .child(div().flex().flex_col().p(SPACE_4).child(self.list(cx))),
            )
    }
}

#[cfg(test)]
#[path = "palette_tests.rs"]
mod tests;
