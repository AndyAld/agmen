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

/// Asserts the drawn breadcrumb splits its `parts` parts with the "›"
/// glyph (V80: console.py's 12 px "›" in icon.muted, nodes 609 and 11630):
/// one separator before each part after the first, each narrower than a
/// 12 px icon, so not a chevron icon.
pub(crate) fn assert_breadcrumb_glyphs(cx: &mut VisualTestContext, parts: usize) {
    let selector =
        |ix: usize| -> &'static str { Box::leak(format!("breadcrumb:sep:{ix}").into_boxed_str()) };
    for ix in 1..parts {
        let sel = selector(ix);
        let sep = cx
            .debug_bounds(sel)
            .unwrap_or_else(|| panic!("no separator drawn as {sel}"));
        assert!(
            sep.size.width < crate::ui::tokens::ICON_SM,
            "{sel} is {sep:?}, wider than a glyph"
        );
    }
    assert!(
        cx.debug_bounds(selector(parts)).is_none(),
        "a separator too many"
    );
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
