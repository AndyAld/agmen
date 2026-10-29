//! A dock tree's foldable section row with its count ("PLANNING ROOM 4",
//! node 8684; console.py `git_section`), titled with a [`SectionHeading`].

use gpui::{
    App, ClickEvent, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Window, div,
};

use super::{Badge, ClickHandler, Color, Icon, IconSize, Indent, Label, ListItem, SectionHeading};
use crate::icons::UiIcon;

/// Uppercase section headings or case-preserving names in either font.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SectionTitleStyle {
    #[default]
    Heading,
    Name,
    Code,
}

/// Mono small text on the section's count chip.
pub(crate) fn count_badge(text: impl Into<SharedString>) -> Badge {
    Badge::new(text, Color::Placeholder).count()
}

/// A foldable section in a dock tree: chevron, heading and a count chip.
/// Tests find it by its id and its count as `"{id}:count"`.
#[derive(IntoElement)]
pub(crate) struct SectionHeader {
    id: SharedString,
    title: SharedString,
    open: bool,
    count: Option<SharedString>,
    count_selector: Option<SharedString>,
    title_style: SectionTitleStyle,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for SectionHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SectionHeader")
            .field("id", &self.id)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

impl SectionHeader {
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            open: true,
            count: None,
            count_selector: None,
            title_style: SectionTitleStyle::Heading,
            on_click: None,
        }
    }

    /// Whether its rows show: the chevron points down when open.
    #[must_use]
    pub(crate) const fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// The chip after the heading, such as "4" or "planned".
    #[must_use]
    pub(crate) fn count(mut self, count: impl Into<SharedString>) -> Self {
        self.count = Some(count.into());
        self
    }

    /// Keeps an existing count selector when migrating a tree header.
    #[must_use]
    pub(crate) fn count_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.count_selector = Some(selector.into());
        self
    }

    #[must_use]
    pub(crate) const fn title_style(mut self, style: SectionTitleStyle) -> Self {
        self.title_style = style;
        self
    }

    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for SectionHeader {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let chevron = if self.open {
            UiIcon::ChevronDown
        } else {
            UiIcon::ChevronRight
        };
        let count_selector = self
            .count_selector
            .map_or_else(|| format!("{}:count", self.id), |s| s.to_string());
        let title_selector = format!("{}:title", self.id);
        let title = match self.title_style {
            SectionTitleStyle::Heading => SectionHeading::new(&self.title).into_any_element(),
            SectionTitleStyle::Name | SectionTitleStyle::Code => {
                let label = Label::new(self.title).weight(FontWeight::MEDIUM);
                if self.title_style == SectionTitleStyle::Code {
                    label.code()
                } else {
                    label
                }
                .into_any_element()
            }
        };
        let mut row = ListItem::new(self.id, Indent::Group)
            .leading(Icon::new(chevron).size(IconSize::Small).color(Color::Muted))
            .child(div().debug_selector(|| title_selector).child(title));
        if let Some(count) = self.count {
            row = row.child(
                gpui::div()
                    .debug_selector(|| count_selector)
                    .child(count_badge(count)),
            );
        }
        match self.on_click {
            Some(handler) => row.on_click(handler),
            None => row,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Styled as _, TestAppContext, px};

    use super::*;
    use crate::ui::test_support::draw;
    use crate::ui::tokens::AGENT_ROW_HEIGHT;

    #[gpui::test]
    fn section_header_is_a_row_with_its_count(cx: &mut TestAppContext) {
        let cx = draw(cx, || {
            gpui::div()
                .w(px(272.))
                .child(SectionHeader::new("room", "Planning room").count("4"))
                .into_any_element()
        });
        let row = cx.debug_bounds("room").unwrap();
        assert_eq!(row.size.height, AGENT_ROW_HEIGHT);
        // The chevron sits at the group indent, as the Agents dock's people.
        assert_eq!(cx.debug_bounds("room:leading").unwrap().origin.x, px(10.));
        assert!(cx.debug_bounds("room:count").is_some());
    }
}
