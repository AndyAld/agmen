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
    // Chip's default text.placeholder.
    assert_eq!(
        chip("fx1 · main".into(), "engineer".into()),
        Chip::new("fx1 · main")
            .color(Color::Default)
            .dot(Color::Role("engineer".into()))
            .code()
            .full_width()
            .trailing(UiIcon::ChevronDown)
    );
    let theme = crate::appearance::Appearance::builtin().theme;
    assert_eq!(Color::Default.hsla(&theme), gpui::rgb(0xd8bf94).into());
}

#[gpui::test]
fn the_open_menu_stops_at_its_height_cap(cx: &mut TestAppContext) {
    // C256: with many worktrees the menu scrolls instead of growing past
    // the shared menu's cap.
    init(cx);
    let s = chat_snapshot();
    let one = ScopeOption::for_worktrees(&s.worktrees, &s.agents);
    let options: Vec<ScopeOption> = std::iter::repeat_n(one, 10).flatten().collect();
    assert!(options.len() >= 40);
    let (_, cx) = cx.add_window_view(|_, _| ScopeChip::new(options));
    redraw(cx);
    click(cx, "scope-chip");
    let menu = cx.debug_bounds("scope-chip:menu").unwrap();
    assert!(
        menu.size.height <= crate::ui::tokens::DROPDOWN_MENU_MAX_HEIGHT,
        "{menu:?}"
    );
}
