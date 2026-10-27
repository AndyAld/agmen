//! A one-line tooltip on the elevated surface, as a commit row's author and
//! time.

use gpui::{
    Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window, div,
};

use super::{Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{BORDER_WIDTH, RADIUS_MD, SPACE_2, SPACE_6};

/// Made by an element's `.tooltip(..)` builder, as `cx.new(|_|
/// Tooltip::new(text)).into()`.
#[derive(Debug)]
pub(crate) struct Tooltip(SharedString);

impl Tooltip {
    pub(crate) const fn new(text: SharedString) -> Self {
        Self(text)
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        div()
            .py(SPACE_2)
            .px(SPACE_6)
            .rounded(RADIUS_MD)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .bg(theme.elevated_surface)
            .child(Label::new(self.0.clone()).size(LabelSize::Small))
    }
}
