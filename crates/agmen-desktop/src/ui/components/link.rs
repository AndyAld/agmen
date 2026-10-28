//! Accent text that opens something on click, such as "Browse Zed themes…"
//! or the Zed extensions page in Settings (V100, V106).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::{ClickHandler, Color, Label, LabelSize};

/// Small accent text, underlined on hover and keyboard focus when it has
/// a click handler.
#[derive(IntoElement)]
pub(crate) struct Link {
    id: ElementId,
    text: SharedString,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("id", &self.id)
            .field("text", &self.text)
            .finish_non_exhaustive()
    }
}

impl Link {
    /// `id` names the link; tests find it with `debug_bounds`.
    #[must_use]
    pub(crate) fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            on_click: None,
        }
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

impl RenderOnce for Link {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let selector = self.id.to_string();
        // Only a link that does something takes focus and underlines, as
        // Dropdown, Toggle, Segmented and Stepper do (C250).
        div()
            .id(self.id)
            .debug_selector(|| selector)
            .flex_none()
            .when_some(self.on_click, |this, handler| {
                this.tab_index(0)
                    .cursor_pointer()
                    .hover(gpui::StyleRefinement::underline)
                    .focus_visible(gpui::StyleRefinement::underline)
                    .on_click(handler)
            })
            .child(
                Label::new(self.text)
                    .size(LabelSize::Small)
                    .color(Color::Accent),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{InputEvent as _, Modifiers, TestAppContext};

    use super::*;
    use crate::ui::components::control_harness::open;
    use crate::ui::test_support::{redraw, rendered_size};
    use crate::ui::tokens::{LINE_HEIGHT, MOCKUP_REM, TEXT_SMALL};

    #[gpui::test]
    fn link_is_one_small_line(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || Link::new("l", "Browse").into_any_element());
        let line = TEXT_SMALL.to_pixels(MOCKUP_REM) * LINE_HEIGHT;
        assert!((size.height - line).abs() < gpui::px(0.25), "{size:?}");
    }

    #[gpui::test]
    fn focus_then_enter_opens_a_link(cx: &mut TestAppContext) {
        // C250: a link takes keyboard focus like the other controls.
        let clicks = Rc::new(Cell::new(0));
        let seen = clicks.clone();
        let cx = open(cx, move || {
            let seen = seen.clone();
            Link::new("link", "Browse")
                .on_click(move |_, _, _| seen.set(seen.get() + 1))
                .into_any_element()
        });
        // A click focuses a tab stop (agmen binds no Tab key yet), and Enter
        // on the focused link then clicks it again.
        let bounds = cx.debug_bounds("link").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        redraw(cx);
        let focused = cx.update(|window, cx| window.focused(cx).is_some());
        assert!(focused, "the click did not focus the link");
        // gpui clicks a focused element on Enter's key up (div.rs, keyboard
        // activation), so send the down and the up.
        let enter = gpui::Keystroke::parse("enter").unwrap();
        cx.update(|window, cx| {
            window.dispatch_event(
                gpui::KeyDownEvent {
                    keystroke: enter.clone(),
                    is_held: false,
                    prefer_character_input: false,
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(
                gpui::KeyUpEvent { keystroke: enter }.to_platform_input(),
                cx,
            );
        });
        redraw(cx);
        assert_eq!(clicks.get(), 2);
    }

    #[gpui::test]
    fn a_link_without_a_handler_takes_no_focus_or_underline(cx: &mut TestAppContext) {
        // C256: plain accent text, not a control.
        let cx = open(cx, || Link::new("link", "Browse").into_any_element());
        let bounds = cx.debug_bounds("link").unwrap();
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
        redraw(cx);
        assert!(cx.update(|window, _| window.painted_underlines().is_empty()));
        cx.simulate_click(bounds.center(), Modifiers::none());
        redraw(cx);
        assert!(cx.update(|window, cx| window.focused(cx).is_none()));
    }

    #[gpui::test]
    fn a_link_with_a_handler_underlines_on_hover(cx: &mut TestAppContext) {
        let cx = open(cx, || {
            Link::new("link", "Browse")
                .on_click(|_, _, _| {})
                .into_any_element()
        });
        let bounds = cx.debug_bounds("link").unwrap();
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
        redraw(cx);
        assert!(!cx.update(|window, _| window.painted_underlines().is_empty()));
    }

    #[gpui::test]
    fn clicking_a_link_calls_its_handler(cx: &mut TestAppContext) {
        let clicks = Rc::new(Cell::new(0));
        let seen = clicks.clone();
        let cx = open(cx, move || {
            let seen = seen.clone();
            Link::new("link", "Browse")
                .on_click(move |_, _, _| seen.set(seen.get() + 1))
                .into_any_element()
        });
        let bounds = cx.debug_bounds("link").unwrap();
        cx.simulate_click(bounds.center(), Modifiers::none());
        redraw(cx);
        assert_eq!(clicks.get(), 1);
    }
}
