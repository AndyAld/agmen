//! One line of a message body that mixes words with reference chips, laid
//! out as inline text (`rich_body` in `docs/mockups/scripts/console.py`):
//! the words after a chip fill the rest of its line and wrap word by word.
//!
//! Each run of words is shaped once as a single line. [`flow`] breaks it
//! with the shaped glyph positions, so a break never lets a line pass the
//! column, and the run's first line starts where the item before it ended.
//! Each chip is an atom: its own element tree, laid out as a root at its
//! natural size and placed where the flow puts it, so its click, ⌘-click
//! and hover card work as they do anywhere else. A chip never splits; one
//! wider than the column takes a line of its own and wraps its label.
//!
//! gpui's `InteractiveText` cannot draw these chips: its click listener
//! gets no event, so it cannot tell a ⌘-click, its tooltip is not
//! hoverable, and it notifies its view on every character the pointer
//! crosses. Text highlights also have no padding or rounded corners.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, LineLayout, Pixels, Point, ShapedGlyph, ShapedLine,
    ShapedLineCursor, ShapedRun, SharedString, Size, Style, TextAlign, TextRun, Window, point, px,
    relative, size,
};

use super::tokens::{LINE_HEIGHT, SPACE_3, SPACE_4};
use crate::appearance::Appearance;

/// The widths whose layouts a flow keeps: taffy measures a few times per
/// frame, and prepaint reads the last one.
const CACHED_WIDTHS: usize = 4;

/// One item of a line: a run of words, or a chip.
pub(super) enum FlowItem {
    /// Words joined by single spaces.
    Run(SharedString),
    /// A chip's element tree, and its label as drawn by `Label::code`, from
    /// which a chip wider than the column estimates its wrapped height.
    Chip {
        element: AnyElement,
        label: SharedString,
    },
}

/// A line of words and chips that flows as inline text.
pub(super) struct InlineFlow {
    items: Vec<FlowItem>,
}

impl InlineFlow {
    #[must_use]
    pub(super) const fn new(items: Vec<FlowItem>) -> Self {
        Self { items }
    }
}

/// A shaped run as the breaker sees it, in reading order: where each glyph
/// cluster starts, and how far its glyphs advance the pen.
///
/// The breaker works on bytes, but a shaper draws right to left text last
/// letter first, so a glyph's x says nothing about the bytes around it.
/// Each stop's x is instead the advance of every glyph before it in byte
/// order, which makes the width of any byte range the width its glyphs
/// draw, whatever order they are drawn in.
#[derive(Debug, Clone, Default)]
pub(super) struct Glyphs {
    pub(super) text: SharedString,
    /// The byte index and x of each stop, in strictly rising byte order,
    /// each on a character boundary and the first at byte 0. A stop that
    /// starts with whitespace holds only whitespace.
    stops: Vec<(usize, Pixels)>,
    width: Pixels,
}

impl From<&ShapedLine> for Glyphs {
    fn from(line: &ShapedLine) -> Self {
        let text = line.text.clone();
        let width = line.width();
        let mut stops: Vec<(usize, Pixels)> = Vec::with_capacity(text.len() + 1);
        if !text.is_empty() {
            stops.push((0, px(0.)));
        }
        let ordered = line
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.index))
            .is_sorted();
        // A glyph inside a character counts for it.
        if ordered {
            // Glyphs in byte order sit where the advance before them ends.
            for glyph in line.runs.iter().flat_map(|run| run.glyphs.iter()) {
                push_stop(
                    &text,
                    &mut stops,
                    text.floor_char_boundary(glyph.index),
                    glyph.position.x,
                );
            }
        } else {
            let mut clusters: Vec<(usize, Pixels)> = advances(line)
                .map(|(_, _, glyph, advance)| (text.floor_char_boundary(glyph.index), advance))
                .collect();
            // Stable, so the glyphs of one cluster keep their order.
            clusters.sort_by_key(|(at, _)| *at);
            let mut x = px(0.);
            for (at, advance) in clusters {
                push_stop(&text, &mut stops, at, x);
                x += advance;
            }
        }
        push_stop(&text, &mut stops, text.len(), width);
        Self { text, stops, width }
    }
}

