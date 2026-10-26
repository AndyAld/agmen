//! The frame of a dropdown's open menu: a panel drawn over the rows below
//! its trigger, which closes on a click outside. Dropdown and the scope chip
//! both use it (C249); each draws its own rows.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Deferred, InteractiveElement as _, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, anchored, deferred, div,
};

use crate::appearance::Appearance;
use crate::ui::tokens::{BORDER_WIDTH, DROPDOWN_MENU_MAX_HEIGHT, RADIUS_LG, SPACE_2};

/// The open menu with `rows`, `width` wide or as wide as its rows. Tests find
/// it as `id`. `on_close` runs on a mouse down outside it.
pub(crate) fn menu(
    id: SharedString,
    width: Option<Pixels>,
    rows: impl IntoIterator<Item = AnyElement>,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> Deferred {
    let theme = &cx.global::<Appearance>().theme;
    let selector = id.to_string();
    // Deferred so the menu paints over the rows below its trigger.
    deferred(
        anchored().snap_to_window().child(
            div()
                .id(id)
                .debug_selector(|| selector)
                .occlude()
                .mt(SPACE_2)
                .when_some(width, |this, width| this.w(width))
                .max_h(DROPDOWN_MENU_MAX_HEIGHT)
                .overflow_y_scroll()
                .p(SPACE_2)
                .rounded(RADIUS_LG)
                .border(BORDER_WIDTH)
                .border_color(theme.border_variant)
                .bg(theme.elevated_surface)
                .on_mouse_down_out(move |_, window, cx| on_close(window, cx))
                .children(rows),
        ),
    )
}
