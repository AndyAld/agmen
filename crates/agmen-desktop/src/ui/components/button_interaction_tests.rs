//! Rendered keyboard, pointer and surface contracts shared by both button types.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, IntoElement as _, Modifiers, MouseButton, ParentElement as _, Styled as _,
    TestAppContext, div, px,
};

use super::{Button, ButtonStyle, IconButton, IconButtonStyle, IconSize};
use crate::icons::UiIcon;
use crate::ui::test_support::{draw, fills_at, painted_at, redraw};

const BUTTON_STYLES: [ButtonStyle; 9] = [
    ButtonStyle::Outlined,
    ButtonStyle::Filled,
    ButtonStyle::Primary,
    ButtonStyle::Tinted,
    ButtonStyle::Plain,
    ButtonStyle::Secondary,
    ButtonStyle::Danger,
    ButtonStyle::Bordered,
    ButtonStyle::Inset,
];

const ICON_STYLES: [IconButtonStyle; 5] = [
    IconButtonStyle::Ghost,
    IconButtonStyle::Filled,
    IconButtonStyle::InRow,
    IconButtonStyle::Caption,
    IconButtonStyle::CaptionClose,
];

fn styles() -> impl Iterator<Item = (Option<ButtonStyle>, Option<IconButtonStyle>)> {
    BUTTON_STYLES
        .into_iter()
        .map(|style| (Some(style), None))
        .chain(ICON_STYLES.into_iter().map(|style| (None, Some(style))))
}

fn button(
    style: (Option<ButtonStyle>, Option<IconButtonStyle>),
    clicks: Rc<Cell<usize>>,
) -> AnyElement {
    let on_click = move |_: &gpui::ClickEvent, _: &mut gpui::Window, _: &mut gpui::App| {
        clicks.set(clicks.get() + 1);
    };
    if let Some(style) = style.0 {
        Button::new("button", "Act")
            .style(style)
            .on_click(on_click)
            .into_any_element()
    } else {
        IconButton::new("button", UiIcon::Close)
            .style(style.1.unwrap())
            .on_click(on_click)
            .into_any_element()
    }
}

#[gpui::test]
fn buttons_take_focus_with_tab_and_fire_on_enter_and_space(cx: &mut TestAppContext) {
    for style in styles() {
        let clicks = Rc::new(Cell::new(0));
        let seen = clicks.clone();
        let cx = crate::ui::test_support::draw_with_tab(cx, move || button(style, seen.clone()));
        crate::ui::test_support::press(cx, "tab");
        crate::ui::test_support::press(cx, "enter");
        assert_eq!(clicks.get(), 1, "{style:?}: Enter after Tab");
        crate::ui::test_support::press(cx, "space");
        assert_eq!(clicks.get(), 2, "{style:?}: Space after Tab");
    }
}

#[gpui::test]
fn focused_buttons_show_a_visible_focus_style_for_every_style(cx: &mut TestAppContext) {
    for style in styles() {
        let cx = crate::ui::test_support::draw_with_tab(cx, move || {
            button(style, Rc::new(Cell::new(0)))
        });
        cx.simulate_mouse_move(gpui::point(px(900.), px(900.)), None, Modifiers::none());
        redraw(cx);
        let rest = painted_at(cx, "button");
        let rest_fills = fills_at(cx, "button");
        let size = cx.debug_bounds("button").unwrap().size;
        crate::ui::test_support::press(cx, "tab");
        assert_eq!(
            cx.debug_bounds("button").unwrap().size,
            size,
            "{style:?}: focus changes size"
        );
        let focused = painted_at(cx, "button");
        assert!(
            fills_at(cx, "button") != rest_fills
                || focused.iter().any(|quad| {
                    quad.border_widths.left.0 > 0.
                        && quad.border_color.a > 0.
                        && !rest.iter().any(|before| {
                            before.border_color == quad.border_color
                                && before.border_widths == quad.border_widths
                        })
                }),
            "{style:?} has no visible focus style on its surface: {focused:?}"
        );
    }
}

#[gpui::test]
fn disabled_buttons_are_not_tab_stops_and_dim_as_a_whole(cx: &mut TestAppContext) {
    for style in BUTTON_STYLES {
        let clicks = Rc::new(Cell::new(0));
        let seen = clicks.clone();
        let cx = crate::ui::test_support::draw_with_tab(cx, move || {
            let seen = seen.clone();
            div()
                .child(
                    Button::new("disabled", "Wait")
                        .style(style)
                        .disabled(true)
                        .leading(div().w(px(4.)).h(px(4.)).bg(gpui::white()))
                        .on_click(|_, _, _| panic!("disabled button fired")),
                )
                .child(
                    Button::new("enabled", "Act")
                        .style(style)
                        .on_click(move |_, _, _| {
                            seen.set(seen.get() + 1);
                        }),
                )
                .into_any_element()
        });
        let disabled = fills_at(cx, "disabled");
        let enabled = fills_at(cx, "enabled");
        assert_eq!(disabled.len(), 1);
        assert_eq!(enabled.len(), 1);
        assert!(
            disabled[0].a < enabled[0].a,
            "{style:?} surface is not dimmed"
        );
        let bounds = cx.debug_bounds("disabled").unwrap();
        assert!(
            cx.update(|window, _| window.painted_quads().iter().any(|quad| {
                let scale = window.scale_factor();
                let x = px(quad.bounds.origin.x.0 / scale);
                let y = px(quad.bounds.origin.y.0 / scale);
                bounds.contains(&gpui::point(x, y))
                    && quad
                        .background
                        .as_solid()
                        .is_some_and(|fill| fill.a > 0. && fill.a < 1.)
                    && (quad.bounds.size.width.0 / scale - 4.).abs() < 0.1
            })),
            "{style:?} leading content is not dimmed"
        );
        crate::ui::test_support::press(cx, "tab");
        crate::ui::test_support::press(cx, "enter");
        assert_eq!(
            clicks.get(),
            1,
            "{style:?}: Tab must skip the disabled button"
        );
        crate::ui::test_support::click(cx, "disabled");
    }
}

#[gpui::test]
fn buttons_show_a_pressed_state(cx: &mut TestAppContext) {
    for style in styles() {
        let cx = draw(cx, move || button(style, Rc::new(Cell::new(0))));
        let center = cx.debug_bounds("button").unwrap().center();
        cx.simulate_mouse_move(center, None, Modifiers::none());
        redraw(cx);
        let hover = fills_at(cx, "button");
        cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::none());
        redraw(cx);
        assert_ne!(
            fills_at(cx, "button"),
            hover,
            "{style:?} has no pressed state"
        );
        cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::none());
        redraw(cx);
        assert_eq!(
            fills_at(cx, "button"),
            hover,
            "{style:?}: release restores hover"
        );
    }
}

#[gpui::test]
fn ghost_icon_buttons_have_at_least_the_icon_button_size(cx: &mut TestAppContext) {
    for size in [IconSize::Small, IconSize::Glyph, IconSize::Medium] {
        let cx = draw(cx, move || {
            IconButton::new("ghost", UiIcon::Close)
                .size(size)
                .into_any_element()
        });
        let bounds = cx.debug_bounds("ghost").unwrap();
        let minimum = crate::ui::tokens::ICON_BUTTON_SIZE;
        assert!(bounds.size.width >= minimum && bounds.size.height >= minimum);
        assert_eq!(bounds.size.width, bounds.size.height);
    }
}