/// Starts a stop at byte `at` and x `x`, unless one starts there already;
/// the text's end starts none. Before it, the stop that ends at `at` is
/// split if it starts with whitespace and goes on to other characters, so
/// a break can trim the whitespace and keep the rest: CoreText draws no
/// glyph for a zero width space, so the space before one carries its
/// bytes. The whitespace keeps the advance, and the rest is drawn by no
/// glyph.
fn push_stop(text: &str, stops: &mut Vec<(usize, Pixels)>, at: usize, x: Pixels) {
    let Some(&(last, _)) = stops.last() else {
        return;
    };
    if last == at {
        return;
    }
    let cluster = text.get(last..at).unwrap_or_default();
    if cluster.starts_with(char::is_whitespace)
        && let Some(rest) = cluster.find(|c: char| !c.is_whitespace())
    {
        stops.push((last + rest, x));
    }
    if at < text.len() {
        stops.push((at, x));
    }
}

/// Each glyph of `line` in the order it draws them, with its run, its index
/// in the run, and its advance: the distance to the next glyph drawn, the
/// first from 0 and the last to the line's end, so they sum to its width.
fn advances(line: &ShapedLine) -> impl Iterator<Item = (usize, usize, &ShapedGlyph, Pixels)> {
    let mut visual = line
        .runs
        .iter()
        .enumerate()
        .flat_map(|(r, run)| {
            run.glyphs
                .iter()
                .enumerate()
                .map(move |(g, glyph)| (r, g, glyph))
        })
        .peekable();
    let mut left = px(0.);
    std::iter::from_fn(move || {
        let (r, g, glyph) = visual.next()?;
        let right = visual
            .peek()
            .map_or(line.width(), |(_, _, next)| next.position.x);
        let advance = right - left;
        left = right;
        Some((r, g, glyph, advance))
    })
}

impl Glyphs {
    /// `text` with every character `advance` wide, for tests of the
    /// breaker without a text system.
    #[cfg(test)]
    pub(super) fn monospace(text: &str, advance: Pixels) -> Self {
        let stops: Vec<(usize, Pixels)> = text
            .char_indices()
            .enumerate()
            .map(|(n, (at, _))| (at, advance * n as f32))
            .collect();
        Self {
            text: text.to_owned().into(),
            width: advance * stops.len() as f32,
            stops,
        }
    }

    /// The byte where glyph `k` starts; the text's end past the last.
    fn byte(&self, k: usize) -> usize {
        self.stops.get(k).map_or(self.text.len(), |(at, _)| *at)
    }

    /// The x where glyph `k` starts; the run's width past the last.
    fn x(&self, k: usize) -> Pixels {
        self.stops.get(k).map_or(self.width, |(_, x)| *x)
    }

    /// The characters of stop `k`.
    fn cluster(&self, k: usize) -> &str {
        self.text
            .get(self.byte(k)..self.byte(k + 1))
            .unwrap_or_default()
    }

    /// The first character of stop `k`.
    fn first(&self, k: usize) -> char {
        self.cluster(k).chars().next().unwrap_or(' ')
    }

    /// Whether stop `k` is whitespace, and so nothing else, so it may hang
    /// past a line's end and be trimmed there.
    fn blank(&self, k: usize) -> bool {
        self.first(k).is_whitespace()
    }
}

/// What [`flow`] lays out.
#[derive(Debug, Clone)]
pub(super) enum Seg {
    Run(Glyphs),
    /// A chip at its natural size.
    Atom(Size<Pixels>),
}

/// One visual line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Line {
    pub(super) top: Pixels,
    pub(super) height: Pixels,
}

