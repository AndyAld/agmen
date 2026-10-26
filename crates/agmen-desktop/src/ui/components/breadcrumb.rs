//! The bar above a center tab's content (console.py `breadcrumb`, node 130):
//! path parts split by "›", the last one bright, with buttons at the right
//! edge.

use gpui::{
    AnyElement, App, Div, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::ui::tokens::{BORDER_WIDTH, BREADCRUMB_HEIGHT, SPACE_8, SPACE_14};

/// Tests find it as `breadcrumb`, part `ix` as `"breadcrumb:part:{ix}"`,
/// the separator before it as `"breadcrumb:sep:{ix}"` and the end element
/// as `breadcrumb:end`.
#[derive(IntoElement)]
pub(crate) struct Breadcrumb {
    parts: Vec<SharedString>,
    end: Option<AnyElement>,
}

impl std::fmt::Debug for Breadcrumb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Breadcrumb")
            .field("parts", &self.parts)
            .finish_non_exhaustive()
    }
}

impl Breadcrumb {
    #[must_use]
    pub(crate) fn new(parts: impl IntoIterator<Item = SharedString>) -> Self {
        Self {
            parts: parts.into_iter().collect(),
            end: None,
        }
    }

    /// Drawn at the bar's right edge, such as "Show ledger".
    #[must_use]
    pub(crate) fn end(mut self, end: impl IntoElement) -> Self {
        self.end = Some(end.into_any_element());
        self
    }
}

/// The "›" before part `ix`.
fn separator(ix: usize) -> Div {
    let selector = format!("breadcrumb:sep:{ix}");
    div()
        .debug_selector(|| selector)
        .flex_none()
        .child(Label::new("›").size(LabelSize::Small).color(Color::Muted))
}

/// Part `ix` in `color`, cut with an ellipsis when its box is narrower than
/// its text. The box is a block, so its full width is the text's.
fn part(ix: usize, text: SharedString, color: Color) -> Div {
    let selector = format!("breadcrumb:part:{ix}");
    div()
        .debug_selector(|| selector)
        .min_w_0()
        .overflow_hidden()
        .child(Label::new(text).code().color(color).truncate())
}

