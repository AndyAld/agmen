//! A clickable icon.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClickEvent, ElementId, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, RenderOnce, StatefulInteractiveElement as _, Styled as _, Window,
    WindowControlArea, div,
};

use super::{Color, Icon, IconSize};
use crate::appearance::{Appearance, Theme};
use crate::icons::UiIcon;
use crate::ui::tokens::{
    BUTTON_HOVER_OPACITY, DOCK_BUTTON, ICON_BUTTON_SIZE, RADIUS_SM, WINDOW_BUTTON_WIDTH, focus_ring,
};

/// What a click does.
pub(crate) type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// How the button is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum IconButtonStyle {
    /// The bare icon, with a hover fill.
    #[default]
    Ghost,
    /// On a surface fill, as the right dock's close button.
    Filled,
    /// The bare icon at the end of a `ListItem`, which fills with
    /// `element_hover` under the pointer: this one steps to
    /// `element_selected`, so it shows inside the hovered row.
    InRow,
    /// A Windows or Linux caption button: as tall as the title bar and 36 px
    /// wide, square cornered, as Zed draws them.
    Caption,
    /// The caption's close button, which hovers in Windows' red.
    CaptionClose,
}

/// The fill under the pointer.
fn hover_fill(style: IconButtonStyle, theme: &Theme) -> Hsla {
    match style {
        IconButtonStyle::CaptionClose => Theme::WINDOWS_CLOSE_HOVER.into(),
        IconButtonStyle::Ghost | IconButtonStyle::Filled | IconButtonStyle::Caption => {
            theme.element_hover
        }
        IconButtonStyle::InRow => theme.element_selected,
    }
}

#[derive(IntoElement)]
pub(crate) struct IconButton {
    id: ElementId,
    icon: UiIcon,
    color: Color,
    size: IconSize,
    style: IconButtonStyle,
    selected: bool,
    control_area: Option<WindowControlArea>,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for IconButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IconButton")
            .field("id", &self.id)
            .field("icon", &self.icon)
            .finish_non_exhaustive()
    }
}

impl IconButton {
    #[must_use]
    pub(crate) fn new(id: impl Into<ElementId>, icon: UiIcon) -> Self {
        Self {
            id: id.into(),
            icon,
            color: Color::Placeholder,
            size: IconSize::Medium,
            style: IconButtonStyle::Ghost,
            selected: false,
            control_area: None,
            on_click: None,
        }
    }

    #[must_use]
    pub(crate) fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    #[must_use]
    pub(crate) const fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    #[must_use]
    pub(crate) const fn style(mut self, style: IconButtonStyle) -> Self {
        self.style = style;
        self
    }

