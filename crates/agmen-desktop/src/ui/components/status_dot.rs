//! An agent's status glyph: ● working, ○ idle, ◐ reading.

use gpui::{App, IntoElement, RenderOnce, Window};

use super::{Color, Label, LabelSize};
use crate::backend::AgentStatus;

/// The glyph and color for `status`, from `STATUS` in console.py.
#[must_use]
pub(crate) const fn status_glyph(status: AgentStatus) -> (&'static str, Color) {
    match status {
        AgentStatus::Working => ("●", Color::Success),
        AgentStatus::Idle => ("○", Color::Placeholder),
        AgentStatus::Reading => ("◐", Color::Accent),
    }
}

#[derive(Debug, IntoElement)]
pub(crate) struct StatusDot {
    status: AgentStatus,
}

impl StatusDot {
    #[must_use]
    pub(crate) const fn new(status: AgentStatus) -> Self {
        Self { status }
    }
}

impl RenderOnce for StatusDot {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let (glyph, color) = status_glyph(self.status);
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
    }

    #[gpui::test]
    fn status_dot_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            StatusDot::new(AgentStatus::Reading).into_any_element()
        });
        assert!(size.width > px(0.));
    }
}