/// A piece of a run, or an atom, at its place in the flow.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Placed {
    /// The index of its [`Seg`].
    pub(super) seg: usize,
    /// The run's bytes this piece draws; empty for an atom.
    pub(super) bytes: Range<usize>,
    pub(super) line: usize,
    /// Relative to the flow's origin.
    pub(super) bounds: Bounds<Pixels>,
}

/// A laid out flow.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Flow {
    /// In the order of the segments, and of the bytes within a run.
    pub(super) placed: Vec<Placed>,
    pub(super) lines: Vec<Line>,
    /// The widest line, and the height of all of them.
    pub(super) size: Size<Pixels>,
}

/// Lays out `segs` in lines `width` wide (one line with no width), each at
/// least `line_height` tall, `SPACE_3` apart, with `SPACE_4` between items.
/// An atom wider than the line takes the line's width and the height
/// `wrapped_height(seg, width)` gives it.
pub(super) fn flow(
    segs: &[Seg],
    width: Option<Pixels>,
    line_height: Pixels,
    mut wrapped_height: impl FnMut(usize, Pixels) -> Pixels,
) -> Flow {
    let mut breaker = Breaker::new(width.unwrap_or(Pixels::MAX), line_height);
    for (seg, item) in segs.iter().enumerate() {
        match item {
            Seg::Run(glyphs) => breaker.run(seg, glyphs),
            Seg::Atom(natural) if natural.width > breaker.limit => {
                let height = wrapped_height(seg, breaker.limit);
                breaker.atom(seg, size(breaker.limit, height));
            }
            Seg::Atom(natural) => breaker.atom(seg, *natural),
        }
    }
    breaker.finish()
}

/// Whether a line may break before `c` when `prev` comes before it: at the
/// start of a word, after a zero width space, or beside an ideograph, but
/// never before closing or after opening punctuation.
fn breaks_before(prev: Option<char>, c: char) -> bool {
    let Some(prev) = prev else {
        return false;
    };
    if c.is_whitespace() || closes(c) || opens(prev) {
        return false;
    }
    prev.is_whitespace() || prev == '\u{200B}' || wide(c) || wide(prev)
}

/// Scripts written without spaces between words: CJK, kana, Hangul, Yi and
/// full width forms.
fn wide(c: char) -> bool {
    matches!(c,
        '\u{2E80}'..='\u{A4CF}'
        | '\u{AC00}'..='\u{D7A3}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FF00}'..='\u{FFEF}'
        | '\u{20000}'..='\u{3FFFD}')
}

fn closes(c: char) -> bool {
    matches!(
        c,
        '、' | '。'
            | '，'
            | '．'
            | '：'
            | '；'
            | '！'
            | '？'
            | '）'
            | '］'
            | '｝'
            | '」'
            | '』'
            | '】'
            | '〕'
            | '〉'
            | '》'
            | 'ー'
    )
}

fn opens(c: char) -> bool {
    matches!(
        c,
        '（' | '［' | '｛' | '「' | '『' | '【' | '〔' | '〈' | '《'
    )
}

struct Breaker {
    limit: Pixels,
    line_height: Pixels,
    placed: Vec<Placed>,
    /// Never empty; the last is the line being filled.
    lines: Vec<Line>,
    /// Where the last item on the current line ends.
    pen: Pixels,
    /// Whether the current line holds anything yet.
    filled: bool,
    widest: Pixels,
}

impl Breaker {
    fn new(limit: Pixels, line_height: Pixels) -> Self {
        Self {
            limit,
            line_height,
            placed: Vec::new(),
            lines: vec![Line {
                top: px(0.),
                height: line_height,
            }],
            pen: px(0.),
            filled: false,
            widest: px(0.),
        }
    }

    fn new_line(&mut self) {
        self.lines.push(Line {
            top: px(0.),
            height: self.line_height,
        });
        self.pen = px(0.);
        self.filled = false;
    }

    /// Where the next item on this line starts.
    fn next_x(&self) -> Pixels {
        if self.filled {
            self.pen + SPACE_4
        } else {
            self.pen
        }
    }

