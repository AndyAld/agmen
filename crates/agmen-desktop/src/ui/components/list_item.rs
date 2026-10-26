//! A tree row: a group row (a person) or an item nested by depth (an agent).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement,
    Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::ClickHandler;
use crate::appearance::Appearance;
use crate::ui::tokens::{
    AGENT_ROW_HEIGHT, LEADING_SLOT, RADIUS_MD, SPACE_6, SPACE_7, SPACE_8, SPACE_10,
    TREE_INDENT_BASE, TREE_INDENT_STEP,
};

/// Where a row's content starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Indent {
    /// A group heading, such as a person, flush with the panel's padding.
    Group,
    /// An item at this depth under its group.
    Depth(u8),
    /// A tree row at this depth (console.py's `tree_row`), which draws its
    /// own chevron or marks: the Project panel's files and the Git panel's
    /// rows.
    Tree(u8),
    /// A row in a list of pages, such as Settings' page list (node 8050):
    /// flush with the list's padding, 8 px gaps and a rounded fill.
    Page,
}

impl Indent {
    /// The row's left padding: 10 for a group or a page, 26 + 14 * depth for
    /// an item, 10 + 14 * depth for a file tree row.
    #[must_use]
    pub(crate) fn padding(self) -> Pixels {
        match self {
            Self::Group | Self::Page => SPACE_10,
            Self::Depth(depth) => TREE_INDENT_BASE + TREE_INDENT_STEP * f32::from(depth),
            Self::Tree(depth) => SPACE_10 + TREE_INDENT_STEP * f32::from(depth),
        }
    }
}

/// One row: leading slot, content, a spacer, then the end slot.
#[derive(IntoElement)]
pub(crate) struct ListItem {
    id: SharedString,
    indent: Indent,
    height: Pixels,
    selected: bool,
    leading: Option<AnyElement>,
    children: Vec<AnyElement>,
    end: Option<AnyElement>,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for ListItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListItem")
            .field("id", &self.id)
            .field("indent", &self.indent)
            .finish_non_exhaustive()
    }
}

