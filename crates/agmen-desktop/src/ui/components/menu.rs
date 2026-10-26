//! The one frame of an open popover menu (node 1802): a card in the theme's
//! `border` on `elevated_surface` with the popover's drop shadow, an
//! optional heading over a 4 px padded list of rows that scrolls past its
//! height cap, and an optional foot. It draws over everything below it and
//! closes on a click outside. Dropdown, the scope chip and the switch agent
//! menu use it; each draws its own rows.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, AnyElement, App, Bounds, Div, InteractiveElement, Interactivity, IntoElement,
    ParentElement, Pixels, Point, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, anchored, canvas, deferred, div,
};

use super::Card;
use crate::appearance::Appearance;
use crate::ui::tokens::{
    BORDER_WIDTH, DROPDOWN_MENU_MAX_HEIGHT, POPOVER_SHADOW_BLUR, POPOVER_SHADOW_OFFSET, SPACE_2,
    SPACE_4, SPACE_8, SPACE_10, SPACE_12, drop_shadow,
};

type CloseHandler = Box<dyn Fn(&mut Window, &mut App)>;
type BoundsHandler = Box<dyn Fn(Bounds<Pixels>)>;

/// An open menu. Its caller adds rows as children, owns whether it is open
/// and closes it in [`Menu::on_close`]. Focus, key context and actions set
/// through [`InteractiveElement`] land on the frame.
#[derive(IntoElement)]
pub(crate) struct Menu {
    id: SharedString,
    frame: Div,
    width: Option<Pixels>,
    max_height: Pixels,
    place: Option<(Anchor, Point<Pixels>)>,
    header: Option<AnyElement>,
    rows: Vec<AnyElement>,
    footer: Option<AnyElement>,
    on_close: Option<CloseHandler>,
    on_bounds: Option<BoundsHandler>,
}

impl std::fmt::Debug for Menu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Menu")
            .field("id", &self.id)
            .field("width", &self.width)
            .field("max_height", &self.max_height)
            .field("rows", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl Menu {
    /// `id` names the menu; tests find the frame as `id` with
    /// `debug_bounds`. It opens 2 px under the element that holds it, as
    /// wide as its rows, and scrolls past [`DROPDOWN_MENU_MAX_HEIGHT`].
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            frame: div(),
            width: None,
            max_height: DROPDOWN_MENU_MAX_HEIGHT,
            place: None,
            header: None,
            rows: Vec::new(),
            footer: None,
            on_close: None,
            on_bounds: None,
        }
    }

    /// Makes the menu `width` wide instead of as wide as its rows.
    #[must_use]
    pub(crate) const fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }

    /// Cuts the menu to `height`; its rows scroll in what is left.
    #[must_use]
    pub(crate) const fn max_height(mut self, height: Pixels) -> Self {
        self.max_height = height;
        self
    }

    /// Puts the menu's `anchor` corner `offset` from the top left of the
    /// element that holds it, instead of 2 px under it.
    #[must_use]
    pub(crate) const fn place(mut self, anchor: Anchor, offset: Point<Pixels>) -> Self {
        self.place = Some((anchor, offset));
        self
    }

    /// A heading over the rows, ruled off from them.
    #[must_use]
    pub(crate) fn header(mut self, header: impl IntoElement) -> Self {
        self.header = Some(header.into_any_element());
        self
    }

    /// A foot under the rows, ruled off from them.
    #[must_use]
    pub(crate) fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    /// Called on a mouse down outside the menu.
    #[must_use]
    pub(crate) fn on_close(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Box::new(handler));
        self
    }

    /// Called with the frame's window bounds each time it is drawn, for a
    /// caller that places the menu by its height.
    #[must_use]
    pub(crate) fn on_bounds(mut self, handler: impl Fn(Bounds<Pixels>) + 'static) -> Self {
        self.on_bounds = Some(Box::new(handler));
        self
    }
}

impl ParentElement for Menu {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.rows.extend(elements);
    }
}

impl InteractiveElement for Menu {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.frame.interactivity()
    }
}

/// The frame's card: node 1802's border and fill (Card's), no padding of
/// its own, and the popover's drop shadow.
fn card(width: Option<Pixels>, max_height: Pixels) -> Card {
    Card::new()
        .when_some(width, |card, width| card.w(width))
        .max_h(max_height)
        .p_0()
        .gap_0()
        .overflow_hidden()
        .shadow(vec![drop_shadow(
            POPOVER_SHADOW_OFFSET,
            POPOVER_SHADOW_BLUR,
        )])
}