    fn place(&mut self, seg: usize, bytes: Range<usize>, x: Pixels, item: Size<Pixels>) {
        let line = self.lines.len().saturating_sub(1);
        if let Some(current) = self.lines.last_mut() {
            current.height = current.height.max(item.height);
        }
        self.placed.push(Placed {
            seg,
            bytes,
            line,
            bounds: Bounds::new(point(x, px(0.)), item),
        });
        self.pen = x + item.width;
        self.filled = true;
        self.widest = self.widest.max(self.pen);
    }

    fn atom(&mut self, seg: usize, item: Size<Pixels>) {
        if self.filled && self.next_x() + item.width > self.limit {
            self.new_line();
        }
        self.place(seg, 0..0, self.next_x(), item);
    }

    /// Places stops `start..end` of a run at `x`, less trailing spaces,
    /// which hang past the line's end.
    fn text(&mut self, seg: usize, glyphs: &Glyphs, start: usize, end: usize, x: Pixels) {
        let mut end = end;
        while end > start && glyphs.blank(end - 1) {
            end -= 1;
        }
        if end > start {
            let width = glyphs.x(end) - glyphs.x(start);
            let item = size(width, self.line_height);
            self.place(seg, glyphs.byte(start)..glyphs.byte(end), x, item);
        }
    }

    /// Breaks a run into lines. Each pass of the loop either moves `k` on,
    /// or starts a new line at a later glyph, or (once per run) moves the
    /// whole run to an empty line, so it ends after at most a few passes
    /// per glyph.
    fn run(&mut self, seg: usize, glyphs: &Glyphs) {
        let count = glyphs.stops.len();
        // The current line's piece starts at glyph `start`, drawn at `x`.
        let mut start = 0;
        let mut x = self.next_x();
        let mut candidate = None;
        let mut prev = None;
        let mut k = 0;
        while k < count {
            let cluster = glyphs.cluster(k);
            let first = cluster.chars().next().unwrap_or(' ');
            let last = cluster.chars().next_back().unwrap_or(' ');
            if k > start && breaks_before(prev, first) {
                candidate = Some(k);
            }
            let end = x + glyphs.x(k + 1) - glyphs.x(start);
            // A stop that starts with whitespace holds nothing else.
            if !first.is_whitespace() && end > self.limit {
                let at = match candidate.take() {
                    Some(at) => at,
                    // The first word does not fit beside what is already
                    // on the line: the run starts the next one.
                    None if start == 0 && self.filled => {
                        self.new_line();
                        x = px(0.);
                        prev = None;
                        k = 0;
                        continue;
                    }
                    // A word wider than the line breaks where it overflows.
                    None if k > start => k,
                    // A glyph wider than the line stays alone on it.
                    None => {
                        prev = Some(last);
                        k += 1;
                        continue;
                    }
                };
                self.text(seg, glyphs, start, at, x);
                self.new_line();
                start = at;
                x = px(0.);
                prev = None;
                k = at;
                continue;
            }
            prev = Some(last);
            k += 1;
        }
        self.text(seg, glyphs, start, count, x);
    }

    fn finish(mut self) -> Flow {
        let mut top = px(0.);
        for line in &mut self.lines {
            line.top = top;
            top = top + line.height + SPACE_3;
        }
        for placed in &mut self.placed {
            if let Some(line) = self.lines.get(placed.line) {
                placed.bounds.origin.y = line.top + (line.height - placed.bounds.size.height) / 2.;
            }
        }
        let height = self
            .lines
            .last()
            .map_or(px(0.), |line| line.top + line.height);
        Flow {
            placed: self.placed,
            lines: self.lines,
            size: size(self.widest, height),
        }
    }
}

/// The code font a chip label is drawn in, as `Label::code` sets it.
struct LabelFont {
    run: TextRun,
    size: Pixels,
    line_height: Pixels,
}

