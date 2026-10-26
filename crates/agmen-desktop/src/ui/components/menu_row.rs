//! A row of a popover menu, as the switch menu's agent rows (node 1806):
//! a 12 px leading slot with a check on the current row, the row's
//! content, a spacer, then the end slot.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::{ClickHandler, Color, Icon, IconSize};
use crate::appearance::Appearance;
use crate::icons::UiIcon;
use crate::ui::tokens::{ICON_SM, MENU_ROW_HEIGHT, RADIUS_MD, SPACE_8, SPACE_10};

#[derive(IntoElement)]
pub(crate) struct MenuRow {
    id: SharedString,
    checked: bool,
    leading: Option<AnyElement>,
    children: Vec<AnyElement>,
    end: Option<AnyElement>,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for MenuRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuRow")
            .field("id", &self.id)
            .field("checked", &self.checked)
            .finish_non_exhaustive()
    }
}

impl MenuRow {
    /// `id` names the row; tests find it, and its check as `"{id}:check"`,
    /// with `debug_bounds`.
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            checked: false,
            leading: None,
            children: Vec::new(),
            end: None,
            on_click: None,
        }
    }

    /// Draws a check in the leading slot, as the current session's row.
    #[must_use]
    pub(crate) const fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Something in the leading slot instead of a check, such as a "+".
    #[must_use]
    pub(crate) fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    /// Something at the row's far end, such as a note and a button.
    #[must_use]
    pub(crate) fn end(mut self, end: impl IntoElement) -> Self {
        self.end = Some(end.into_any_element());
        self
    }

    /// Makes the row clickable, with the hover fill.
    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl ParentElement for MenuRow {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for MenuRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        // The mockup's hovered row (node 1812) fills with the selected color.
        let hover = cx.global::<Appearance>().theme.element_selected;
        let selector = self.id.to_string();
        let check = format!("{selector}:check");
        let slot = div().size(ICON_SM).flex_none().flex().items_center();
        let slot = match (self.leading, self.checked) {
            (Some(leading), _) => slot.child(leading),
            (None, true) => slot.debug_selector(|| check).child(
                Icon::new(UiIcon::Check)
                    .size(IconSize::Small)
                    .color(Color::Accent),
            ),
            (None, false) => slot,
        };
        div()
            .id(self.id)
            .debug_selector(|| selector)
            .h(MENU_ROW_HEIGHT)
            .w_full()
            .flex()
            .items_center()
            .gap(SPACE_8)
            .px(SPACE_10)
            .rounded(RADIUS_MD)
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(handler)
            })
            .child(slot)
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

    #[gpui::test]
    fn menu_row_is_thirty_tall(cx: &mut TestAppContext) {
        let row = rendered_size(cx, || {
            div()
                .w(px(430.))
                .child(MenuRow::new("row").child(Label::new("claude").code()))
                .into_any_element()
        });
        assert_eq!(row.height, MENU_ROW_HEIGHT);
    }

    #[gpui::test]
    fn only_the_checked_row_draws_a_check(cx: &mut TestAppContext) {
        let cx = draw(cx, || {
            div()
                .w(px(430.))
                .child(MenuRow::new("on").checked(true))
                .child(MenuRow::new("off"))
                .into_any_element()
        });
        assert!(cx.debug_bounds("on:check").is_some());
        assert!(cx.debug_bounds("off:check").is_none());
        let (on, off) = (
            cx.debug_bounds("on").unwrap(),
            cx.debug_bounds("off").unwrap(),
        );
        assert_eq!(on.size, off.size);
    }
}
