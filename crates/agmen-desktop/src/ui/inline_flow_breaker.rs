//! The breaker behind [`InlineFlow`](super::InlineFlow): pure layout over
//! plain data. It measures shaped runs by their glyph advances in byte
//! order and breaks them into lines around atoms, so its edge cases are
//! tested without a window.

use std::ops::Range;

use gpui::{Bounds, Pixels, ShapedGlyph, ShapedLine, SharedString, Size, point, px, size};

use crate::ui::tokens::SPACE_4;

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
pub(super) fn advances(
    line: &ShapedLine,
) -> impl Iterator<Item = (usize, usize, &ShapedGlyph, Pixels)> {
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
/// least `line_height` tall, `line_gap` apart, with `SPACE_4` between items.
/// An atom wider than the line takes the line's width and the height
/// `wrapped_height(seg, width)` gives it.
pub(super) fn flow(
    segs: &[Seg],
    width: Option<Pixels>,
    line_height: Pixels,
    line_gap: Pixels,
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
    breaker.finish(line_gap)
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

    fn finish(mut self, line_gap: Pixels) -> Flow {
        let mut top = px(0.);
        for line in &mut self.lines {
            line.top = top;
            top = top + line.height + line_gap;
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
