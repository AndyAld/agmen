//! An agent's status glyph: ● working, ○ idle, ◐ reading, ◌ offline.

use gpui::{App, IntoElement, RenderOnce, Window};

use super::{Color, Label, LabelSize};
use crate::backend::AgentStatus;

/// One pending tick for the whole app, regardless of the number of dots.
/// Rendering a reading dot renews it; without readers it stops after that tick.
#[derive(Default)]
struct ReadingAnimation {
    second_half: bool,
    pending: bool,
}

impl gpui::Global for ReadingAnimation {}

fn reading_glyph(cx: &mut App) -> &'static str {
    let reduce_motion = cx.reduce_motion();
    #[cfg(all(target_os = "macos", not(test)))]
    let reduce_motion = reduce_motion
        || objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
    if reduce_motion {
        return "◐";
    }
    if !cx.has_global::<ReadingAnimation>() {
        cx.set_global(ReadingAnimation::default());
    }
    let animation = cx.global_mut::<ReadingAnimation>();
    let glyph = if animation.second_half { "◑" } else { "◐" };
    if !animation.pending {
        animation.pending = true;
        cx.spawn(async |cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            // A single redraw request covers all reading dots in all windows.
            cx.update(|cx| {
                let animation = cx.global_mut::<ReadingAnimation>();
                animation.pending = false;
                animation.second_half = !animation.second_half;
                cx.refresh_windows();
            });
        })
        .detach();
    }
    glyph
}

/// The glyph and color for `status`, from `STATUS` in console.py.
#[must_use]
pub(crate) const fn status_glyph(status: AgentStatus) -> (&'static str, Color) {
    match status {
        AgentStatus::Working => ("●", Color::Success),
        AgentStatus::Idle => ("○", Color::Placeholder),
        // Task 7: console.py has no offline; DIM (icon.muted) and a dotted
        // ring keep it apart from idle's FAINT ○.
        AgentStatus::Offline => ("◌", Color::Muted),
        AgentStatus::Reading => ("◐", Color::Accent),
    }
}

#[derive(Debug, IntoElement)]
pub(crate) struct StatusDot {
    status: AgentStatus,
    #[cfg(test)]
    drawn: Option<std::rc::Rc<std::cell::Cell<&'static str>>>,
}

impl StatusDot {
    #[must_use]
    pub(crate) const fn new(status: AgentStatus) -> Self {
        Self {
            status,
            #[cfg(test)]
            drawn: None,
        }
    }
}