impl RenderOnce for Menu {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let border = cx.global::<Appearance>().theme.border;
        let selector = self.id.to_string();
        let header = self.header.map(|header| {
            div()
                .w_full()
                .flex_none()
                .py(SPACE_8)
                .px(SPACE_12)
                .border_b(BORDER_WIDTH)
                .border_color(border)
                .child(header)
        });
        let footer = self.footer.map(|footer| {
            div()
                .w_full()
                .flex_none()
                .pt(SPACE_8)
                .px(SPACE_12)
                .pb(SPACE_10)
                .border_t(BORDER_WIDTH)
                .border_color(border)
                .child(footer)
        });
        let list = div()
            .id(self.id)
            .w_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p(SPACE_4)
            .children(self.rows);
        let frame = self
            .frame
            .debug_selector(|| selector)
            .occlude()
            .when(self.place.is_none(), |frame| frame.mt(SPACE_2))
            .when_some(self.on_close, |frame, handler| {
                frame.on_mouse_down_out(move |_, window, cx| handler(window, cx))
            })
            .child(
                card(self.width, self.max_height)
                    .children(header)
                    .child(list)
                    .children(footer),
            )
            .when_some(self.on_bounds, |frame, handler| {
                frame.child(
                    canvas(move |bounds, _, _| handler(bounds), |_, (), _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                )
            });
        let anchored = anchored().snap_to_window();
        let anchored = match self.place {
            Some((anchor, offset)) => anchored.anchor(anchor).offset(offset),
            None => anchored,
        };
        // Deferred so the menu paints over everything below it.
        deferred(anchored.child(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::Label;
    use crate::ui::test_support::{assert_menu_frame, draw};
    use crate::ui::tokens::SHADOW_ALPHA;
    use gpui::{point, px};

    fn rows(count: usize) -> impl Iterator<Item = AnyElement> {
        (0..count).map(|i| {
            div()
                .debug_selector(move || format!("menu:{i}"))
                .h(px(30.))
                .child(Label::new(format!("row {i}")))
                .into_any_element()
        })
    }

    #[test]
    fn the_frame_casts_the_popover_shadow() {
        // Node 1802: one drop shadow, #000000aa, 8 px down, a 24 px blur.
        // gpui's tests cannot read painted shadows, so this reads the card.
        let mut card = card(None, DROPDOWN_MENU_MAX_HEIGHT);
        let shadows = card.style().box_shadow.clone().unwrap();
        assert_eq!(shadows.len(), 1, "{shadows:?}");
        assert_eq!(shadows[0].offset, point(px(0.), POPOVER_SHADOW_OFFSET));
        assert_eq!(shadows[0].blur_radius, POPOVER_SHADOW_BLUR);
        assert_eq!(shadows[0].spread_radius, px(0.));
        assert!((shadows[0].color.a - SHADOW_ALPHA).abs() < 0.001);
    }

    #[gpui::test]
    fn the_frame_is_node_1802s(cx: &mut gpui::TestAppContext) {
        // V118 and C255: tests find the frame by its id; border, fill and
        // the list's 4 px padding as drawn.
        let cx = draw(cx, || {
            Menu::new("menu")
                .width(px(200.))
                .children(rows(3))
                .into_any_element()
        });
        assert_menu_frame(cx, "menu", "menu:0");
        let (frame, row) = (
            cx.debug_bounds("menu").unwrap(),
            cx.debug_bounds("menu:0").unwrap(),
        );
        assert_eq!(frame.size.width, px(200.));
        assert_eq!(row.top() - frame.top(), BORDER_WIDTH + SPACE_4);
        assert_eq!(row.size.width, px(200.) - (BORDER_WIDTH + SPACE_4) * 2.);
    }

    #[gpui::test]
    fn a_long_menu_stops_at_its_cap(cx: &mut gpui::TestAppContext) {
        let cx = draw(cx, || {
            Menu::new("menu")
                .max_height(px(100.))
                .children(rows(20))
                .into_any_element()
        });
        assert_eq!(cx.debug_bounds("menu").unwrap().size.height, px(100.));
    }
}