impl LabelFont {
    fn run(&self, label: &str) -> TextRun {
        TextRun {
            len: label.len(),
            ..self.run.clone()
        }
    }

    /// How tall an atom `natural` in size, with label `label`, is when
    /// laid out `width` wide: its padding and glue keep their width, and
    /// the label wraps in the rest.
    fn wrapped_height(
        &self,
        label: &SharedString,
        natural: Size<Pixels>,
        width: Pixels,
        window: &Window,
    ) -> Pixels {
        let text_system = window.text_system();
        // Ceiled as gpui's Text measures its width.
        let unwrapped = text_system
            .shape_line(label.clone(), self.size, &[self.run(label)], None)
            .width()
            .ceil();
        let overhead = natural.width - unwrapped;
        // A label that fails to shape counts as one row; its own layout in
        // prepaint still wraps it.
        let rows: usize = text_system
            .shape_text(
                label.clone(),
                self.size,
                &[self.run(label)],
                Some(width - overhead),
                None,
            )
            .map_or(1, |lines| {
                lines
                    .iter()
                    .map(|line| line.wrap_boundaries.len() + 1)
                    .sum()
            });
        #[expect(clippy::cast_precision_loss, reason = "rows is a small count")]
        let rows = rows as f32;
        natural.height.max(self.line_height * rows)
    }
}

/// What a segment draws with: a shaped run, or an atom's element.
enum Part {
    /// Boxed: a shaped line keeps its decorations inline, about 3 KB.
    Run(Box<ShapedLine>),
    Atom {
        child: usize,
        label: SharedString,
    },
}

/// The shaped runs and measured atoms, shared with the measure closure,
/// which can lay out only plain data: GPUI takes the layout engine out of
/// the window while it runs.
struct FlowState {
    segs: Vec<Seg>,
    parts: Vec<Part>,
    line_height: Pixels,
    label: LabelFont,
    cached: Vec<(Option<Pixels>, Rc<Flow>)>,
}

impl FlowState {
    /// The flow at `width`, laid out once per width.
    fn flow(&mut self, width: Option<Pixels>, window: &Window) -> Rc<Flow> {
        if let Some((_, laid)) = self.cached.iter().find(|(w, _)| *w == width) {
            return laid.clone();
        }
        let (segs, parts, label) = (&self.segs, &self.parts, &self.label);
        let laid = Rc::new(flow(segs, width, self.line_height, |seg, width| {
            match (segs.get(seg), parts.get(seg)) {
                (Some(Seg::Atom(natural)), Some(Part::Atom { label: text, .. })) => {
                    label.wrapped_height(text, *natural, width, window)
                }
                _ => px(0.),
            }
        }));
        if self.cached.len() >= CACHED_WIDTHS {
            self.cached.remove(0);
        }
        self.cached.push((width, laid.clone()));
        laid
    }
}

pub(super) struct FlowLayout {
    atoms: Vec<AnyElement>,
    state: Rc<RefCell<FlowState>>,
}

pub(super) struct FlowPaint {
    pieces: Vec<Piece>,
    line_height: Pixels,
}

/// A piece of a run, cut from its shaped line, at its absolute place.
struct Piece {
    origin: Point<Pixels>,
    shaped: ShapedLine,
    #[cfg_attr(not(test), expect(dead_code, reason = "tests record what was drawn"))]
    placed: Placed,
}

