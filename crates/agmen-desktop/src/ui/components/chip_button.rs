//! A chip that toggles a popover owned by its caller.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, Div, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    RenderOnce, SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled, Window,
    canvas, div,
};

use super::{ChangeHandler, Chip, Color, Icon, IconSize};
use crate::appearance::Appearance;
use crate::icons::UiIcon;

/// Stateless trigger. Menus keep their caller's anchors and dismissal
/// handlers; an open trigger never installs a mouse click handler (C81).
#[derive(IntoElement)]
pub(crate) struct ChipButton {
    id: SharedString,
    selector: SharedString,
    chip: Chip,
    open: bool,
    caret: bool,
    frame: Div,
    on_toggle: Option<ChangeHandler<bool>>,
    menu: Option<AnyElement>,
    on_bounds: Option<Box<dyn Fn(Bounds<Pixels>)>>,
}

impl std::fmt::Debug for ChipButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChipButton")
            .field("id", &self.id)
            .field("chip", &self.chip)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

impl ChipButton {
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>, chip: Chip) -> Self {
        let id = id.into();
        Self {
            selector: id.clone(),
            id,
            chip,
            open: false,
            caret: false,
            frame: div().relative(),
            on_toggle: None,
            menu: None,
            on_bounds: None,
        }
    }

    /// Keeps an existing view's debug selector while sharing the trigger.
    #[must_use]
    pub(crate) fn debug_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.selector = selector.into();
        self
    }

    #[must_use]
    pub(crate) const fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    #[must_use]
    pub(crate) const fn caret(mut self, caret: bool) -> Self {
        self.caret = caret;
        self
    }

    #[must_use]
    pub(crate) fn on_toggle(
        mut self,
        handler: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(std::rc::Rc::new(handler));
        self
    }

    /// An anchored, deferred popover with the caller's existing placement.
    #[must_use]
    pub(crate) fn menu(mut self, menu: Option<AnyElement>) -> Self {
        self.menu = menu;
        self
    }

    #[must_use]
    pub(crate) fn min_width(mut self, width: Pixels) -> Self {
        self.frame = self.frame.min_w(width);
        self
    }

    #[must_use]
    pub(crate) fn on_bounds(mut self, handler: impl Fn(Bounds<Pixels>) + 'static) -> Self {
        self.on_bounds = Some(Box::new(handler));
        self
    }

    fn caret_icon() -> Icon {
        Icon::new(UiIcon::ChevronDown)
            .size(IconSize::Small)
            .color(Color::Placeholder)
    }
}

impl Styled for ChipButton {
    fn style(&mut self) -> &mut StyleRefinement {
        self.frame.style()
    }
}

impl RenderOnce for ChipButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let caret_selector = format!("{}-caret", self.selector);
        let chip = self.chip.when(self.caret, |chip| {
            chip.trailing(
                div()
                    .debug_selector(|| caret_selector)
                    .flex_none()
                    .child(Self::caret_icon()),
            )
        });
        let mut surface = chip.surface(cx);
        let theme = &cx.global::<Appearance>().theme;
        let rest = surface
            .style()
            .background
            .as_ref()
            .and_then(gpui::Fill::color)
            .and_then(|background| background.as_solid())
            .unwrap_or(theme.elevated_surface);
        let hover = theme.hover_over(rest);
        let surface_selector = format!("chip-button:{}", self.id);
        let open = self.open;
        let selector = self.selector.to_string();
        // One element takes the click, the keys, focus and the fill, so
        // hover and keyboard focus change the chip's own background.
        let trigger = surface
            .id("trigger")
            .debug_selector(|| surface_selector)
            .when_some(self.on_toggle.clone(), |surface, handler| {
                let click = handler.clone();
                surface
                    .tab_index(0)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .focus_visible(move |style| style.bg(hover))
                    // The menu closes on mouse down outside. Installing a
                    // click while open would reopen it on mouse up (C81).
                    .when(!open, |surface| {
                        surface.on_click(move |_, window, cx| click(true, window, cx))
                    })
                    .on_key_down(move |event, window, cx| {
                        let key = &event.keystroke;
                        if open
                            && !event.is_held
                            && !key.modifiers.modified()
                            && matches!(key.key.as_str(), "enter" | "space")
                        {
                            // Closed triggers use GPUI's key-up click. Open
                            // ones have no click handler, so close here.
                            window.prevent_default();
                            cx.stop_propagation();
                            handler(false, window, cx);
                        }
                    })
            });
        self.frame
            .id(self.id)
            .debug_selector(|| selector)
            .when_some(self.on_toggle.filter(|_| open), |frame, handler| {
                frame.on_key_down(move |event, window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        handler(false, window, cx);
                    }
                })
            })
            .child(trigger)
            .when(open, |frame| frame.children(self.menu))
            .when_some(self.on_bounds, |frame, handler| {
                frame.child(
                    canvas(move |bounds, _, _| handler(bounds), |_, (), _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                )
            })
    }
}

#[cfg(test)]
#[path = "chip_button_tests.rs"]
mod tests;
