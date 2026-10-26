//! The bar above a center tab's content (console.py `breadcrumb`, node 130):
//! path parts split by "›", the last one bright, with buttons at the right
//! edge.

use gpui::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::ui::tokens::{BORDER_WIDTH, BREADCRUMB_HEIGHT, SPACE_8, SPACE_14};

/// Tests find it as `breadcrumb`, and the separator before part `ix` as
/// `"breadcrumb:sep:{ix}"`.
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

impl RenderOnce for Breadcrumb {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let last = self.parts.len().saturating_sub(1);
        let mut bar = div()
            .debug_selector(|| "breadcrumb".into())
            .h(BREADCRUMB_HEIGHT)
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_8)
            .px(SPACE_14)
            .border_b(BORDER_WIDTH)
            .border_color(crate::ui::rule_color(cx));
        for (ix, part) in self.parts.into_iter().enumerate() {
            if ix > 0 {
                let selector = format!("breadcrumb:sep:{ix}");
                bar = bar.child(
                    div()
                        .debug_selector(|| selector)
                        .child(Label::new("›").size(LabelSize::Small).color(Color::Muted)),
                );
            }
            let color = if ix == last {
                Color::Default
            } else {
                Color::Placeholder
            };
            bar = bar.child(Label::new(part).code().color(color));
        }
        bar.child(div().flex_1()).children(self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{assert_breadcrumb_glyphs, init, redraw, rendered_size};
    use gpui::{Context, Render, TestAppContext, px};

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
