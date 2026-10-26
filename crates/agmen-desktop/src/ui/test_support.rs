//! Helpers for GPUI view tests.

use gpui::{
    AnyElement, AnyView, Context, IntoElement, ParentElement as _, Pixels, Render, Size,
    Styled as _, TestAppContext, VisualTestContext, Window, div,
};
use gpui::{InteractiveElement as _, Modifiers};

use super::components::{Button, ButtonStyle};
use super::tokens::MOCKUP_REM;
use crate::appearance::{Appearance, IconThemeChoice};
use crate::icons::FileIcons;

/// Sets the globals every view reads: the built-in appearance and Lucide
/// file icons.
pub(crate) fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Appearance::builtin());
        let (icons, _) = FileIcons::new(&IconThemeChoice::Lucide, std::path::Path::new(""));
        cx.set_global(icons);
    });
}

/// A view that renders one component, wrapped in a box sized to it.
struct Harness(Box<dyn Fn() -> AnyElement>);

impl Render for Harness {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div()
            .size_full()
            .flex()
            .items_start()
            .child(div().debug_selector(|| "smoke".into()).child((self.0)()))
    }
}

/// Draws `make()` in a test window, so a test can read the bounds of the
/// selectors inside it with `debug_bounds`.
pub(crate) fn draw(
    cx: &mut TestAppContext,
    make: impl Fn() -> AnyElement + 'static,
) -> &mut VisualTestContext {
    init(cx);
    let (_, cx) = cx.add_window_view(|_, _| Harness(Box::new(make)));
    redraw(cx);
    cx
}

/// Renders `make()` in a test window and returns its laid out size.
pub(crate) fn rendered_size(
    cx: &mut TestAppContext,
    make: impl Fn() -> AnyElement + 'static,
) -> Size<Pixels> {
    draw(cx, make)
        .debug_bounds("smoke")
        .map(|bounds| bounds.size)
        .unwrap_or_default()
}

/// Draws the window once so `debug_bounds` sees the current state.
pub(crate) fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.refresh();
        // The arena is cleared on the next frame.
        drop(window.draw(cx));
    });
}

/// Clicks the middle of the element with debug selector `selector`.
pub(crate) fn click(cx: &mut VisualTestContext, selector: &'static str) {
    redraw(cx);
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("nothing is drawn with selector {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    redraw(cx);
}

/// A view under a reference [`ButtonStyle::Plain`] button, found as
/// `want-plain`, to compare a view's button with.
pub(crate) struct PlainButtonHost {
    view: AnyView,
    label: &'static str,
}

impl PlainButtonHost {
    pub(crate) fn new(view: impl Into<AnyView>, label: &'static str) -> Self {
        Self {
            view: view.into(),
            label,
        }
    }
}

impl Render for PlainButtonHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_start()
            .child(Button::new("want-plain", self.label).style(ButtonStyle::Plain))
            .child(div().w_full().flex_1().child(self.view.clone()))
    }
}

/// The quads the last frame painted exactly over the element `selector`:
/// its own fill and border, read with `background.as_solid()` and
/// `border_color`, as Zed's tests do (gpui's `Window::painted_quads`).
pub(crate) fn painted_at(cx: &mut VisualTestContext, selector: &'static str) -> Vec<gpui::Quad> {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("nothing is drawn with selector {selector}"));
    cx.update(|window, _| {
        let scale = window.scale_factor();
        let near = |scaled: gpui::ScaledPixels, logical: gpui::Pixels| {
            (scaled.0 / scale - f32::from(logical)).abs() < 0.5
        };
        window
            .painted_quads()
            .into_iter()
            .filter(|quad| {
                near(quad.bounds.origin.x, bounds.origin.x)
                    && near(quad.bounds.origin.y, bounds.origin.y)
                    && near(quad.bounds.size.width, bounds.size.width)
                    && near(quad.bounds.size.height, bounds.size.height)
            })
            .collect()
    })
}

/// The solid fills among [`painted_at`]'s quads.
pub(crate) fn fills_at(cx: &mut VisualTestContext, selector: &'static str) -> Vec<gpui::Hsla> {
    painted_at(cx, selector)
        .iter()
        .filter_map(|quad| quad.background.as_solid())
        .filter(|fill| fill.a > 0.)
        .collect()
}
