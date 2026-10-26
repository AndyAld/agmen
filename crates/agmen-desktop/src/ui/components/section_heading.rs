//! An uppercase section heading, as the switch menu's "Terminal agents on
//! this machine" (node 1803) and the add agent dialog's "Install it
//! yourself" (node 2422).

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

    #[test]
    fn heading_is_uppercase() {
        assert_eq!(
            SectionHeading::new("Install it yourself").title.as_ref(),
            "INSTALL IT YOURSELF"
        );
    }
}
