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

/// How far right anything inside the element `selector` can show: the
/// right edge of the content mask on the quads the last frame painted
/// starting inside its bounds. `debug_bounds` reports a shrunken box, not
/// a child drawn past it; gpui clips that child's fill and its glyphs to
/// this mask. The element must paint a quad (a fill or a border); `None`
/// when it painted none.
pub(crate) fn clip_right(
    cx: &mut VisualTestContext,
    selector: &'static str,
) -> Option<gpui::Pixels> {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("nothing is drawn with selector {selector}"));
    cx.update(|window, _| {
        let scale = window.scale_factor();
        window
            .painted_quads()
            .into_iter()
            .filter(|quad| {
                let x = gpui::px(quad.bounds.origin.x.0 / scale);
                let y = gpui::px(quad.bounds.origin.y.0 / scale);
                x >= bounds.left() && x < bounds.right() && y >= bounds.top() && y < bounds.bottom()
            })
            .map(|quad| gpui::px(quad.content_mask.bounds.right().0 / scale))
            .reduce(gpui::Pixels::max)
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

/// The least contrast a menu row's hover fill keeps against the menu's
/// `elevated_surface`. The mockup's checked row, `element_selected` on the
/// menu (#3c3836 on #282828), is 1.27:1; the old half-strength hover was
/// 1.12:1 and hard to see.
pub(crate) const MIN_MENU_HOVER_CONTRAST: f32 = 1.25;

/// The WCAG contrast ratio between `fill` painted over `under` and `under`
/// itself, blending `fill`'s alpha as the menu draws it.
pub(crate) fn contrast_over(fill: gpui::Hsla, under: gpui::Hsla) -> f32 {
    let (top, base) = (fill.to_rgb(), under.to_rgb());
    let blend = |a: f32, b: f32| a * top.a + b * (1. - top.a);
    let luminance = |r: f32, g: f32, b: f32| {
        let linear = |c: f32| {
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    };
    let shown = luminance(
        blend(top.r, base.r),
        blend(top.g, base.g),
        blend(top.b, base.b),
    );
    let menu = luminance(base.r, base.g, base.b);
    (shown.max(menu) + 0.05) / (shown.min(menu) + 0.05)
}

/// Asserts a menu's rows draw `MenuRow`'s picker look under the pointer
/// (V119): the `checked` row keeps the selected fill, and the `other` row
/// takes a fill that shows on the menu's `elevated_surface` by at least
/// [`MIN_MENU_HOVER_CONTRAST`].
pub(crate) fn assert_menu_row_hover(
    cx: &mut VisualTestContext,
    checked: &'static str,
    other: &'static str,
) {
    let theme = cx.update(|_, cx| cx.global::<Appearance>().theme.clone());
    for row in [checked, other] {
        let center = cx.debug_bounds(row).expect("the row").center();
        cx.simulate_mouse_move(center, None, Modifiers::none());
        redraw(cx);
        let fills = fills_at(cx, row);
        let [fill] = fills.as_slice() else {
            panic!("{row} under the pointer paints {fills:?}");
        };
        if row == checked {
            assert_eq!(
                *fill, theme.element_selected,
                "{row} keeps the selected fill"
            );
        }
        let contrast = contrast_over(*fill, theme.elevated_surface);
        assert!(
            contrast >= MIN_MENU_HOVER_CONTRAST,
            "{row} under the pointer is {contrast:.2}:1 on the menu"
        );
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

/// Asserts `height` is `padding` plus whole lines of `line`.
pub(crate) fn assert_lines(height: Pixels, padding: Pixels, line: Pixels) {
    let lines = (height - padding) / line;
    assert!(
        lines >= 1. && (lines - lines.round()).abs() < 0.01,
        "{height:?} is {padding:?} and {lines} lines of {line:?}"
    );
}

/// Text with the tallest and deepest glyphs a label shows: Ë's dots, the
/// descenders of g, j, p, q and y, and the bar (C287).
#[cfg(target_os = "macos")]
pub(crate) const TALL_AND_DEEP: &str = "Ëgjpqy|";

/// Where a line of `text` in `font` at `size` paints in a `line_height`
/// box whose top is `top`, as gpui's paint places it.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ink {
    /// The top and bottom of the font: its ascent above the baseline and
    /// its descent below.
    pub(crate) font: (Pixels, Pixels),
    /// The top and bottom of the glyphs themselves.
    pub(crate) glyphs: (Pixels, Pixels),
}

#[cfg(target_os = "macos")]
impl Ink {
    /// Shapes `text` and reads each glyph's bounds from the text system.
    pub(crate) fn of(
        window: &Window,
        text: &'static str,
        font: &gpui::Font,
        size: Pixels,
        top: Pixels,
        line_height: Pixels,
    ) -> Self {
        let run = gpui::TextRun {
            len: text.len(),
            font: font.clone(),
            color: gpui::Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(text.into(), size, &[run], None);
        let id = window.text_system().resolve_font(font);
        let (mut above, mut below) = (gpui::px(0.), gpui::px(0.));
        for ch in text.chars() {
            // The bounds are y up from the baseline.
            let glyph = window
                .text_system()
                .typographic_bounds(id, size, ch)
                .unwrap_or_else(|err| panic!("no bounds for {ch:?}: {err}"));
            above = above.max(glyph.origin.y + glyph.size.height);
            below = below.max(-glyph.origin.y);
        }
        // gpui's paint: half the spare height, then the ascent.
        let baseline = top + (line_height - line.ascent - line.descent) / 2. + line.ascent;
        Self {
            font: (baseline - line.ascent, baseline + line.descent),
            glyphs: (baseline - above, baseline + below),
        }
    }
}

/// `bounds` in window pixels, from the scaled pixels of the scene.
#[cfg(target_os = "macos")]
pub(crate) fn unscaled(
    bounds: gpui::Bounds<gpui::ScaledPixels>,
    scale: f32,
) -> gpui::Bounds<Pixels> {
    gpui::Bounds::new(
        gpui::point(
            gpui::px(bounds.origin.x.0 / scale),
            gpui::px(bounds.origin.y.0 / scale),
        ),
        gpui::size(
            gpui::px(bounds.size.width.0 / scale),
            gpui::px(bounds.size.height.0 / scale),
        ),
    )
}
