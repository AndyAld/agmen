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

use super::tokens::{LINE_HEIGHT, SPACE_3};
use crate::appearance::Appearance;

#[path = "inline_flow_breaker.rs"]
mod breaker;
use breaker::{Flow, Glyphs, Line, Placed, Seg, advances, flow};

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
    line_gap: Pixels,
    chip_line: Option<Pixels>,
}

impl InlineFlow {
    /// Lines `SPACE_3` apart, as a message body's (`rich_body`).
    #[must_use]
    pub(super) const fn new(items: Vec<FlowItem>) -> Self {
        Self {
            items,
            line_gap: SPACE_3,
            chip_line: None,
        }
    }

    /// The space between lines, as none in a dialog's prose (node 2440).
    #[must_use]
    pub(super) const fn line_gap(mut self, gap: Pixels) -> Self {
        self.line_gap = gap;
        self
    }

    /// The line height of the chips' labels, when they set one (as
    /// `CodeText`'s chips take the prose line). A chip wider than the
    /// column counts its wrapped rows at this height, not the code font's.
    #[must_use]
    pub(super) const fn chip_line_height(mut self, height: Pixels) -> Self {
        self.chip_line = Some(height);
        self
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
        // prepaint still wraps it. In a column narrower than the padding
        // and glue the label still gets a positive width, so the estimate
        // never depends on how a wrap at zero or less behaves.
        let rows: usize = text_system
            .shape_text(
                label.clone(),
                self.size,
                &[self.run(label)],
                Some((width - overhead).max(px(1.))),
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
    line_gap: Pixels,
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
        let laid = Rc::new(flow(
            segs,
            width,
            self.line_height,
            self.line_gap,
            |seg, width| match (segs.get(seg), parts.get(seg)) {
                (Some(Seg::Atom(natural)), Some(Part::Atom { label: text, .. })) => {
                    label.wrapped_height(text, *natural, width, window)
                }
                _ => px(0.),
            },
        ));
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
            line_height: self
                .chip_line
                .unwrap_or_else(|| window.pixel_snap(fonts.code_size * LINE_HEIGHT)),
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
            line_gap: self.line_gap,
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
                glyphs: piece
                    .shaped
                    .runs
                    .iter()
                    .flat_map(|run| run.glyphs.iter())
                    .map(|glyph| glyph.index + piece.placed.bytes.start)
                    .collect(),
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
                glyphs: Vec::new(),
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
    /// The run byte each of a piece's glyphs draws, in drawing order.
    pub(super) glyphs: Vec<usize>,
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
