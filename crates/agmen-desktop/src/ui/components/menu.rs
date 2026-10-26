//! The frame of a dropdown's open menu: a [`Card`] drawn over the rows below
//! its trigger, which closes on a click outside. Dropdown and the scope chip
//! both use it (C249); each draws its own rows.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Deferred, InteractiveElement as _, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, anchored, deferred, div,
};

use super::Card;
use crate::ui::tokens::{DROPDOWN_MENU_MAX_HEIGHT, SPACE_2};

/// The open menu with `rows`, `width` wide or as wide as its rows. Tests find
/// the frame as `id` with `debug_bounds`. `on_close` runs on a mouse down
/// outside it.
pub(crate) fn menu(
    id: SharedString,
    width: Option<Pixels>,
    rows: impl IntoIterator<Item = AnyElement>,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> Deferred {
    let selector = id.to_string();
    // Deferred so the menu paints over the rows below its trigger.
    deferred(
        anchored().snap_to_window().child(
            div()
                .debug_selector(|| selector)
                .occlude()
                .mt(SPACE_2)
                .on_mouse_down_out(move |_, window, cx| on_close(window, cx))
                .child(
                    Card::new()
                        .when_some(width, |this, width| this.w(width))
                        .max_h(DROPDOWN_MENU_MAX_HEIGHT)
                        .p(SPACE_2)
                        .gap_0()
                        .child(
                            // The rows scroll inside the card's border.
                            div()
                                .id(id)
                                .flex()
                                .flex_col()
                                .min_h_0()
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                ),
        ),
    )
}
