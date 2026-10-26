//! A two line row: a name, a wrapping detail line under it, and an end slot,
//! such as a theme in Settings' browse list (V100).

use gpui::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{RADIUS_MD, SPACE_3, SPACE_8, SPACE_10};

#[derive(IntoElement)]
pub(crate) struct DetailRow {
    id: SharedString,
    name: SharedString,
    detail: SharedString,
    end: Option<AnyElement>,
}

impl std::fmt::Debug for DetailRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DetailRow")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl DetailRow {
    /// `id` names the row; tests find it with `debug_bounds`.
    #[must_use]
    pub(crate) fn new(
        id: impl Into<SharedString>,
        name: impl Into<SharedString>,
        detail: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            detail: detail.into(),
            end: None,
        }
    }

    /// Drawn at the row's right edge, such as a button.
    #[must_use]
    pub(crate) fn end(mut self, end: impl IntoElement) -> Self {
        self.end = Some(end.into_any_element());
        self
    }
}

impl RenderOnce for DetailRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let hover = cx.global::<Appearance>().theme.element_hover;
        let selector = self.id.to_string();
        div()
            .id(ElementId::Name(self.id))
            .debug_selector(|| selector)
            .w_full()
            .flex()
            .items_center()
            .gap(SPACE_10)
            .py(SPACE_3)
            .px(SPACE_8)
            .rounded(RADIUS_MD)
            .hover(move |style| style.bg(hover))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(Label::new(self.name))
                    .child(
                        Label::new(self.detail)
                            .size(LabelSize::Small)
                            .color(Color::Placeholder)
                            .wrap(),
                    ),
            )
            .children(self.end)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, px};

    use super::*;
    use crate::ui::test_support::draw;

    #[gpui::test]
    fn detail_wraps_and_the_end_sits_at_the_right(cx: &mut TestAppContext) {
        let cx = draw(cx, || {
            div()
                .w(px(240.))
                .child(
                    DetailRow::new("short", "Nord", "by Nord Folks")
                        .end(div().debug_selector(|| "end".into()).w(px(40.)).h(px(10.))),
                )
                .child(DetailRow::new(
                    "long",
                    "Nord",
                    "by Nord Folks · installed in Zed (nord) · license kept by Zed · https://github.com/example/nord",
                ))
                .into_any_element()
        });
        let short = cx.debug_bounds("short").unwrap();
        let long = cx.debug_bounds("long").unwrap();
        assert!(long.size.height > short.size.height, "{long:?} {short:?}");
        let end = cx.debug_bounds("end").unwrap();
        assert_eq!(end.right(), short.right() - px(8.));
    }
}
