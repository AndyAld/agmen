//! Spike S8 checks that need a real window (`agmen-desktop --spike <name>`).

use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ListAlignment, ListState, ParentElement,
    Render, Styled, Window, div, list, px,
};

use crate::appearance::Appearance;
use crate::ui::TextInput;

/// How many rows the list spike shows.
pub(crate) const ROWS: usize = 10_000;
/// Frames scrolled down, then the same number up.
const FRAMES_EACH_WAY: usize = 1_500;
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
    Down,
    Up,
    Done,
}

/// A 10,000-row virtualized list above a text input.
pub(crate) struct ListSpike {
    state: ListState,
    input: Entity<TextInput>,
    autoscroll: Option<Phase>,
    last_frame: Option<Instant>,
    intervals: Vec<Duration>,
    deepest: usize,
}

impl ListSpike {
    pub(crate) fn new(autoscroll: bool, cx: &mut Context<Self>) -> Self {
        Self {
            state: ListState::new(ROWS, ListAlignment::Top, px(400.)),
            input: cx.new(|cx| TextInput::new(cx).placeholder("Type here, IME too")),
            autoscroll: autoscroll.then_some(Phase::Down),
            last_frame: None,
            intervals: Vec::with_capacity(2 * FRAMES_EACH_WAY),
            deepest: 0,
        }
    }

    fn step(&mut self, window: &mut Window, cx: &mut App) {
        let Some(phase) = self.autoscroll else { return };
        let now = Instant::now();
        if let Some(last) = self.last_frame.replace(now) {
            self.intervals.push(now - last);
        }
        let frames = self.intervals.len();
        let next = match phase {
            Phase::Down if frames >= FRAMES_EACH_WAY => Phase::Up,
            Phase::Up if frames >= 2 * FRAMES_EACH_WAY => Phase::Done,
            other => other,
        };
        self.autoscroll = Some(next);
        match next {
            Phase::Down => {
                self.state.scroll_by(px(SPEED));
                self.deepest = self.deepest.max(self.state.logical_scroll_top().item_ix);
            }
            Phase::Up => self.state.scroll_by(px(-SPEED)),
            Phase::Done => {
                let top = self.state.logical_scroll_top().item_ix;
                println!(
                    "{} lowest_row_reached={}",
                    FrameStats::from(self.intervals.as_slice()),
                    self.deepest.max(top)
                );
                cx.quit();
                return;
            }
        }
        window.request_animation_frame();
    }
}

impl Render for ListSpike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.step(window, cx);
        let appearance = cx.global::<Appearance>();
        let theme = &appearance.theme;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(appearance.fonts.ui_family.clone())
            .text_size(appearance.fonts.ui_size)
            .child(
                list(self.state.clone(), |ix, _, cx| {
                    let theme = &cx.global::<Appearance>().theme;
                    div()
                        .px_4()
                        .py_1()
                        .border_b_1()
                        .border_color(theme.border_variant)
                        .child(row_text(ix))
                        .into_any_element()
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
