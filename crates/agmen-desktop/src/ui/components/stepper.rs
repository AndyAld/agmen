//! A number with minus and plus buttons, as Settings' font sizes.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::{ChangeHandler, Color, Label};
use crate::appearance::Appearance;
use crate::ui::tokens::{BORDER_WIDTH, RADIUS_MD, SPACE_3, SPACE_9};

/// Font size bounds, from the plan's `size_stepper_bounds`.
const DEFAULT_MIN: u32 = 6;
const DEFAULT_MAX: u32 = 72;

#[derive(IntoElement)]
pub(crate) struct Stepper {
    id: SharedString,
    value: u32,
    min: u32,
    max: u32,
    on_change: Option<ChangeHandler<u32>>,
}

impl std::fmt::Debug for Stepper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stepper")
            .field("id", &self.id)
            .field("value", &self.value)
            .field("min", &self.min)
            .field("max", &self.max)
            .finish_non_exhaustive()
    }
}

impl Stepper {
    /// A stepper from 6 to 72 in steps of 1. `id` names the control; tests
    /// find its buttons as `"{id}:decrement"` and `"{id}:increment"` with
    /// `debug_bounds`.
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>, value: u32) -> Self {
        Self {
            id: id.into(),
            value,
            min: DEFAULT_MIN,
            max: DEFAULT_MAX,
            on_change: None,
        }
    }

    /// The lowest and highest values, in either order.
    #[must_use]
    pub(crate) fn range(mut self, a: u32, b: u32) -> Self {
        self.min = a.min(b);
        self.max = a.max(b);
        self
    }

    /// Called with the new value when a button is clicked.
    #[must_use]
    pub(crate) fn on_change(
        mut self,
        handler: impl Fn(u32, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    /// The value shown: the given value clamped to the range.
    fn shown(&self) -> u32 {
        self.value.clamp(self.min, self.max)
    }

    /// The value one step down, or `None` at the minimum.
    fn decremented(&self) -> Option<u32> {
        let value = self.shown();
        (value > self.min).then(|| value - 1)
    }

    /// The value one step up, or `None` at the maximum.
    fn incremented(&self) -> Option<u32> {
        let value = self.shown();
        (value < self.max).then(|| value + 1)
    }
}

/// The minus or plus button: drawn in `Color::Muted` with no hover and no
/// click when `next` is `None`, at a bound.
fn step_button(
    selector: String,
    glyph: &'static str,
    next: Option<u32>,
    on_change: Option<&ChangeHandler<u32>>,
    cx: &App,
) -> impl IntoElement {
    let hover = cx.global::<Appearance>().theme.element_hover;
    let action = next.zip(on_change.cloned());
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(|| selector)
        .flex()
        .flex_none()
        .items_center()
        .py(SPACE_3)
        .px(SPACE_9)
        .when_some(action, |this, (next, handler)| {
            this.tab_index(0)
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                // Buttons sit flush in the stepper's border, so keyboard focus
                // shows as the hover fill rather than a ring.
                .focus_visible(move |style| style.bg(hover))
                .on_click(move |_, window, cx| handler(next, window, cx))
        })
        .child(Label::new(glyph).code().color(if next.is_some() {
            Color::Placeholder
        } else {
            Color::Muted
        }))
}

impl RenderOnce for Stepper {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let (border, background) = (theme.border, theme.background);
        div()
            .flex()
            .flex_none()
            .rounded(RADIUS_MD)
            .border(BORDER_WIDTH)
            .border_color(border)
            .overflow_hidden()
            .child(step_button(
                format!("{}:decrement", self.id),
                "\u{2212}",
                self.decremented(),
                self.on_change.as_ref(),
                cx,
            ))
            .child(
                div()
                    .py(SPACE_3)
                    .px(SPACE_9)
                    .bg(background)
                    .child(Label::new(self.shown().to_string()).code()),
            )
            .child(step_button(
                format!("{}:increment", self.id),
                "+",
                self.incremented(),
                self.on_change.as_ref(),
                cx,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::control_harness::{Reported, open};
    use crate::ui::test_support::click;
    use gpui::TestAppContext;

    /// Renders `make()` with a reporting handler, clicks `button`, and
    /// returns what it reported.
    fn click_with(
        cx: &mut TestAppContext,
        make: impl Fn() -> Stepper + 'static,
        button: &'static str,
    ) -> Option<u32> {
        let reported = Reported::default();
        let sink = reported.clone();
        let cx = open(cx, move || {
            let sink = sink.clone();
            make()
                .on_change(move |value, _, _| sink.set(Some(value)))
                .into_any_element()
        });
        click(cx, button);
        reported.get()
    }

    /// Renders a stepper at `value` with the default range, clicks `button`,
    /// and returns what it reported.
    fn click_at(cx: &mut TestAppContext, value: u32, button: &'static str) -> Option<u32> {
        click_with(cx, move || Stepper::new("size", value), button)
    }

    #[test]
    fn size_stepper_bounds() {
        let at_min = Stepper::new("size", 6);
        assert_eq!(at_min.decremented(), None);
        assert_eq!(at_min.incremented(), Some(7));
        let at_max = Stepper::new("size", 72);
        assert_eq!(at_max.incremented(), None);
        assert_eq!(at_max.decremented(), Some(71));
        assert_eq!(Stepper::new("size", 2).shown(), 6);
        assert_eq!(Stepper::new("size", 90).shown(), 72);
        let custom = Stepper::new("size", 3).range(10, 4);
        assert_eq!((custom.shown(), custom.decremented()), (4, None));
    }

    #[gpui::test]
    fn decrement_at_min_is_disabled(cx: &mut TestAppContext) {
        assert_eq!(click_at(cx, 6, "size:decrement"), None);
    }

    #[gpui::test]
    fn increment_at_max_is_disabled(cx: &mut TestAppContext) {
        assert_eq!(click_at(cx, 72, "size:increment"), None);
    }

    #[gpui::test]
    fn stepper_increments_one_step(cx: &mut TestAppContext) {
        assert_eq!(click_at(cx, 15, "size:increment"), Some(16));
    }

    #[gpui::test]
    fn stepper_decrements_one_step(cx: &mut TestAppContext) {
        assert_eq!(click_at(cx, 15, "size:decrement"), Some(14));
    }

    #[gpui::test]
    fn custom_range_disables_increment_at_its_max(cx: &mut TestAppContext) {
        let make = || Stepper::new("size", 12).range(10, 12);
        assert_eq!(click_with(cx, make, "size:increment"), None);
    }

    #[gpui::test]
    fn custom_range_steps_below_the_default_min(cx: &mut TestAppContext) {
        let make = || Stepper::new("size", 3).range(2, 4);
        assert_eq!(click_with(cx, make, "size:decrement"), Some(2));
    }
}