impl ListItem {
    /// `id` names the row; tests find it, and its leading slot as
    /// `"{id}:leading"`, with `debug_bounds`.
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>, indent: Indent) -> Self {
        Self {
            id: id.into(),
            indent,
            height: AGENT_ROW_HEIGHT,
            selected: false,
            leading: None,
            children: Vec::new(),
            end: None,
            on_click: None,
        }
    }

    #[must_use]
    pub(crate) const fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    #[must_use]
    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub(crate) fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    /// Drawn at the row's right edge.
    #[must_use]
    pub(crate) fn end(mut self, end: impl IntoElement) -> Self {
        self.end = Some(end.into_any_element());
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

impl ParentElement for ListItem {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

/// The fill a row takes on hover: none on a selected row, which keeps its
/// selected fill.
fn hover_fill(selected: bool, theme: &crate::appearance::Theme) -> Option<gpui::Hsla> {
    (!selected).then_some(theme.element_hover)
}

impl RenderOnce for ListItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let row_selector = self.id.to_string();
        let leading_selector = format!("{}:leading", self.id);
        div()
            .id(ElementId::Name(self.id))
            .debug_selector(|| row_selector)
            .h(self.height)
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(match self.indent {
                Indent::Group | Indent::Tree(_) => SPACE_6,
                Indent::Depth(_) => SPACE_7,
                Indent::Page => SPACE_8,
            })
            .when(self.indent == Indent::Page, |this| this.rounded(RADIUS_MD))
            .pl(self.indent.padding())
            .pr(SPACE_10)
            .when(self.selected, |this| this.bg(theme.element_selected))
            .when_some(hover_fill(self.selected, theme), |this, fill| {
                this.hover(move |style| style.bg(fill))
            })
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            .when_some(self.leading, |this, leading| {
                this.child(
                    // A minimum, not a fixed width: status glyphs line up in it
                    // (V62), while a 12 px chevron or a file icon keeps its
                    // own width instead of overflowing into the name (V74).
                    div()
                        .flex()
                        .flex_none()
                        .min_w(LEADING_SLOT)
                        .justify_center()
                        .debug_selector(|| leading_selector)
                        .child(leading),
                )
            })
            .children(self.children)
            .child(div().flex_1())
            .children(self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::Label;
    use crate::ui::test_support::{draw, rendered_size};
    use gpui::{TestAppContext, px};

    #[test]
    fn indent_padding_steps_by_depth() {
        assert_eq!(Indent::Group.padding(), px(10.));
        assert_eq!(Indent::Depth(0).padding(), px(26.));
        assert_eq!(Indent::Depth(1).padding(), px(40.));
        assert_eq!(Indent::Depth(2).padding(), px(26. + 14. * 2.));
        assert_eq!(Indent::Tree(0).padding(), px(10.));
        assert_eq!(Indent::Page.padding(), px(10.));
        assert_eq!(Indent::Tree(3).padding(), px(10. + 14. * 3.));
    }

    #[gpui::test]
    fn selected_rows_keep_their_fill_on_hover(cx: &mut TestAppContext) {
        // V117 (C245, drawn per C253): hovering a selected row keeps the
        // selected fill, as Zed's lists do; an unselected row takes hover.
        let theme = crate::appearance::Appearance::builtin().theme;
        assert_ne!(theme.element_selected, theme.element_hover);
        let cx = crate::ui::test_support::draw(cx, || {
            gpui::div()
                .w(gpui::px(200.))
                .child(
                    ListItem::new("on", Indent::Group)
                        .selected(true)
                        .child(Label::new("on")),
                )
                .child(ListItem::new("off", Indent::Group).child(Label::new("off")))
                .into_any_element()
        });
        for (row, want) in [("on", theme.element_selected), ("off", theme.element_hover)] {
            let center = cx.debug_bounds(row).unwrap().center();
            cx.simulate_mouse_move(center, None, gpui::Modifiers::none());
            crate::ui::test_support::redraw(cx);
            let fills = crate::ui::test_support::fills_at(cx, row);
            assert_eq!(fills, [want], "{row}");
        }
    }

    #[gpui::test]
    fn list_item_renders_at_row_height(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            ListItem::new("row", Indent::Depth(1))
                .selected(true)
                .leading(Label::new("●"))
                .child(Label::new("fx1"))
                .end(Label::new("claude"))
                .on_click(|_, _, _| {})
                .into_any_element()
        });
        assert_eq!(size.height, AGENT_ROW_HEIGHT);
    }

    #[gpui::test]
    fn leading_slot_fits_wider_content(cx: &mut TestAppContext) {
        // V74 and V83: a 12 px chevron or a 16 px file icon keeps its width,
        // while a narrow glyph still gets the fixed slot (V62).
        let cx = draw(cx, || {
            div()
                .w(px(300.))
                .child(
                    ListItem::new("wide", Indent::Group)
                        .leading(div().w(px(16.)).h(px(12.)))
                        .child(Label::new("andy")),
                )
                .child(
                    ListItem::new("narrow", Indent::Depth(0))
                        .leading(div().w(px(6.)).h(px(6.)))
                        .child(Label::new("fx1")),
                )
                .into_any_element()
        });
        assert_eq!(cx.debug_bounds("wide:leading").unwrap().size.width, px(16.));
        assert_eq!(
            cx.debug_bounds("narrow:leading").unwrap().size.width,
            LEADING_SLOT
        );
    }

    #[gpui::test]
    fn list_item_takes_a_height(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            ListItem::new("row", Indent::Group)
                .height(crate::ui::tokens::PROJECT_ROW_HEIGHT)
                .child(Label::new("src"))
                .into_any_element()
        });
        assert_eq!(size.height, crate::ui::tokens::PROJECT_ROW_HEIGHT);
    }
}