    /// Draws the button's on state, as a status bar panel button while its
    /// panel shows: a selected fill behind a bright icon.
    #[must_use]
    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Marks the button as a window control that the OS hit-tests, as
    /// Windows does for its caption buttons.
    #[must_use]
    pub(crate) const fn window_control_area(mut self, area: WindowControlArea) -> Self {
        self.control_area = Some(area);
        self
    }

    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let hover = match self.style {
            // The fill the button rests on, so its hover must differ from it.
            IconButtonStyle::Filled => theme.hover_over(theme.elevated_surface),
            _ => hover_fill(self.style, theme),
        };
        let selected_fill = theme.element_selected;
        let focus = theme.text_accent;
        let color = if self.selected {
            Color::Default
        } else {
            self.color
        };
        let selector = self.id.to_string();
        let caption = matches!(
            self.style,
            IconButtonStyle::Caption | IconButtonStyle::CaptionClose
        );
        div()
            .id(self.id)
            .debug_selector(|| selector)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .when(!caption, |this| this.rounded(RADIUS_SM))
            .when(self.style == IconButtonStyle::Ghost, |this| {
                this.size(ICON_BUTTON_SIZE)
            })
            .when(self.style == IconButtonStyle::Filled, |this| {
                this.bg(theme.elevated_surface)
                    .w(DOCK_BUTTON.width)
                    .h(DOCK_BUTTON.height)
            })
            .when(self.selected, |this| this.bg(selected_fill))
            .when(caption, |this| {
                this.w(WINDOW_BUTTON_WIDTH)
                    .h_full()
                    .occlude()
                    // Keep the title bar from starting a window move under
                    // the button.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            })
            .when(
                self.on_click.is_some() || self.control_area.is_some(),
                |this| {
                    this.hover(move |style| style.bg(hover)).active(|style| {
                        style
                            .bg(hover)
                            .opacity(BUTTON_HOVER_OPACITY * BUTTON_HOVER_OPACITY)
                    })
                },
            )
            .when_some(self.control_area, |this, area| {
                this.window_control_area(area)
            })
            .when_some(self.on_click, |this, handler| {
                // Caption buttons keep the arrow, as native ones do.
                this.when(!caption, |this| this.cursor_pointer())
                    .tab_index(0)
                    .focus_visible(move |style| style.bg(hover).shadow(focus_ring(focus)))
                    .on_click(handler)
            })
            .child(Icon::new(self.icon).color(color).size(self.size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{draw, rendered_size};
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn caption_buttons_fill_the_bar_height(cx: &mut TestAppContext) {
        // V89: Zed's caption buttons are 36 px wide and as tall as the bar.
        let cx = draw(cx, || {
            div()
                .h(px(40.))
                .flex()
                .child(
                    IconButton::new("caption", UiIcon::Minus)
                        .style(IconButtonStyle::Caption)
                        .window_control_area(WindowControlArea::Min),
                )
                .into_any_element()
        });
        let bounds = cx.debug_bounds("caption").unwrap();
        assert_eq!(bounds.size, gpui::size(WINDOW_BUTTON_WIDTH, px(40.)));
    }

    #[test]
    fn caption_close_hovers_in_windows_red() {
        let theme = crate::appearance::Appearance::builtin().theme;
        assert_eq!(
            hover_fill(IconButtonStyle::CaptionClose, &theme),
            Hsla::from(Theme::WINDOWS_CLOSE_HOVER)
        );
        for style in [
            IconButtonStyle::Ghost,
            IconButtonStyle::Filled,
            IconButtonStyle::Caption,
        ] {
            assert_eq!(hover_fill(style, &theme), theme.element_hover, "{style:?}");
        }
    }

    #[gpui::test]
    fn icon_button_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            IconButton::new("close", UiIcon::ChevronRight)
                .style(IconButtonStyle::Filled)
                .on_click(|_, _, _| {})
                .into_any_element()
        });
        // Mockup check V63: the dock's collapse button is 17 by 20.
        assert_eq!(size, gpui::size(px(17.), px(20.)));
    }

    #[gpui::test]
    fn filled_icon_button_hover_is_visible_when_the_theme_hover_equals_the_surface(
        cx: &mut TestAppContext,
    ) {
        let cx = crate::ui::test_support::draw(cx, || {
            IconButton::new("filled", UiIcon::ChevronRight)
                .style(IconButtonStyle::Filled)
                .on_click(|_, _, _| {})
                .into_any_element()
        });
        cx.update(|_, cx| {
            let theme = &mut cx.global_mut::<Appearance>().theme;
            theme.element_hover = theme.elevated_surface;
        });
        cx.simulate_mouse_move(
            gpui::point(px(900.), px(900.)),
            None,
            gpui::Modifiers::none(),
        );
        crate::ui::test_support::redraw(cx);
        let rest = crate::ui::test_support::fills_at(cx, "filled");
        let center = cx.debug_bounds("filled").unwrap().center();
        cx.simulate_mouse_move(center, None, gpui::Modifiers::none());
        crate::ui::test_support::redraw(cx);
        assert_ne!(crate::ui::test_support::fills_at(cx, "filled"), rest);
    }
}
