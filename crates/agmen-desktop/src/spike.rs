//! Spike S8 checks that need a real window (`agmen-desktop --spike <name>`).

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ListAlignment, ListState, ParentElement,
    Render, Styled, Window, div, list, px,
};

use crate::appearance::Appearance;
use crate::ui::TextInput;

/// How many rows the list spike shows.
pub(crate) const ROWS: usize = 10_000;
/// Frames rendered before scrolling starts, so the list has laid out once
/// and measured the rows in view.
const WARMUP_FRAMES: u32 = 3;
/// The list is at its end once its top stops moving for this many frames.
const STALL_FRAMES: u32 = 30;
/// The run stops after this many frames even if it never reaches the end.
const MAX_FRAMES: usize = 40_000;
/// Pixels scrolled per frame while autoscrolling.
const SPEED: f32 = 120.0;

const WORDS: [&str; 12] = [
    "andy/fx1",
    "DONE",
    "C015",
    "fix/c015-main-conflict@9e2a71c",
    "cargo",
    "test",
    "71",
    "passed,",
    "keeps",
    "cylinder,",
    "document",
    "minimap",
];

/// The text of row `ix`: 1 to 60 words, so heights vary as chat rows do.
fn row_text(ix: usize) -> String {
    let words = 1 + (ix * 7919) % 60;
    let mut text = format!("#{ix} ");
    for w in 0..words {
        text.push_str(WORDS[(ix + w) % WORDS.len()]);
        text.push(' ');
    }
    text
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Warmup(u32),
    Down,
    Up,
    Done,
}

/// The list's scroll top: the first visible row and how far into it.
type Top = (usize, f32);

/// Decides each frame's scroll: wait for the first layout, scroll down until
/// the top stops moving, then back up to row 0.
#[derive(Debug, Clone, PartialEq)]
struct Scroller {
    phase: Phase,
    last: Option<Top>,
    stalled: u32,
    frames: usize,
    /// The largest number of rows the top moved in one frame.
    max_jump: usize,
    /// The lowest top row reached on the way down.
    deepest: usize,
}

impl Scroller {
    const fn new() -> Self {
        Self {
            phase: Phase::Warmup(0),
            last: None,
            stalled: 0,
            frames: 0,
            max_jump: 0,
            deepest: 0,
        }
    }

    /// The scroll for this frame given the list's top, or `None` when done.
    fn next(&mut self, top: Top) -> Option<f32> {
        self.frames += 1;
        if self.frames >= MAX_FRAMES {
            self.phase = Phase::Done;
        }
        if let Some(last) = self.last {
            self.max_jump = self.max_jump.max(last.0.abs_diff(top.0));
        }
        let moved = self.last != Some(top);
        self.last = Some(top);
        match self.phase {
            Phase::Warmup(n) if n + 1 >= WARMUP_FRAMES => {
                self.phase = Phase::Down;
                self.max_jump = 0;
                Some(0.0)
            }
            Phase::Warmup(n) => {
                self.phase = Phase::Warmup(n + 1);
                Some(0.0)
            }
            Phase::Down => {
                self.deepest = self.deepest.max(top.0);
                self.stalled = if moved { 0 } else { self.stalled + 1 };
                if self.stalled >= STALL_FRAMES {
                    self.phase = Phase::Up;
                    self.stalled = 0;
                }
                Some(SPEED)
            }
            Phase::Up if top.0 == 0 && top.1 <= 0.0 => {
                self.phase = Phase::Done;
                None
            }
            Phase::Up => Some(-SPEED),
            Phase::Done => None,
        }
    }

    const fn is_scrolling(&self) -> bool {
        matches!(self.phase, Phase::Down | Phase::Up)
    }
}

/// A 10,000-row virtualized list above a text input.
pub(crate) struct ListSpike {
    state: ListState,
    input: Entity<TextInput>,
    scroller: Option<Scroller>,
    last_frame: Option<Instant>,
    intervals: Vec<Duration>,
    /// Frames skipped because the window was not the active one.
    inactive_frames: usize,
    /// The highest row index the list has rendered.
    rendered_max: Rc<Cell<usize>>,
}

impl ListSpike {
    pub(crate) fn new(autoscroll: bool, cx: &mut Context<Self>) -> Self {
        Self {
            state: ListState::new(ROWS, ListAlignment::Top, px(400.)),
            input: cx.new(|cx| TextInput::new(cx).placeholder("Type here, IME too")),
            scroller: autoscroll.then(Scroller::new),
            last_frame: None,
            intervals: Vec::new(),
            inactive_frames: 0,
            rendered_max: Rc::new(Cell::new(0)),
        }
    }

