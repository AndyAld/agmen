//! An uppercase section heading, as the switch menu's "Terminal agents on
//! this machine" (node 1803), the add agent dialog's "Install it yourself"
//! (node 2422), the plan panel's "GOAL" (node 8943) and a dock section's
//! title.

use gpui::{App, FontWeight, IntoElement, RenderOnce, SharedString, Window};

use super::{Color, Label, LabelSize};

/// Small semibold text in the muted color, uppercased. The mockup's 6 %
/// letter spacing is left out: gpui text has none.
#[derive(Debug, IntoElement)]
pub(crate) struct SectionHeading {
    title: SharedString,
}

impl SectionHeading {
    /// `title` is shown uppercased.
    #[must_use]
    pub(crate) fn new(title: &str) -> Self {
        Self {
            title: title.to_uppercase().into(),
        }
    }

    /// The text as drawn.
    #[cfg(test)]
    pub(crate) fn title(&self) -> &str {
        &self.title
    }
}

impl RenderOnce for SectionHeading {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        Label::new(self.title)
            .size(LabelSize::Small)
            .color(Color::Muted)
            .weight(FontWeight::SEMIBOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn heading_renders(cx: &mut gpui::TestAppContext) {
        use gpui::IntoElement as _;
        let size = crate::ui::test_support::rendered_size(cx, || {
            SectionHeading::new("Goal").into_any_element()
        });
        assert!(size.width > gpui::px(10.));
    }

    #[test]
    fn heading_is_uppercase() {
        assert_eq!(
            SectionHeading::new("Install it yourself").title.as_ref(),
            "INSTALL IT YOURSELF"
        );
    }
}
