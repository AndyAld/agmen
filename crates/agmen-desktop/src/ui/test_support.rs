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

/// A test app that shapes and measures with CoreText, as the app does on
/// macOS, where gpui's test text system gives every font one set of metrics.
#[cfg(target_os = "macos")]
pub(crate) fn core_text_app() -> TestAppContext {
    TestAppContext::build_with_text_system(
        gpui::TestDispatcher::new(0),
        None,
        std::sync::Arc::new(gpui_apple::AppleTextSystem::new()),
    )
}

/// The chat fixture's andy/fx1 worktree.
pub(crate) fn fx1_worktree() -> crate::backend::WorktreeInfo {
    crate::backend::chat_snapshot()
        .worktrees
        .into_iter()
        .find(|wt| wt.handle == "andy/fx1")
        .expect("the chat fixture has andy/fx1")
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

/// Asserts the open menu drawn as `frame` is the shared menu frame (node
/// 1802): the theme's `border` around `elevated_surface`, and its first
/// row `row` 4 px inside the 1 px border.
pub(crate) fn assert_menu_frame(
    cx: &mut VisualTestContext,
    frame: &'static str,
    row: &'static str,
) {
    let theme = cx.update(|_, cx| cx.global::<Appearance>().theme.clone());
    let quads = painted_at(cx, frame);
    // gpui paints the fill and the border as separate quads.
    assert!(
        quads
            .iter()
            .any(|quad| quad.border_color == theme.border && quad.border_widths.left.0 > 0.),
        "{frame} has no border in the theme's border: {quads:?}"
    );
    assert!(
        fills_at(cx, frame).contains(&theme.elevated_surface),
        "{frame} is not filled with elevated_surface"
    );
    let (frame_at, row_at) = (
        cx.debug_bounds(frame).expect("the frame"),
        cx.debug_bounds(row).expect("the first row"),
    );
    let inset = super::tokens::BORDER_WIDTH + super::tokens::SPACE_4;
    assert_eq!(row_at.left() - frame_at.left(), inset, "{row} in {frame}");
}

/// Asserts a menu's rows draw `MenuRow`'s picker look under the pointer
/// (V119): the `checked` row keeps the selected fill, and the `other` row
/// takes the selected fill at half strength, which shows on the menu's
/// `elevated_surface` where `element_hover` would not.
pub(crate) fn assert_menu_row_hover(
    cx: &mut VisualTestContext,
    checked: &'static str,
    other: &'static str,
) {
    let theme = cx.update(|_, cx| cx.global::<Appearance>().theme.clone());
    let hover = super::tokens::tint(theme.element_selected, super::tokens::TINT_MENU_HOVER);
    assert_ne!(hover, theme.elevated_surface);
    for (row, want) in [(checked, theme.element_selected), (other, hover)] {
        let center = cx.debug_bounds(row).expect("the row").center();
        cx.simulate_mouse_move(center, None, Modifiers::none());
        redraw(cx);
        assert_eq!(fills_at(cx, row), [want], "{row} under the pointer");
    }
}

/// The solid fills among [`painted_at`]'s quads.
pub(crate) fn fills_at(cx: &mut VisualTestContext, selector: &'static str) -> Vec<gpui::Hsla> {
    painted_at(cx, selector)
        .iter()
        .filter_map(|quad| quad.background.as_solid())
        .filter(|fill| fill.a > 0.)
        .collect()
}
