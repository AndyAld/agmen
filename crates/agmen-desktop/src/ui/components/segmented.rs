//! A row of options with one selected, as Settings' Light, Dark, System and
//! the model picker's effort row.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::{ChangeHandler, Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{BORDER_WIDTH, RADIUS_MD, SPACE_3, SPACE_10, SPACE_12, TINT_SEGMENT, tint};

#[derive(IntoElement)]
pub(crate) struct Segmented {
    id: SharedString,
    options: Vec<SharedString>,
    selected: usize,
    code: bool,
    on_change: Option<ChangeHandler<usize>>,
}

impl std::fmt::Debug for Segmented {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segmented")
            .field("id", &self.id)
            .field("options", &self.options)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl Segmented {
    /// `id` names the control; tests find it as `"{id}"`, option `i` as
    /// `"{id}:{i}"` and its label as `"{id}:{i}:label"`, with `debug_bounds`.
    #[must_use]
    pub(crate) fn new(
        id: impl Into<SharedString>,
        options: impl IntoIterator<Item = impl Into<SharedString>>,
        selected: usize,
    ) -> Self {
        Self {
            id: id.into(),
            options: options.into_iter().map(Into::into).collect(),
            selected,
            code: false,
            on_change: None,
        }
    }

    /// Sets the options in the code font with wider padding, as the effort row.
    #[must_use]
    pub(crate) const fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// Called with the index of an unselected option when it is clicked.
    #[must_use]
    pub(crate) fn on_change(
        mut self,
        handler: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Segmented {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let hover = theme.element_hover;
        let last = self.options.len().saturating_sub(1);
        let root_selector = self.id.to_string();
        let segments = self.options.into_iter().enumerate().map(|(index, option)| {
            let selected = index == self.selected;
            let selector = format!("{}:{index}", self.id);
            let label_selector = format!("{selector}:label");
            let label = Label::new(option)
                .size(LabelSize::Small)
                .color(if selected {
                    Color::Accent
                } else {
                    Color::Placeholder
                });
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(|| selector)
                .tab_index(0)
                .flex()
                .flex_none()
                .items_center()
                .py(SPACE_3)
                .px(if self.code { SPACE_12 } else { SPACE_10 })
                .when(index < last, |this| {
                    this.border_r(BORDER_WIDTH).border_color(theme.border)
                })
                // Segments share their borders, so keyboard focus shows as the
                // hover fill rather than a ring.
                .focus_visible(move |style| style.bg(hover))
                .map(|this| {
                    if selected {
                        this.bg(tint(theme.text_accent, TINT_SEGMENT))
                    } else {
                        this.hover(move |style| style.bg(hover)).when_some(
                            self.on_change.clone(),
                            |this, handler| {
                                this.cursor_pointer()
                                    .on_click(move |_, window, cx| handler(index, window, cx))
                            },
                        )
                    }
                })
                .child(div().debug_selector(|| label_selector).child(if self.code {
                    label.code()
                } else {
                    label
                }))
        });
        div()
            .debug_selector(|| root_selector)
            .flex()
            .flex_none()
            .rounded(RADIUS_MD)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .overflow_hidden()
            .children(segments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::control_harness::{Reported, open};
    use crate::ui::test_support::click;
    use crate::ui::tokens::BORDER_WIDTH;
    use gpui::{Pixels, TestAppContext};

    /// The side padding of the last option, which draws no divider.
    fn side_padding(cx: &mut TestAppContext, code: bool) -> Pixels {
        let cx = open(cx, move || {
            let effort = Segmented::new("effort", ["low", "high"], 0);
            if code { effort.code() } else { effort }.into_any_element()
        });
        let option = cx.debug_bounds("effort:1").expect("option").size.width;
        let label = cx.debug_bounds("effort:1:label").expect("label").size.width;
        (option - label) / 2.
    }

    #[gpui::test]
    fn ui_options_pad_10(cx: &mut TestAppContext) {
        assert_eq!(side_padding(cx, false), SPACE_10);
    }

    #[gpui::test]
    fn code_options_pad_12(cx: &mut TestAppContext) {
        assert_eq!(side_padding(cx, true), SPACE_12);
    }

    #[gpui::test]
    fn segmented_reports_the_clicked_option(cx: &mut TestAppContext) {
        let reported = Reported::default();
        let sink = reported.clone();
        let cx = open(cx, move || {
            let sink = sink.clone();
            Segmented::new("mode", ["Light", "Dark", "System"], 2)
                .on_change(move |index, _, _| sink.set(Some(index)))
                .into_any_element()
        });
        click(cx, "mode:0");
        assert_eq!(reported.get(), Some(0));
    }

    #[gpui::test]
    fn segmented_draws_no_divider_after_the_last_option(cx: &mut TestAppContext) {
        let cx = open(cx, || {
            Segmented::new("same", ["A", "A", "A"], 1).into_any_element()
        });
        let first = cx.debug_bounds("same:0").expect("first option").size.width;
        let last = cx.debug_bounds("same:2").expect("last option").size.width;
        assert_eq!(first - last, BORDER_WIDTH);
    }
}
