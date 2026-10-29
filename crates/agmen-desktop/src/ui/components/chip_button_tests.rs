use super::*;
use crate::appearance::Appearance;
use crate::ui::components::Label;
use crate::ui::components::Menu;
use crate::ui::components::control_harness::open_with_caller;
use crate::ui::test_support::{click, fills_at, redraw};
use crate::ui::tokens::ICON_SM;
use gpui::{InputEvent as _, Modifiers, TestAppContext, VisualTestContext};

fn open(
    cx: &mut TestAppContext,
) -> (
    gpui::Entity<super::super::control_harness::Caller>,
    &mut VisualTestContext,
) {
    let focus = cx.update(|cx| cx.focus_handle());
    let root_focus = focus.clone();
    let (caller, cx) = open_with_caller(cx, false, move |open, set| {
        let close = set.clone();
        let chip = ChipButton::new("chip", Chip::new("model"))
            .open(open)
            .caret(true)
            .on_toggle(move |open, window, cx| set(open, window, cx))
            .menu(open.then(|| {
                div()
                    .absolute()
                    .top_full()
                    .left_0()
                    .child(
                        Menu::new("chip-menu")
                            .child(Label::new("option"))
                            .on_close(move |window, cx| close(false, window, cx)),
                    )
                    .into_any_element()
            }))
            .into_any_element();
        div()
            .track_focus(&root_focus)
            .on_key_down(crate::ui::focus_tab)
            .child(chip)
            .into_any_element()
    });
    cx.update(|window, cx| focus.focus(window, cx));
    redraw(cx);
    (caller, cx)
}

fn press(cx: &mut VisualTestContext, key: &'static str) {
    let key = gpui::Keystroke::parse(key).unwrap();
    cx.update(|window, cx| {
        window.dispatch_event(
            gpui::KeyDownEvent {
                keystroke: key.clone(),
                is_held: false,
                prefer_character_input: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(gpui::KeyUpEvent { keystroke: key }.to_platform_input(), cx);
    });
    redraw(cx);
}

#[gpui::test]
fn chip_button_click_while_open_closes_and_does_not_reopen(cx: &mut TestAppContext) {
    let (caller, cx) = open(cx);
    click(cx, "chip");
    assert!(caller.read_with(cx, |caller, _| caller.flag));
    assert!(cx.debug_bounds("chip-menu").is_some());
    click(cx, "chip");
    assert!(!caller.read_with(cx, |caller, _| caller.flag));
    assert!(cx.debug_bounds("chip-menu").is_none());
}

#[gpui::test]
fn chip_button_escape_closes_the_menu(cx: &mut TestAppContext) {
    let (caller, cx) = open(cx);
    click(cx, "chip");
    press(cx, "escape");
    assert!(!caller.read_with(cx, |caller, _| caller.flag));
    assert!(cx.debug_bounds("chip-menu").is_none());
}

#[gpui::test]
fn chip_button_takes_focus_with_tab_and_toggles_with_enter(cx: &mut TestAppContext) {
    let (caller, cx) = open(cx);
    press(cx, "tab");
    // Enter proves Tab moved from the test root to the trigger.
    press(cx, "enter");
    assert!(caller.read_with(cx, |caller, _| caller.flag));
    press(cx, "enter");
    assert!(!caller.read_with(cx, |caller, _| caller.flag));
    press(cx, "space");
    assert!(caller.read_with(cx, |caller, _| caller.flag));
    press(cx, "space");
    assert!(!caller.read_with(cx, |caller, _| caller.flag));
}

#[gpui::test]
fn chip_button_hover_is_visible_when_the_theme_hover_equals_the_surface(cx: &mut TestAppContext) {
    let (_, cx) = open(cx);
    cx.simulate_mouse_move(
        gpui::point(gpui::px(500.), gpui::px(500.)),
        None,
        Modifiers::none(),
    );
    redraw(cx);
    let rest = fills_at(cx, "chip-button:chip");
    let (surface, hover) = cx.update(|_, cx| {
        let theme = &cx.global::<Appearance>().theme;
        assert_eq!(theme.element_hover, theme.elevated_surface);
        (
            theme.elevated_surface,
            theme.hover_over(theme.elevated_surface),
        )
    });
    assert!(rest.contains(&surface), "{rest:?}");
    let bounds = cx.debug_bounds("chip").unwrap();
    cx.simulate_mouse_move(bounds.center(), None, Modifiers::none());
    redraw(cx);
    let hovered = fills_at(cx, "chip-button:chip");
    assert_ne!(hovered, rest);
    assert!(hovered.contains(&hover), "{hovered:?}");
}

#[gpui::test]
fn chip_button_shows_keyboard_focus(cx: &mut TestAppContext) {
    let (_, cx) = open(cx);
    cx.simulate_mouse_move(
        gpui::point(gpui::px(500.), gpui::px(500.)),
        None,
        Modifiers::none(),
    );
    redraw(cx);
    let rest = fills_at(cx, "chip-button:chip");
    press(cx, "tab");
    let focused = fills_at(cx, "chip-button:chip");
    let hover = cx.update(|_, cx| {
        let theme = &cx.global::<Appearance>().theme;
        theme.hover_over(theme.elevated_surface)
    });
    assert_ne!(focused, rest);
    assert!(focused.contains(&hover), "{focused:?}");
}

#[gpui::test]
fn chip_button_caret_is_an_icon(cx: &mut TestAppContext) {
    let (_, cx) = open(cx);
    let bounds = cx.debug_bounds("chip-caret").unwrap();
    assert_eq!(bounds.size, gpui::size(ICON_SM, ICON_SM));
    let debug = format!("{:?}", ChipButton::caret_icon());
    assert!(debug.contains("ChevronDown"), "{debug}");
}

#[gpui::test]
fn chip_button_keeps_menu_child_keyboard_activation(cx: &mut TestAppContext) {
    use crate::ui::components::Link;
    use std::cell::Cell;
    use std::rc::Rc;
    let clicks = Rc::new(Cell::new(0));
    let seen = clicks.clone();
    let (caller, cx) = open_with_caller(cx, false, move |open, set| {
        let close = set.clone();
        let seen = seen.clone();
        ChipButton::new("chip", Chip::new("model"))
            .open(open)
            .on_toggle(move |open, window, cx| set(open, window, cx))
            .menu(open.then(|| {
                div()
                    .absolute()
                    .top_full()
                    .left_0()
                    .child(
                        Menu::new("chip-menu")
                            .child(
                                Link::new("menu-child", "choose")
                                    .on_click(move |_, _, _| seen.set(seen.get() + 1)),
                            )
                            .on_close(move |window, cx| close(false, window, cx)),
                    )
                    .into_any_element()
            }))
            .into_any_element()
    });
    click(cx, "chip");
    cx.update(|window, cx| window.focus_next(cx));
    redraw(cx);
    press(cx, "enter");
    assert_eq!(clicks.get(), 1);
    assert!(caller.read_with(cx, |caller, _| caller.flag));
    press(cx, "space");
    assert_eq!(clicks.get(), 2);
    press(cx, "escape");
    assert!(!caller.read_with(cx, |caller, _| caller.flag));
}