impl IntoElement for InlineFlow {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InlineFlow {
    type RequestLayoutState = FlowLayout;
    type PrepaintState = FlowPaint;

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
        // The body's style, which the enclosing div has in effect here.
        let style = window.text_style();
        let rem = window.rem_size();
        let font_size = style.font_size.to_pixels(rem);
        // Snapped as gpui's Text snaps it, so a line here is as tall as a
        // line of the body's plain text.
        let line_height = window.pixel_snap(style.line_height.to_pixels(font_size.into(), rem));
        let fonts = &cx.global::<Appearance>().fonts;
        let mut code = style.clone();
        code.font_family = fonts.code_family.clone();
        let label = LabelFont {
            run: code.to_run(0),
            size: fonts.code_size,
            line_height: window.pixel_snap(fonts.code_size * LINE_HEIGHT),
        };
        let mut atoms = Vec::new();
        let mut segs = Vec::with_capacity(self.items.len());
        let mut parts = Vec::with_capacity(self.items.len());
        for item in std::mem::take(&mut self.items) {
            match item {
                FlowItem::Run(text) => {
                    let run = style.to_run(text.len());
                    let line = window
                        .text_system()
                        .shape_line(text, font_size, &[run], None);
                    segs.push(Seg::Run(Glyphs::from(&line)));
                    parts.push(Part::Run(Box::new(line)));
                }
                FlowItem::Chip {
                    mut element,
                    label: text,
                } => {
                    let natural = element.layout_as_root(
                        size(AvailableSpace::MaxContent, AvailableSpace::MinContent),
                        window,
                        cx,
                    );
                    segs.push(Seg::Atom(natural));
                    parts.push(Part::Atom {
                        child: atoms.len(),
                        label: text,
                    });
                    atoms.push(element);
                }
            }
        }
        let state = Rc::new(RefCell::new(FlowState {
            segs,
            parts,
            line_height,
            label,
            cached: Vec::new(),
        }));
        let measured = state.clone();
        let measure = move |known: Size<Option<Pixels>>,
                            available: Size<AvailableSpace>,
                            window: &mut Window,
                            _: &mut App| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                AvailableSpace::MinContent | AvailableSpace::MaxContent => None,
            });
            let laid = measured.borrow_mut().flow(width, window);
            size(width.unwrap_or(laid.size.width), laid.size.height)
        };
        let mut layout_style = Style::default();
        layout_style.size.width = relative(1.).into();
        let id = window.request_measured_layout(layout_style, measure);
        (id, FlowLayout { atoms, state })
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let laid = layout
            .state
            .borrow_mut()
            .flow(Some(bounds.size.width), window);
        let (pieces, atoms, line_height) = slice(&layout.state.borrow(), &laid, bounds.origin);
        #[cfg(test)]
        let mut drawn: Vec<Drawn> = pieces
            .iter()
            .map(|piece| Drawn {
                line: piece.placed.line,
                bounds: Bounds::new(piece.origin, piece.placed.bounds.size),
                text: piece.shaped.text.clone(),
                chip: false,
            })
            .collect();
        for atom in atoms {
            let Some(element) = layout.atoms.get_mut(atom.child) else {
                continue;
            };
            let mut origin = bounds.origin + atom.placed.bounds.origin;
            if atom.over_wide {
                // Laid out again at the line's width, so its label wraps.
                let width = atom.placed.bounds.size.width;
                let real = element.layout_as_root(
                    size(AvailableSpace::Definite(width), AvailableSpace::MinContent),
                    window,
                    cx,
                );
                #[cfg(test)]
                record_estimate(atom.placed.bounds.size.height, real.height);
                origin.y = bounds.origin.y + atom.line.top + (atom.line.height - real.height) / 2.;
            }
            element.prepaint_at(origin, window, cx);
            #[cfg(test)]
            drawn.push(Drawn {
                line: atom.placed.line,
                bounds: Bounds::new(origin, atom.placed.bounds.size),
                text: atom.label,
                chip: true,
            });
        }
        #[cfg(test)]
        record_drawn(drawn);
        FlowPaint {
            pieces,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut failed = false;
        for piece in &prepaint.pieces {
            failed |= piece
                .shaped
                .paint(
                    piece.origin,
                    prepaint.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .is_err();
        }
        for atom in &mut layout.atoms {
            atom.paint(window, cx);
        }
        // Logged once: a failing paint repeats every frame.
        static LOGGED: AtomicBool = AtomicBool::new(false);
        if failed && !LOGGED.swap(true, Ordering::Relaxed) {
            eprintln!("agmen-desktop: a line of message text failed to paint");
        }
    }
}

