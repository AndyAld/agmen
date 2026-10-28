//! A dock tree's foldable section row with its count ("PLANNING ROOM 4",
//! node 8684; console.py `git_section`), titled with a [`SectionHeading`].

use gpui::{
    App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Window,
};

use super::{Badge, ClickHandler, Color, Icon, IconSize, Indent, ListItem, SectionHeading};
use crate::icons::UiIcon;

/// A foldable section in a dock tree: chevron, heading and a count chip.
/// Tests find it by its id and its count as `"{id}:count"`.
#[derive(IntoElement)]
pub(crate) struct SectionHeader {
    id: SharedString,
    title: SharedString,
    open: bool,
    count: Option<SharedString>,
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
        let count_selector = format!("{}:count", self.id);
        let mut row = ListItem::new(self.id, Indent::Group)
            .leading(Icon::new(chevron).size(IconSize::Small).color(Color::Muted))
            .child(SectionHeading::new(&self.title));
        if let Some(count) = self.count {
            row = row.child(
                gpui::div()
                    .debug_selector(|| count_selector)
                    .child(Badge::new(count, Color::Placeholder).count()),
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
