//! A row of a popover menu with a check slot: the switch menu's agent rows
//! (node 1806: a 12 px leading slot with a check on the current row, the
//! row's content, a spacer, then the end slot) and, drawn as a picker row,
//! the model picker's model rows (node 3361: 26 px tall, the checked row on
//! the selected fill), Dropdown's options and the scope chip's worktrees.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Hsla, InteractiveElement as _, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::{ClickHandler, Color, Icon, IconSize};
use crate::appearance::{Appearance, Theme};
use crate::icons::UiIcon;
use crate::ui::tokens::{
    ICON_SM, MENU_ROW_HEIGHT, PICKER_ROW_HEIGHT, RADIUS_MD, SPACE_8, SPACE_10,
};

/// Which menu's row it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Look {
    /// The switch menu's: 30 px, the selected fill under the pointer.
    Menu,
    /// The model picker's, Dropdown's and the scope chip's: 26 px, the
    /// checked row on the selected fill.
    Picker,
}

#[derive(IntoElement)]
pub(crate) struct MenuRow {
    id: SharedString,
    look: Look,
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
            .field("look", &self.look)
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
            look: Look::Menu,
            checked: false,
            leading: None,
            children: Vec::new(),
            end: None,
            on_click: None,
        }
    }

    /// Draws the row as the model picker's (node 3361): 26 px tall, the
    /// checked row on the selected fill.
    #[must_use]
    pub(crate) const fn picker(mut self) -> Self {
        self.look = Look::Picker;
        self
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

/// The fill a row takes under the pointer: the selected color, as Zed's
/// pickers select the hovered row and fill it with `ghost_element.selected`
/// (the switch menu's hovered row, node 1812, has it too). A checked picker
/// row has none, as it keeps its selected fill. Zed's `element_hover`
/// would vanish here: Andy's Gruvbox and the built-in theme give it the
/// menu's own `elevated_surface`.
fn hover_fill(theme: &Theme, look: Look, checked: bool) -> Option<Hsla> {
    match look {
        Look::Menu => Some(theme.element_selected),
        Look::Picker => (!checked).then_some(theme.element_selected),
    }
}

impl RenderOnce for MenuRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let hover = hover_fill(theme, self.look, self.checked);
        let picker = self.look == Look::Picker;
        let selector = self.id.to_string();
        let check = format!("{selector}:check");
        // An empty slot on unchecked rows keeps every label in line.
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
            .h(if picker {
                PICKER_ROW_HEIGHT
            } else {
                MENU_ROW_HEIGHT
            })
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_8)
            .px(SPACE_10)
            .rounded(RADIUS_MD)
            .when(picker && self.checked, |this| {
                this.bg(theme.element_selected)
            })
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer()
                    .when_some(hover, |this, hover| {
                        this.hover(move |style| style.bg(hover))
                    })
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
    use crate::ui::test_support::{MIN_MENU_HOVER_CONTRAST, contrast_over, draw, rendered_size};
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
    fn picker_row_is_26_tall(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            MenuRow::new("row")
                .picker()
                .child(Label::new("opus").code())
                .into_any_element()
        });
        assert_eq!(size.height, PICKER_ROW_HEIGHT);
    }

    #[gpui::test]
    fn picker_hover_shows_on_the_menu_and_keeps_the_checked_fill(cx: &mut TestAppContext) {
        // The built-in theme, like Andy's Gruvbox, has element_hover equal
        // to the menu's elevated_surface.
        crate::ui::test_support::init(cx);
        let theme = cx.update(|cx| cx.global::<Appearance>().theme.clone());
        assert_eq!(hover_fill(&theme, Look::Picker, true), None);
        for look in [Look::Menu, Look::Picker] {
            let hover = hover_fill(&theme, look, false).expect("a hover fill");
            let contrast = contrast_over(hover, theme.elevated_surface);
            assert!(
                contrast >= MIN_MENU_HOVER_CONTRAST,
                "{look:?} hover is {contrast:.2}:1 on the menu"
            );
        }
    }

    #[gpui::test]
    fn only_the_checked_row_draws_a_check(cx: &mut TestAppContext) {
        let cx = draw(cx, || {
            div()
                .w(px(430.))
                .child(MenuRow::new("on").checked(true))
                .child(MenuRow::new("off"))
                .child(MenuRow::new("picked").picker().checked(true))
                .into_any_element()
        });
        assert!(cx.debug_bounds("on:check").is_some());
        assert!(cx.debug_bounds("off:check").is_none());
        let (on, off) = (
            cx.debug_bounds("on").unwrap(),
            cx.debug_bounds("off").unwrap(),
        );
        assert_eq!(on.size, off.size);
        let check = cx.debug_bounds("picked:check").expect("the check");
        assert_eq!(check.size.width, ICON_SM);
    }
}