    fn step(&mut self, window: &mut Window, cx: &mut App) {
        let Some(scroller) = self.scroller.as_mut() else {
            return;
        };
        let now = Instant::now();
        if scroller.is_scrolling() {
            // Only frames shown while the window is active say anything about
            // the list: macOS slows a window that is not in front.
            if window.is_window_active() {
                if let Some(last) = self.last_frame {
                    self.intervals.push(now - last);
                }
                self.last_frame = Some(now);
            } else {
                self.inactive_frames += 1;
                self.last_frame = None;
            }
        }
        let top = self.state.logical_scroll_top();
        match scroller.next((top.item_ix, f32::from(top.offset_in_item))) {
            Some(delta) => {
                self.state.scroll_by(px(delta));
                window.request_animation_frame();
            }
            None => {
                println!(
                    "{} inactive_frames={} max_jump_rows={} deepest_top={} last_rendered_row={}",
                    FrameStats::from(self.intervals.as_slice()),
                    self.inactive_frames,
                    scroller.max_jump,
                    scroller.deepest,
                    self.rendered_max.get(),
                );
                cx.quit();
            }
        }
    }
}

impl Render for ListSpike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.step(window, cx);
        let appearance = cx.global::<Appearance>();
        window.set_rem_size(appearance.fonts.ui_size);
        let theme = &appearance.theme;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(appearance.fonts.ui_family.clone())
            .child(
                list(self.state.clone(), {
                    let rendered_max = self.rendered_max.clone();
                    move |ix, _, cx| {
                        rendered_max.set(rendered_max.get().max(ix));
                        let theme = &cx.global::<Appearance>().theme;
                        div()
                            .px_4()
                            .py_1()
                            .border_b_1()
                            .border_color(theme.border_variant)
                            .child(row_text(ix))
                            .into_any_element()
                    }
                })
                .flex_1(),
            )
            .child(
                div()
                    .flex_none()
                    .m_2()
                    .p_2()
                    .border_1()
                    .border_color(theme.border)
                    .rounded_md()
                    .child(self.input.clone()),
            )
    }
}

/// Frame intervals while autoscrolling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FrameStats {
    pub(crate) frames: usize,
    pub(crate) p50_ms: f64,
    pub(crate) p99_ms: f64,
    pub(crate) max_ms: f64,
}

impl From<&[Duration]> for FrameStats {
    fn from(intervals: &[Duration]) -> Self {
        let mut ms: Vec<f64> = intervals.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        ms.sort_by(f64::total_cmp);
        let at = |q: f64| {
            if ms.is_empty() {
                return 0.0;
            }
            // Nearest rank; the index is in range because q is in [0, 1].
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "q * len is a small non-negative index"
            )]
            let ix = ((q * ms.len() as f64).ceil() as usize)
                .saturating_sub(1)
                .min(ms.len() - 1);
            ms.get(ix).copied().unwrap_or(0.0)
        };
        Self {
            frames: ms.len(),
            p50_ms: at(0.50),
            p99_ms: at(0.99),
            max_ms: ms.last().copied().unwrap_or(0.0),
        }
    }
}

impl std::fmt::Display for FrameStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "frames={} p50_ms={:.2} p99_ms={:.2} max_ms={:.2}",
            self.frames, self.p50_ms, self.p99_ms, self.max_ms
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_vary_in_length() {
        let lens: std::collections::BTreeSet<usize> =
            (0..100).map(|ix| row_text(ix).split(' ').count()).collect();
        assert!(lens.len() > 20, "{lens:?}");
    }

    /// A fake list of `ROWS` rows of 30 px with a 600 px viewport, scrolled
    /// the way GPUI's `scroll_by` moves it.
    fn run_fake() -> Scroller {
        let (row, max) = (30.0_f32, 30.0 * ROWS as f32 - 600.0);
        let mut offset = 0.0_f32;
        let mut s = Scroller::new();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "offset is in [0, max]"
        )]
        let top = |o: f32| ((o / row) as usize, o % row);
        while let Some(delta) = s.next(top(offset)) {
            offset = (offset + delta).clamp(0.0, max);
        }
        s
    }

    #[test]
    fn scroller_reaches_the_end_and_comes_back() {
        let s = run_fake();
        assert_eq!(s.phase, Phase::Done);
        assert!(s.frames < MAX_FRAMES, "stopped at the cap");
        assert_eq!(
            s.deepest,
            ROWS - 20,
            "the top row when the last row is in view"
        );
        assert!(s.max_jump <= 5, "jumped {} rows in one frame", s.max_jump);
    }

    #[test]
    fn scroller_waits_for_first_layout() {
        let mut s = Scroller::new();
        for _ in 0..WARMUP_FRAMES {
            assert_eq!(s.next((0, 0.0)), Some(0.0));
        }
        assert_eq!(s.next((0, 0.0)), Some(SPEED));
    }

    #[test]
    fn frame_stats_percentiles() {
        let intervals: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        let s = FrameStats::from(intervals.as_slice());
        assert_eq!(s.frames, 100);
        assert!((s.p50_ms - 50.0).abs() < 1e-9);
        assert!((s.p99_ms - 99.0).abs() < 1e-9);
        assert!((s.max_ms - 100.0).abs() < 1e-9);
        assert_eq!(FrameStats::from(&[][..]).frames, 0);
    }
}
