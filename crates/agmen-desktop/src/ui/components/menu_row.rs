//! A row in a picker menu with a check slot, as the model picker's model
//! rows (node 3361): 26 px tall, the check in a 12 px slot, the label in
//! the code font, and the checked row on the selected fill.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::{ClickHandler, Color, Icon, IconSize, Label};
use crate::appearance::{Appearance, Theme};
use crate::icons::UiIcon;
use crate::ui::tokens::{
    ICON_SM, MENU_ROW_HEIGHT, RADIUS_MD, SPACE_8, SPACE_10, TINT_MENU_HOVER, tint,
};

// shortcut: Dropdown::menu_row (dropdown.rs) keeps its own row: it hugs its
// label with 3/8 padding, may use the UI font, and draws no selected fill,
// as the Settings frames show. Fold it into MenuRow with a compact variant
// when a third menu needs a row or the Settings frames are compared again.
#[derive(IntoElement)]
pub(crate) struct MenuRow {
    id: SharedString,
    label: SharedString,
    checked: bool,
    dim: bool,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for MenuRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuRow")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("checked", &self.checked)
            .finish_non_exhaustive()
    }
}

impl MenuRow {
    /// `id` names the row; tests find it, and its check as `"{id}:check"`,
    /// with `debug_bounds`.
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            checked: false,
            dim: false,
            on_click: None,
        }
    }

    /// Draws the check and the selected fill.
    #[must_use]
    pub(crate) const fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// Sets the label in the placeholder color, as "configured model".
    #[must_use]
    pub(crate) const fn dim(mut self) -> Self {
        self.dim = true;
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

/// The fill a row takes under the pointer: none on the checked row, which
/// keeps its selected fill, and otherwise the selected fill at half
/// strength. Zed's `element_hover` would vanish here: Andy's Gruvbox and the
/// built-in theme give it the menu's own `elevated_surface`.
fn hover_fill(theme: &Theme, checked: bool) -> Option<Hsla> {
    (!checked).then(|| tint(theme.element_selected, TINT_MENU_HOVER))
}

impl RenderOnce for MenuRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let hover = hover_fill(theme, self.checked);
        let selector = self.id.to_string();
        let check_selector = format!("{selector}:check");
        div()
            .id(self.id)
            .debug_selector(|| selector)
            .h(MENU_ROW_HEIGHT)
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_8)
            .px(SPACE_10)
            .rounded(RADIUS_MD)
            .when(self.checked, |this| this.bg(theme.element_selected))
            .when_some(hover, |this, hover| {
                this.hover(move |style| style.bg(hover))
            })
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            // An empty slot on unchecked rows keeps every label in line.
            .child(div().size(ICON_SM).flex_none().when(self.checked, |this| {
                this.child(
                    div().debug_selector(|| check_selector).child(
                        Icon::new(UiIcon::Check)
                            .size(IconSize::Small)
                            .color(Color::Accent),
                    ),
                )
            }))
            .child(Label::new(self.label).code().color(if self.dim {
                Color::Placeholder
            } else {
                Color::Default
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{draw, rendered_size};
    use gpui::TestAppContext;

    #[gpui::test]
    fn menu_row_is_26_tall(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || MenuRow::new("row", "opus").into_any_element());
        assert_eq!(size.height, MENU_ROW_HEIGHT);
    }

    #[gpui::test]
    fn hover_shows_on_the_menu_and_keeps_the_checked_fill(cx: &mut TestAppContext) {
        // The built-in theme, like Andy's Gruvbox, has element_hover equal
        // to the menu's elevated_surface.
        crate::ui::test_support::init(cx);
        let theme = cx.update(|cx| cx.global::<Appearance>().theme.clone());
        assert_eq!(hover_fill(&theme, true), None);
        let hover = hover_fill(&theme, false).expect("a hover fill");
        assert_ne!(hover, theme.elevated_surface);
        assert_ne!(hover, theme.element_selected);
    }

    #[gpui::test]
    fn only_the_checked_row_draws_its_check(cx: &mut TestAppContext) {
        let cx = draw(cx, || {
            div()
                .child(MenuRow::new("a", "sonnet"))
                .child(MenuRow::new("b", "opus").checked(true))
                .into_any_element()
        });
        assert!(cx.debug_bounds("a:check").is_none());
        let check = cx.debug_bounds("b:check").expect("the check");
        assert_eq!(check.size.width, ICON_SM);
    }
}
