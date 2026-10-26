//! An on and off switch, as Settings' Follow Zed.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::ChangeHandler;
use crate::appearance::Appearance;
use crate::ui::tokens::{BORDER_WIDTH, SPACE_1, TOGGLE_HEIGHT, TOGGLE_KNOB, TOGGLE_WIDTH};

#[derive(IntoElement)]
pub(crate) struct Toggle {
    id: ElementId,
    on: bool,
    on_change: Option<ChangeHandler<bool>>,
}

impl std::fmt::Debug for Toggle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Toggle")
            .field("id", &self.id)
            .field("on", &self.on)
            .finish_non_exhaustive()
    }
}

impl Toggle {
    /// Tests find the toggle as `id` and its knob as `"{id}:knob"` with
    /// `debug_bounds`.
    #[must_use]
    pub(crate) fn new(id: impl Into<ElementId>, on: bool) -> Self {
        Self {
            id: id.into(),
            on,
            on_change: None,
        }
    }

    /// Called with the flipped value when the toggle is clicked.
    #[must_use]
    pub(crate) fn on_change(
        mut self,
        handler: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Toggle {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let (hover, focus) = (theme.element_hover, theme.text_accent);
        let on = self.on;
        let selector = self.id.to_string();
        let knob_selector = format!("{selector}:knob");
        // Off is drawn as Zed's off switch: an empty track with a border. On
        // keeps the border in the track color, so both states inset the knob
        // by the same 2 px the mockup's padding gives.
        let (track, border) = if on {
            (theme.text_accent, theme.text_accent)
        } else {
            (theme.background, theme.border)
        };
        div()
            .id(self.id)
            .debug_selector(|| selector)
            .tab_index(0)
            .flex()
            .flex_none()
            .items_center()
            .when(on, |this| this.justify_end())
            .w(TOGGLE_WIDTH)
            .h(TOGGLE_HEIGHT)
            .p(SPACE_1)
            .rounded_full()
            .border(BORDER_WIDTH)
            .border_color(border)
            .bg(track)
            .when(!on, |this| this.hover(move |style| style.bg(hover)))
            .focus_visible(move |style| style.border_color(focus))
            .when_some(self.on_change, |this, handler| {
                this.cursor_pointer()
                    .on_click(move |_, window, cx| handler(!on, window, cx))
            })
            // The mockup's knob is white; text_muted is the theme's brightest
            // text, so it reads the same and follows a light theme.
            .child(
                div()
                    .debug_selector(|| knob_selector)
                    .size(TOGGLE_KNOB)
                    .flex_none()
                    .rounded_full()
                    .bg(theme.text_muted),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::control_harness::{Reported, open, open_with_caller};
    use crate::ui::test_support::{click, rendered_size};
    use crate::ui::tokens::{TOGGLE_HEIGHT, TOGGLE_WIDTH};
    use gpui::TestAppContext;

    #[gpui::test]
    fn toggle_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || Toggle::new("follow-zed", false).into_any_element());
        assert_eq!((size.width, size.height), (TOGGLE_WIDTH, TOGGLE_HEIGHT));
    }

    #[gpui::test]
    fn toggle_reports_on_to_off(cx: &mut TestAppContext) {
        let reported = Reported::default();
        let sink = reported.clone();
        let cx = open(cx, move || {
            let sink = sink.clone();
            Toggle::new("follow-zed", true)
                .on_change(move |on, _, _| sink.set(Some(on)))
                .into_any_element()
        });
        click(cx, "follow-zed");
        assert_eq!(reported.get(), Some(false));
    }

    #[gpui::test]
    fn toggle_flips_off_to_on_and_moves_the_knob(cx: &mut TestAppContext) {
        let (caller, cx) = open_with_caller(cx, false, |on, set| {
            Toggle::new("follow-zed", on)
                .on_change(move |on, window, cx| set(on, window, cx))
                .into_any_element()
        });
        // The border and padding inset the knob 2 px from the track's edge.
        let inset = BORDER_WIDTH + SPACE_1;
        let track = cx.debug_bounds("follow-zed").expect("track");
        let knob = cx.debug_bounds("follow-zed:knob").expect("knob");
        assert_eq!(knob.left(), track.left() + inset);

        click(cx, "follow-zed");
        assert!(caller.read_with(cx, |caller, _| caller.flag));
        let knob = cx.debug_bounds("follow-zed:knob").expect("knob");
        assert_eq!(knob.right(), track.right() - inset);
    }
}