impl RenderOnce for StatusDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (mut glyph, color) = status_glyph(self.status);
        if self.status == AgentStatus::Reading {
            glyph = reading_glyph(cx);
        }
        #[cfg(test)]
        if let Some(drawn) = self.drawn {
            drawn.set(glyph);
        }
        Label::new(glyph).size(LabelSize::XSmall).color(color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[test]
    fn glyphs_match_mockup() {
        assert_eq!(status_glyph(AgentStatus::Working), ("●", Color::Success));
        assert_eq!(status_glyph(AgentStatus::Idle), ("○", Color::Placeholder));
        assert_eq!(status_glyph(AgentStatus::Reading), ("◐", Color::Accent));
        // Task 7: offline is dimmer than idle and drawn apart from it, so
        // an offline agent does not read as reachable.
        assert_eq!(status_glyph(AgentStatus::Offline), ("◌", Color::Muted));
    }

    #[gpui::test]
    fn status_dot_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            StatusDot::new(AgentStatus::Reading).into_any_element()
        });
        assert!(size.width > px(0.));
    }

    #[gpui::test]
    fn reading_alternates_without_moving(cx: &mut TestAppContext) {
        assert_reading_frames(cx);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reading_bounds_match_with_core_text() {
        let mut cx = crate::ui::test_support::core_text_app();
        assert_reading_frames(&mut cx);
    }

    fn assert_reading_frames(cx: &mut TestAppContext) {
        use crate::ui::test_support::{draw, redraw};
        use std::{cell::Cell, rc::Rc, time::Duration};

        let glyph = Rc::new(Cell::new(""));
        let seen = glyph.clone();
        let cx = draw(cx, move || {
            let mut dot = StatusDot::new(AgentStatus::Reading);
            dot.drawn = Some(seen.clone());
            dot.into_any_element()
        });
        let bounds = cx.debug_bounds("smoke").unwrap();
        assert_eq!(glyph.get(), "◐");
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        redraw(cx);
        assert_eq!(glyph.get(), "◑");
        assert_eq!(cx.debug_bounds("smoke").unwrap(), bounds);
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        redraw(cx);
        assert_eq!(glyph.get(), "◐");
        assert_eq!(cx.debug_bounds("smoke").unwrap(), bounds);
    }

    #[gpui::test]
    fn other_statuses_stay_still_while_reading_animates(cx: &mut TestAppContext) {
        use gpui::{ParentElement as _, div};
        use std::{cell::Cell, rc::Rc, time::Duration};

        let statuses = [
            AgentStatus::Working,
            AgentStatus::Idle,
            AgentStatus::Offline,
            AgentStatus::Reading,
        ];
        let glyphs = statuses.map(|_| Rc::new(Cell::new("")));
        let seen = glyphs.clone();
        let cx = crate::ui::test_support::draw(cx, move || {
            div()
                .children(statuses.into_iter().zip(&seen).map(|(status, seen)| {
                    let mut dot = StatusDot::new(status);
                    dot.drawn = Some(seen.clone());
                    dot
                }))
                .into_any_element()
        });
        assert_eq!(glyphs.each_ref().map(|g| g.get()), ["●", "○", "◌", "◐"]);
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        crate::ui::test_support::redraw(cx);
        assert_eq!(glyphs.each_ref().map(|g| g.get()), ["●", "○", "◌", "◑"]);
    }

    #[gpui::test]
    fn reduce_motion_keeps_reading_static(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc, time::Duration};

        cx.update(|cx| cx.set_reduce_motion(true));
        let glyph = Rc::new(Cell::new(""));
        let seen = glyph.clone();
        let cx = crate::ui::test_support::draw(cx, move || {
            let mut dot = StatusDot::new(AgentStatus::Reading);
            dot.drawn = Some(seen.clone());
            dot.into_any_element()
        });
        assert_eq!(glyph.get(), "◐");
        cx.run_until_parked();
        for _ in 0..3 {
            cx.executor().advance_clock(Duration::from_millis(500));
            cx.run_until_parked();
            crate::ui::test_support::redraw(cx);
            assert_eq!(glyph.get(), "◐");
        }
        cx.update(|_, cx| assert!(!cx.has_global::<ReadingAnimation>()));
    }

    #[gpui::test]
    fn many_readers_share_a_tick_and_stop_when_reduced(cx: &mut TestAppContext) {
        use gpui::{ParentElement as _, div};
        use std::{cell::Cell, rc::Rc, time::Duration};

        let glyphs: Vec<_> = (0..100).map(|_| Rc::new(Cell::new(""))).collect();
        let seen = glyphs.clone();
        let cx = crate::ui::test_support::draw(cx, move || {
            div()
                .children(seen.iter().map(|seen| {
                    let mut dot = StatusDot::new(AgentStatus::Reading);
                    dot.drawn = Some(seen.clone());
                    dot
                }))
                .into_any_element()
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        crate::ui::test_support::redraw(cx);
        assert!(glyphs.iter().all(|glyph| glyph.get() == "◑"));

        // Turning motion off while a tick is pending drains that last tick.
        cx.update(|_, cx| cx.set_reduce_motion(true));
        crate::ui::test_support::redraw(cx);
        assert!(glyphs.iter().all(|glyph| glyph.get() == "◐"));
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        crate::ui::test_support::redraw(cx);
        cx.update(|_, cx| assert!(!cx.global::<ReadingAnimation>().pending));
    }
}
