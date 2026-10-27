//! A bordered, rounded table of rows split by rules (the plan panel's team
//! table, node 8946): each row 26 px tall, padded 8 at the sides, its cells
//! 6 apart. The caller lays out the cells.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use crate::appearance::Appearance;
use crate::ui::tokens::{AGENT_ROW_HEIGHT, BORDER_WIDTH, RADIUS_CARD, SPACE_6, SPACE_8};

/// Tests find row `i` as `"{id}:row-{i}"`.
#[derive(IntoElement)]
pub(crate) struct TableCard {
    id: SharedString,
    rows: Vec<Vec<AnyElement>>,
}

impl std::fmt::Debug for TableCard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TableCard")
            .field("id", &self.id)
            .field("rows", &self.rows.len())
            .finish()
    }
}

impl TableCard {
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            rows: Vec::new(),
        }
    }

    /// Adds a row of cells, left to right.
    #[must_use]
    pub(crate) fn row(mut self, cells: impl IntoIterator<Item = AnyElement>) -> Self {
        self.rows.push(cells.into_iter().collect());
        self
    }
}

impl RenderOnce for TableCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let last = self.rows.len().saturating_sub(1);
        let rule = crate::ui::rule_color(cx);
        let rows = self.rows.into_iter().enumerate().map(|(i, cells)| {
            let selector = format!("{}:row-{i}", self.id);
            let row = div()
                .debug_selector(|| selector)
                .h(AGENT_ROW_HEIGHT)
                .w_full()
                .flex()
                .flex_none()
                .items_center()
                .gap(SPACE_6)
                .px(SPACE_8)
                .overflow_hidden()
                .children(cells);
            if i < last {
                row.border_b(BORDER_WIDTH).border_color(rule)
            } else {
                row
            }
        });
        div()
            .w_full()
            .flex()
            .flex_col()
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .rounded(RADIUS_CARD)
            .overflow_hidden()
            .children(rows.collect::<Vec<_>>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::Label;
    use crate::ui::test_support::draw;
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn rows_are_agent_row_height_inside_a_border(cx: &mut TestAppContext) {
        let cx = draw(cx, || {
            div()
                .w(px(300.))
                .child(
                    TableCard::new("team")
                        .row([Label::new("coord").into_any_element()])
                        .row([Label::new("core").into_any_element()]),
                )
                .into_any_element()
        });
        let first = cx.debug_bounds("team:row-0").unwrap();
        let second = cx.debug_bounds("team:row-1").unwrap();
        assert_eq!(first.size.height, AGENT_ROW_HEIGHT);
        // The first row's rule sits inside it, so rows stack 26 apart.
        assert_eq!(second.origin.y - first.origin.y, AGENT_ROW_HEIGHT);
        assert_eq!(first.origin.x, px(1.));
    }
}
