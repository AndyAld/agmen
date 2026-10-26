use gpui::{Bounds, TestAppContext, point, px, size};

use super::*;
use crate::backend::Backend as _;
use crate::backend::{FixtureBackend, Screen};
use crate::ui::test_support::draw;
use crate::ui::tokens::SWITCH_MENU_HEIGHT;

fn agents() -> Vec<InstalledAgent> {
    FixtureBackend::for_screen(Screen::SwitchAgent)
        .unwrap()
        .snapshot()
        .installed_agents
}

fn agent(name: &str) -> InstalledAgent {
    agents().into_iter().find(|a| a.name == name).unwrap()
}

#[gpui::test]
fn switch_menu_marks_outdated_and_missing(cx: &mut TestAppContext) {
    // console.py INSTALLED: agy 1.1.7 wants an update for JSON, grok is
    // missing, claude runs the current session.
    assert_eq!(
        note(&agent("agy")),
        Some(("update to 1.1.15+ for JSON".into(), Color::Warning))
    );
    assert_eq!(row_button(&agent("agy")), Some(RowButton::Update));
    assert_eq!(
        note(&agent("grok")),
        Some(("not installed".into(), Color::Muted))
    );
    assert_eq!(row_button(&agent("grok")), Some(RowButton::Add));
    for ready in ["claude", "codex", "opencode", "gemini"] {
        assert_eq!(note(&agent(ready)), None, "{ready}");
        assert_eq!(row_button(&agent(ready)), None, "{ready}");
    }

    let cx = draw(cx, || {
        SwitchMenu::new(agents(), "claude", "andy/fx1").into_any_element()
    });
    assert!(cx.debug_bounds("switch-menu:claude:check").is_some());
    assert!(cx.debug_bounds("switch-menu:claude:current").is_some());
    assert!(cx.debug_bounds("switch-menu:codex:check").is_none());
    assert!(cx.debug_bounds("switch-menu:codex:version").is_some());
    assert!(cx.debug_bounds("switch-menu:agy:note").is_some());
    assert!(cx.debug_bounds("switch-menu:agy:update").is_some());
    assert!(cx.debug_bounds("switch-menu:grok:version").is_none());
    assert!(cx.debug_bounds("switch-menu:grok:add").is_some());
    assert!(cx.debug_bounds("switch-menu:add-other").is_some());
    let menu = cx.debug_bounds("switch-menu").unwrap();
    assert_eq!(menu.size.width, SWITCH_MENU_WIDTH);
}

#[gpui::test]
fn switch_menu_offers_an_agent_whose_version_is_unknown(cx: &mut TestAppContext) {
    // C261: `--version` timed out or printed nothing parseable. The agent is
    // on PATH, so a session can switch to it, but the row says why it cannot
    // run a worker and has no version to show.
    let with_unknown_agy = || {
        let mut agents = agents();
        for agent in &mut agents {
            if agent.name == "agy" {
                agent.found = crate::backend::AgentFound::VersionUnknown;
            }
        }
        agents
    };
    let agy = with_unknown_agy()
        .into_iter()
        .find(|a| a.name == "agy")
        .unwrap();
    assert_eq!(
        note(&agy),
        Some(("could not read its version".into(), Color::Warning))
    );
    assert_eq!(row_button(&agy), None);
    let switched: std::rc::Rc<std::cell::RefCell<Vec<String>>> = std::rc::Rc::default();
    let sink = switched.clone();
    let cx = draw(cx, move || {
        let sink = sink.clone();
        SwitchMenu::new(with_unknown_agy(), "claude", "andy/fx1")
            .on_switch(move |agent, _, _| sink.borrow_mut().push(agent))
            .into_any_element()
    });
    assert!(cx.debug_bounds("switch-menu:agy:note").is_some());
    assert!(cx.debug_bounds("switch-menu:agy:version").is_none());
    assert!(cx.debug_bounds("switch-menu:agy:update").is_none());
    crate::ui::test_support::click(cx, "switch-menu:agy");
    assert_eq!(*switched.borrow(), ["agy"]);
}

#[gpui::test]
fn switch_menu_uses_the_menu_frame(cx: &mut TestAppContext) {
    // V118: the switch menu draws the frame Dropdown and the scope chip do.
    let cx = draw(cx, || {
        SwitchMenu::new(agents(), "claude", "andy/fx1").into_any_element()
    });
    crate::ui::test_support::assert_menu_frame(cx, "switch-menu", "switch-menu:claude");
}

fn head() -> Bounds<Pixels> {
    Bounds::new(point(px(300.), px(429.)), size(px(800.), px(32.)))
}

fn chip() -> Bounds<Pixels> {
    Bounds::new(point(px(321.), px(435.)), size(px(72.), px(20.)))
}

fn menu() -> Size<Pixels> {
    size(SWITCH_MENU_WIDTH, SWITCH_MENU_HEIGHT)
}

fn area(height: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(0.), px(0.)), size(px(1440.), px(height)))
}

/// The menu's bounds for a placement.
fn placed(p: &Placement, menu: Size<Pixels>) -> Bounds<Pixels> {
    let top = match p.anchor {
        Anchor::TopLeft => p.position.y,
        _ => p.position.y - menu.height,
    };
    Bounds::new(point(p.position.x, top), menu)
}

#[test]
fn menu_hangs_below_the_head_when_it_fits() {
    // Fix 27: the mockup's menu covered its own chip.
    let p = place_menu(chip(), head(), menu(), area(872.));
    assert_eq!(p.anchor, Anchor::TopLeft);
    assert_eq!(p.position, point(chip().left(), head().bottom()));
    assert!(p.max_height >= menu().height, "a menu that fits is not cut");
    assert!(!placed(&p, menu()).intersects(&chip()));
}

#[test]
fn menu_goes_above_the_head_when_below_is_short() {
    let p = place_menu(chip(), head(), menu(), area(600.));
    assert_eq!(p.anchor, Anchor::BottomLeft);
    assert_eq!(p.position, point(chip().left(), head().top()));
    let bounds = placed(&p, menu());
    assert!(!bounds.intersects(&chip()));
    assert!(bounds.top() >= px(0.));
}

#[test]
fn menu_takes_the_roomier_side_when_neither_fits() {
    // 300 px below the head, 200 above: below, cut by the window.
    let head = Bounds::new(point(px(0.), px(168.)), size(px(800.), px(32.)));
    let chip = Bounds::new(point(px(20.), px(174.)), size(px(72.), px(20.)));
    let p = place_menu(chip, head, menu(), area(500.));
    assert_eq!(p.anchor, Anchor::TopLeft);
    // Cut to the 300 px below, so the window's snap has nothing to move.
    assert_eq!(p.max_height, px(300.));
    let cut = size(menu().width, menu().height.min(p.max_height));
    let bounds = placed(&p, cut);
    assert!(!bounds.intersects(&chip));
    assert!(bounds.bottom() <= px(500.));
}

#[test]
fn menu_stays_inside_the_window_sideways() {
    let chip = Bounds::new(point(px(1300.), px(435.)), size(px(72.), px(20.)));
    let p = place_menu(chip, head(), menu(), area(872.));
    assert_eq!(p.position.x, px(1440.) - SWITCH_MENU_WIDTH);
}
