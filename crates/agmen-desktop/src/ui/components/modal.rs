//! A dialog over the window: a dim over everything and a card near the top
//! (frame "Desktop · kick off": dim node 10289, card node 10290). A click on
//! the dim dismisses it, as a click outside a Zed modal does.

use gpui::{
    AnyElement, App, ClickEvent, InteractiveElement as _, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::ClickHandler;
use crate::appearance::Appearance;
use crate::ui::tokens::{
    BORDER_WIDTH, MODAL_DIM, MODAL_TOP, MODAL_WIDTH, RADIUS_MODAL, SPACE_12, SPACE_20, tint,
};

/// Tests find the dim as `id` and the card as `"{id}:card"`.
#[derive(IntoElement)]
pub(crate) struct Modal {
    id: SharedString,
    children: Vec<AnyElement>,
    on_dismiss: Option<ClickHandler>,
}

impl std::fmt::Debug for Modal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Modal")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Modal {
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            children: Vec::new(),
            on_dismiss: None,
        }
    }

    /// Called on a click on the dim, outside the card.
    #[must_use]
    pub(crate) fn on_dismiss(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dismiss = Some(Box::new(handler));
        self
    }
}

impl ParentElement for Modal {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Modal {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let card_selector = format!("{}:card", self.id);
        let dim_selector = self.id.to_string();
        let card = div()
            .id(format!("{}:card", self.id))
            .debug_selector(|| card_selector)
            .occlude()
            // A click on the card stays on it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .w(MODAL_WIDTH)
            .flex()
            .flex_col()
            .gap(SPACE_12)
            .p(SPACE_20)
            .bg(theme.elevated_surface)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .rounded(RADIUS_MODAL)
            .shadow_lg()
            .children(self.children);
        let dim = div()
            .id(self.id)
            .debug_selector(|| dim_selector)
            .occlude()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(MODAL_TOP)
            .bg(tint(theme.background, MODAL_DIM))
            .child(card);
        match self.on_dismiss {
            Some(handler) => dim.on_click(handler),
            None => dim,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::ui::components::Label;
    use crate::ui::test_support::{click, draw};
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn card_sits_below_the_top_and_dismisses_only_outside(cx: &mut TestAppContext) {
        let dismissed = Rc::new(Cell::new(0));
        let count = dismissed.clone();
        let cx = draw(cx, move || {
            let count = count.clone();
            div()
                .relative()
                .w(px(1000.))
                .h(px(700.))
                .child(
                    Modal::new("dialog")
                        .on_dismiss(move |_, _, _| count.set(count.get() + 1))
                        .child(Label::new("Kick off")),
                )
                .into_any_element()
        });
        let card = cx.debug_bounds("dialog:card").unwrap();
        assert_eq!(card.size.width, MODAL_WIDTH);
        assert_eq!(card.origin.y, MODAL_TOP);
        click(cx, "dialog:card");
        assert_eq!(dismissed.get(), 0);
        let dim = cx.debug_bounds("dialog").unwrap();
        cx.simulate_click(
            gpui::point(dim.origin.x + px(5.), dim.origin.y + px(5.)),
            gpui::Modifiers::none(),
        );
        assert_eq!(dismissed.get(), 1);
    }
}