/// An atom to place in prepaint.
struct AtomAt {
    child: usize,
    placed: Placed,
    line: Line,
    over_wide: bool,
    #[cfg_attr(not(test), expect(dead_code, reason = "tests record what was drawn"))]
    label: SharedString,
}

/// Cuts each run into its placed pieces, at absolute positions from
/// `origin`, and lists the atoms to place.
fn slice(
    state: &FlowState,
    laid: &Flow,
    origin: Point<Pixels>,
) -> (Vec<Piece>, Vec<AtomAt>, Pixels) {
    let mut pieces = Vec::new();
    let mut atoms = Vec::new();
    let mut cutter = None;
    for placed in &laid.placed {
        match (state.parts.get(placed.seg), state.segs.get(placed.seg)) {
            (Some(Part::Run(line)), _) => {
                let (seg, cut) = cutter.get_or_insert_with(|| (placed.seg, Cutter::new(line)));
                if *seg != placed.seg {
                    *seg = placed.seg;
                    *cut = Cutter::new(line);
                }
                pieces.push(Piece {
                    origin: origin + placed.bounds.origin,
                    shaped: cut.cut(placed.bytes.clone()),
                    placed: placed.clone(),
                });
            }
            (Some(Part::Atom { child, label, .. }), Some(Seg::Atom(natural))) => {
                atoms.push(AtomAt {
                    child: *child,
                    placed: placed.clone(),
                    line: laid.lines.get(placed.line).copied().unwrap_or(Line {
                        top: placed.bounds.origin.y,
                        height: placed.bounds.size.height,
                    }),
                    over_wide: natural.width > placed.bounds.size.width,
                    label: label.clone(),
                })
            }
            _ => {}
        }
    }
    (pieces, atoms, state.line_height)
}

/// Cuts a run's shaped line into the pieces a flow placed, in byte order.
#[expect(
    clippy::large_enum_variant,
    reason = "one lives on the stack at a time, while a run is cut"
)]
enum Cutter<'a> {
    /// gpui's cursor, which copies each glyph once.
    Ordered(ShapedLineCursor<'a>),
    /// Right to left text: the cursor splits at the first glyph past a
    /// byte in the order they are drawn, which cuts a reversed stretch in
    /// the wrong place, so each piece collects its own glyphs.
    Unordered(Reordered<'a>),
}

impl<'a> Cutter<'a> {
    fn new(line: &'a ShapedLine) -> Self {
        let ordered = line
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.index))
            .is_sorted();
        if ordered {
            Self::Ordered(line.cursor())
        } else {
            Self::Unordered(Reordered::new(line))
        }
    }

    /// The piece that draws `bytes`, which start at or after the end of the
    /// last piece cut, on character boundaries.
    fn cut(&mut self, bytes: Range<usize>) -> ShapedLine {
        match self {
            Self::Ordered(cursor) => {
                // The bytes before the piece are the space it broke at.
                cursor.take_until(bytes.start);
                cursor.take_until(bytes.end)
            }
            Self::Unordered(line) => line.cut(bytes),
        }
    }
}

/// A shaped line whose glyphs are not in byte order, indexed so a piece
/// finds its glyphs without a pass over the whole line.
struct Reordered<'a> {
    line: &'a ShapedLine,
    /// Each glyph's run, its index in the run, and its advance, in the
    /// order the line draws them.
    drawn: Vec<(usize, usize, Pixels)>,
    /// Each glyph's byte and its place in `drawn`, in byte order.
    by_byte: Vec<(usize, usize)>,
}

impl<'a> Reordered<'a> {
    fn new(line: &'a ShapedLine) -> Self {
        let mut drawn = Vec::new();
        let mut by_byte = Vec::new();
        // Measured as `Glyphs` measures them, so a piece is as wide as the
        // flow placed it.
        for (r, g, glyph, advance) in advances(line) {
            by_byte.push((glyph.index, drawn.len()));
            drawn.push((r, g, advance));
        }
        by_byte.sort_by_key(|(at, _)| *at);
        Self {
            line,
            drawn,
            by_byte,
        }
    }