impl RenderOnce for Breadcrumb {
    /// V110: the bar clips to its width. When the parts and the end element
    /// do not fit, the middle parts give way first, then the first part;
    /// the last part and the end element keep their width.
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut parts = self.parts.into_iter().enumerate();
        let last = parts.next_back();
        // The first part keeps its width until the middle parts are gone
        // (it does not shrink, but it is never wider than the head), then
        // takes what is left of the head.
        let head = parts.next().map(|(ix, text)| {
            let middle = parts.fold(
                div().flex().min_w_0().overflow_hidden().items_center(),
                |middle, (ix, text)| {
                    middle
                        .child(separator(ix).ml(SPACE_8))
                        .child(part(ix, text, Color::Placeholder).ml(SPACE_8))
                },
            );
            div()
                .flex()
                .min_w_0()
                .overflow_hidden()
                .items_center()
                .child(part(ix, text, Color::Placeholder).flex_none().max_w_full())
                .child(middle)
        });
        let last = last.map(|(ix, text)| {
            let sep = (ix > 0).then(|| separator(ix));
            let selector = format!("breadcrumb:part:{ix}");
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(SPACE_8)
                .children(sep)
                .child(
                    div()
                        .debug_selector(|| selector)
                        .child(Label::new(text).code().color(Color::Default)),
                )
        });
        div()
            .debug_selector(|| "breadcrumb".into())
            .h(BREADCRUMB_HEIGHT)
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_8)
            .px(SPACE_14)
            .overflow_hidden()
            .border_b(BORDER_WIDTH)
            .border_color(crate::ui::rule_color(cx))
            .children(head)
            .children(last)
            .child(div().flex_1())
            .children(self.end.map(|end| {
                div()
                    .debug_selector(|| "breadcrumb:end".into())
                    .flex_none()
                    .child(end)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::{Button, ButtonStyle};
    use crate::ui::test_support::{assert_breadcrumb_glyphs, draw, init, redraw, rendered_size};
    use gpui::{Bounds, Context, Pixels, Render, TestAppContext, px};

    struct Host;

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            Breadcrumb::new(["andy/fx1".into(), "C015".into(), "main".into()])
        }
    }

    #[gpui::test]
    fn separators_are_a_text_glyph(cx: &mut TestAppContext) {
        init(cx);
        let (_, cx) = cx.add_window_view(|_, _| Host);
        redraw(cx);
        assert_breadcrumb_glyphs(cx, 3);
    }

    /// The most source files the walk below reads before it gives up.
    const MAX_SOURCE_FILES: usize = 2_000;

    #[test]
    fn only_the_component_defines_a_breadcrumb() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let components = src.join("ui").join("components");
        let mut dirs = vec![src];
        let mut read = 0;
        let mut others = Vec::new();
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.starts_with(&components) {
                    continue;
                }
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                read += 1;
                assert!(read <= MAX_SOURCE_FILES, "more source files than expected");
                let text = std::fs::read_to_string(&path).unwrap();
                let named = path.file_stem().is_some_and(|stem| stem == "breadcrumb");
                if named || text.contains("struct Breadcrumb") {
                    others.push(path);
                }
            }
        }
        assert!(read > 0, "no source files read");
        assert!(
            others.is_empty(),
            "breadcrumbs outside components: {others:?}"
        );
    }

    /// The bounds of the bar, its three parts and its end element, drawn
    /// `width` wide as the view:roles case: a built-in role's file with
    /// Duplicate to .agmen/roles at the end.
    struct Drawn {
        bar: Bounds<Pixels>,
        parts: [Bounds<Pixels>; 3],
        end: Bounds<Pixels>,
    }

    fn roles_crumb(cx: &mut TestAppContext, width: Pixels) -> Drawn {
        let cx = draw(cx, move || {
            div()
                .w(width)
                .child(
                    Breadcrumb::new(["roles".into(), "built-in".into(), "reviewer.xml".into()])
                        .end(
                            Button::new("role-duplicate", "Duplicate to .agmen/roles")
                                .style(ButtonStyle::Tinted),
                        ),
                )
                .into_any_element()
        });
        let mut at = |sel: &'static str| {
            cx.debug_bounds(sel)
                .unwrap_or_else(|| panic!("nothing drawn as {sel}"))
        };
        Drawn {
            bar: at("breadcrumb"),
            parts: [
                at("breadcrumb:part:0"),
                at("breadcrumb:part:1"),
                at("breadcrumb:part:2"),
            ],
            end: at("breadcrumb:end"),
        }
    }

    /// The bar's width with no room to spare: what its parts, separators,
    /// gaps, padding and end element take at their full width.
    fn snug_width(wide: &Drawn) -> Pixels {
        let spacer = wide.end.left() - wide.parts[2].right() - SPACE_8 * 2.;
        wide.bar.size.width - spacer
    }

    #[gpui::test]
    fn wide_breadcrumb_draws_every_part_whole(cx: &mut TestAppContext) {
        let wide = roles_crumb(cx, px(1200.));
        let mut cx2 = TestAppContext::single();
        let snug = roles_crumb(&mut cx2, snug_width(&wide));
        for (ix, (a, b)) in wide.parts.iter().zip(&snug.parts).enumerate() {
            assert_eq!(a.size.width, b.size.width, "part {ix}");
        }
        assert_eq!(wide.end.size, snug.end.size);
    }

    // V110: a narrow bar truncates the middle parts first.
    #[gpui::test]
    fn narrow_breadcrumb_truncates_the_middle_part_first(cx: &mut TestAppContext) {
        let wide = roles_crumb(cx, px(1200.));
        let mut cx2 = TestAppContext::single();
        let narrow = roles_crumb(&mut cx2, snug_width(&wide) - px(10.));
        assert_eq!(narrow.parts[0].size.width, wide.parts[0].size.width);
        assert_eq!(
            narrow.parts[1].size.width,
            wide.parts[1].size.width - px(10.)
        );
        assert_eq!(narrow.parts[2].size.width, wide.parts[2].size.width);
        assert_eq!(narrow.end.size, wide.end.size);
        assert!(narrow.end.right() <= narrow.bar.right() - SPACE_14);
    }

    // V110: once the middle part is gone, the first part gives way.
    #[gpui::test]
    fn narrower_breadcrumb_then_truncates_the_first_part(cx: &mut TestAppContext) {
        let wide = roles_crumb(cx, px(1200.));
        // The middle part, its separator and their gaps.
        let middle = wide.parts[1].right() - wide.parts[0].right();
        let mut cx2 = TestAppContext::single();
        let narrow = roles_crumb(&mut cx2, snug_width(&wide) - middle - px(20.));
        assert_eq!(narrow.parts[1].size.width, px(0.));
        assert_eq!(
            narrow.parts[0].size.width,
            wide.parts[0].size.width - px(20.)
        );
        assert_eq!(narrow.parts[2].size.width, wide.parts[2].size.width);
        assert_eq!(narrow.end.size, wide.end.size);
    }

    // V110, the view:roles case at 350 px: the first and middle parts give
    // way, nothing draws past the bar, and the last part and the end element
    // stay whole.
    #[gpui::test]
    fn breadcrumb_at_350_px_keeps_the_last_part_and_end_whole(cx: &mut TestAppContext) {
        let wide = roles_crumb(cx, px(1200.));
        let mut cx2 = TestAppContext::single();
        let narrow = roles_crumb(&mut cx2, px(350.));
        assert_eq!(narrow.bar.size.width, px(350.));
        assert!(narrow.parts[0].size.width < wide.parts[0].size.width);
        assert_eq!(narrow.parts[1].size.width, px(0.));
        assert_eq!(narrow.parts[2].size.width, wide.parts[2].size.width);
        assert_eq!(narrow.end.size, wide.end.size);
        assert!(narrow.parts[0].left() >= narrow.bar.left() + SPACE_14);
        assert!(narrow.parts[0].right() <= narrow.parts[2].left());
        assert!(narrow.parts[2].right() <= narrow.end.left());
        assert!(narrow.end.right() <= narrow.bar.right());
    }

    #[gpui::test]
    fn breadcrumb_keeps_its_height_with_an_end_button(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            Breadcrumb::new(["pr-3331".into(), "chat".into()])
                .end(div().w(px(40.)).h(px(20.)))
                .into_any_element()
        });
        assert_eq!(size.height, BREADCRUMB_HEIGHT);
        assert!(size.width > px(40.) + SPACE_14 * 2.);
    }
}
