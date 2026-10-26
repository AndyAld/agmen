//! The model a message ran on, in its head row (node 3887): "◆ opus ·
//! high" on the selected fill, the diamond in the accent color and the
//! name in the code font, dimmed.

use gpui::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{RADIUS_SM, SPACE_1, SPACE_4, SPACE_6};

#[derive(Debug, IntoElement)]
pub(crate) struct ModelTag {
    label: SharedString,
}

impl ModelTag {
    #[must_use]
    pub(crate) fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

impl RenderOnce for ModelTag {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_4)
            .py(SPACE_1)
            .px(SPACE_6)
            .rounded(RADIUS_SM)
            .bg(theme.element_selected)
            .child(Label::new("◆").size(LabelSize::XSmall).color(Color::Accent))
            .child(Label::new(self.label).code().color(Color::Placeholder))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn model_tag_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || ModelTag::new("opus · high").into_any_element());
        assert!(size.width > px(60.) && size.height > px(10.));
    }
}