    /// The glyphs of `bytes` in the order the line draws them, packed from
    /// x 0. Bidi reordering of a line cut from a paragraph keeps the order
    /// the paragraph draws the line's characters in, so the piece reads as
    /// a line of its own would, with the glyph shapes of the whole run.
    fn cut(&self, bytes: Range<usize>) -> ShapedLine {
        let from = self.by_byte.partition_point(|(at, _)| *at < bytes.start);
        let to = self.by_byte.partition_point(|(at, _)| *at < bytes.end);
        let mut order: Vec<usize> = self
            .by_byte
            .get(from..to)
            .unwrap_or_default()
            .iter()
            .map(|(_, n)| *n)
            .collect();
        order.sort_unstable();
        let mut runs: Vec<(usize, ShapedRun)> = Vec::new();
        let mut x = px(0.);
        for n in order {
            let Some(&(r, g, advance)) = self.drawn.get(n) else {
                continue;
            };
            let Some(run) = self.line.runs.get(r) else {
                continue;
            };
            let Some(glyph) = run.glyphs.get(g) else {
                continue;
            };
            let moved = ShapedGlyph {
                position: point(x, glyph.position.y),
                index: glyph.index.saturating_sub(bytes.start),
                ..glyph.clone()
            };
            match runs.last_mut() {
                Some((last, shaped)) if *last == r => shaped.glyphs.push(moved),
                _ => runs.push((
                    r,
                    ShapedRun {
                        font_id: run.font_id,
                        glyphs: vec![moved],
                    },
                )),
            }
            x += advance;
        }
        // A clone keeps the line's decoration, one run over all its bytes
        // (`request_layout` shapes each run with a single `TextRun`), which
        // covers the piece's rebased bytes too.
        let mut piece = self.line.clone();
        *piece = Arc::new(LineLayout {
            font_size: self.line.font_size,
            width: x,
            ascent: self.line.ascent,
            descent: self.line.descent,
            runs: runs.into_iter().map(|(_, run)| run).collect(),
            len: bytes.len(),
        });
        piece.text = self
            .line
            .text
            .get(bytes)
            .map(SharedString::new)
            .unwrap_or_default();
        piece
    }
}

/// What one flow drew, for tests: a piece of text or a chip with its label.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(super) struct Drawn {
    pub(super) line: usize,
    pub(super) bounds: Bounds<Pixels>,
    pub(super) text: SharedString,
    pub(super) chip: bool,
}

#[cfg(test)]
const RECORDED: usize = 64;

#[cfg(test)]
thread_local! {
    static DRAWN: RefCell<Vec<Vec<Drawn>>> = const { RefCell::new(Vec::new()) };
    static ESTIMATES: RefCell<Vec<(Pixels, Pixels)>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn record_drawn(drawn: Vec<Drawn>) {
    DRAWN.with_borrow_mut(|all| {
        if all.len() >= RECORDED {
            all.remove(0);
        }
        all.push(drawn);
    });
}

#[cfg(test)]
fn record_estimate(estimated: Pixels, laid_out: Pixels) {
    ESTIMATES.with_borrow_mut(|all| {
        if all.len() < RECORDED * 16 {
            all.push((estimated, laid_out));
        }
    });
}

/// What each flow drew since the last call, oldest first.
#[cfg(test)]
pub(super) fn take_drawn() -> Vec<Vec<Drawn>> {
    DRAWN.with_borrow_mut(std::mem::take)
}

/// For each chip wider than its line since the last call: the height the
/// flow estimated, and the height its own layout gave it.
#[cfg(test)]
pub(super) fn take_estimates() -> Vec<(Pixels, Pixels)> {
    ESTIMATES.with_borrow_mut(std::mem::take)
}

#[cfg(test)]
#[path = "inline_flow_tests.rs"]
mod tests;
