use std::cell::RefCell;
use std::rc::Rc;

use gpui::TestAppContext;

use super::*;
use crate::backend::{chat_snapshot, fixture_id};
use crate::ui::test_support::{click, init, redraw};

#[test]
fn options_take_each_owners_role() {
    // Review C188: the role colors the dot; a worktree whose owner has no
    // AgentInfo gets an empty role.
    let s = chat_snapshot();
    let options = ScopeOption::for_worktrees(&s.worktrees, &s.agents);
    assert_eq!(options.len(), s.worktrees.len());
    assert_eq!(options[0].owner, fixture_id("andy/fx1"));
    assert_eq!(options[0].role.as_ref(), "engineer");
    let t7k = options
        .iter()
        .find(|o| o.owner == fixture_id("andy/t7k"))
        .unwrap();
    assert_eq!(
        t7k.role.as_ref(),
        "",
        "the fixture has no AgentInfo for t7k"
    );
}

#[test]
fn scope_label_names_the_agent_and_head() {
    let s = chat_snapshot();
    assert_eq!(scope_label(&s.worktrees[0]), "fx1 · fix/c015-main-conflict");
    let mut person = s.worktrees[0].clone();
    person.handle = "andy".into();
    assert!(
        scope_label(&person).starts_with("andy · "),
        "{}",
        scope_label(&person)
    );
}

#[gpui::test]
fn picking_the_selected_option_emits_nothing(cx: &mut TestAppContext) {
    init(cx);
    let s = chat_snapshot();
    let options = ScopeOption::for_worktrees(&s.worktrees, &s.agents);
    let (chip, cx) = cx.add_window_view(|_, _| ScopeChip::new(options));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    cx.update(|_, cx| {
        cx.subscribe(&chip, move |_, event: &ScopeChipEvent, _| {
            let ScopeChipEvent::Switch(owner) = event;
            sink.borrow_mut().push(owner.clone());
        })
        .detach();
    });
    redraw(cx);
    click(cx, "scope-chip");
    click(cx, "scope-chip:0");
    assert!(seen.borrow().is_empty());
    click(cx, "scope-chip");
    click(cx, "scope-chip:1");
    assert_eq!(seen.borrow().len(), 1);
}

#[test]
fn chip_text_takes_the_text_color() {
    // V90: node 4553 (console.py 881) draws the label in text #d8bf94, not
    // Chip's default text.placeholder. Chip holds elements, so its Debug
    // (label, color and shape) stands in for equality.
    assert_eq!(
        format!("{:?}", chip("fx1 · main".into(), "engineer".into())),
        format!("{:?}", Chip::new("fx1 · main").color(Color::Default))
    );
    let theme = crate::appearance::Appearance::builtin().theme;
    assert_eq!(Color::Default.hsla(&theme), gpui::rgb(0xd8bf94).into());
}

#[gpui::test]
fn the_open_menu_stops_at_its_height_cap(cx: &mut TestAppContext) {
    // C256: with many worktrees the menu stops at the shared menu's
    // 320 px cap and scrolls.
    init(cx);
    let s = chat_snapshot();
    let one = ScopeOption::for_worktrees(&s.worktrees, &s.agents);
    let options: Vec<ScopeOption> = std::iter::repeat_n(one, 10).flatten().collect();
    assert!(options.len() >= 40);
    let (_, cx) = cx.add_window_view(|_, _| ScopeChip::new(options));
    redraw(cx);
    click(cx, "scope-chip");
    let menu = cx.debug_bounds("scope-chip:menu").unwrap();
    assert_eq!(
        menu.size.height,
        crate::ui::tokens::DROPDOWN_MENU_MAX_HEIGHT,
        "{menu:?}"
    );
    // The rows past the cap are reached by scrolling the list.
    let first = cx.debug_bounds("scope-chip:0").unwrap().top();
    cx.simulate_event(gpui::ScrollEvent {
        position: menu.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-100.))),
        ..Default::default()
    });
    redraw(cx);
    let scrolled = cx.debug_bounds("scope-chip:0").unwrap().top();
    assert_eq!(first - scrolled, gpui::px(100.));
}

#[gpui::test]
fn the_open_menu_uses_the_menu_frame(cx: &mut TestAppContext) {
    // V118: node 1802's border, fill and 4 px list padding.
    init(cx);
    let s = chat_snapshot();
    let options = ScopeOption::for_worktrees(&s.worktrees, &s.agents);
    let (_, cx) = cx.add_window_view(|_, _| ScopeChip::new(options));
    redraw(cx);
    click(cx, "scope-chip");
    crate::ui::test_support::assert_menu_frame(cx, "scope-chip:menu", "scope-chip:0");
}

#[gpui::test]
fn menu_rows_hover_as_menu_rows(cx: &mut TestAppContext) {
    // V119: the selected row keeps its fill; the others show a hover.
    init(cx);
    let s = chat_snapshot();
    let options = ScopeOption::for_worktrees(&s.worktrees, &s.agents);
    let (_, cx) = cx.add_window_view(|_, _| ScopeChip::new(options));
    redraw(cx);
    click(cx, "scope-chip");
    crate::ui::test_support::assert_menu_row_hover(cx, "scope-chip:0", "scope-chip:1");
}
