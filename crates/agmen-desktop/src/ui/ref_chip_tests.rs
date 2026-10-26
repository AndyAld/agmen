use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Modifiers, Render, TestAppContext, VisualTestContext, Window, div, px,
};

use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{click, init, redraw, rendered_size};
use crate::ui::tokens::MOCKUP_REM;

fn ctx() -> Rc<ChatContext> {
    Rc::new(ChatContext::new(Rc::new(chat_snapshot())))
}

fn branch(name: &str, at: Option<&str>) -> RefTarget {
    RefTarget::Branch {
        name: name.into(),
        at: at.map(Into::into),
    }
}

fn file(path: &str, lines: Option<(u32, u32)>) -> RefTarget {
    RefTarget::File {
        path: path.into(),
        lines,
    }
}

/// Checks a kind's color and fill (`ref_color` and `rich_body` in
/// console.py), and that its chip draws.
fn check(cx: &mut TestAppContext, target: RefTarget, color: Color, is_filled: bool) {
    let context = ctx();
    assert_eq!(context.chip_color(&target), color, "{target:?}");
    assert_eq!(filled(&target), is_filled, "{target:?}");
    let size = rendered_size(cx, move || {
        RefChip::new("chip", "text", target.clone(), color.clone()).into_any_element()
    });
    assert!(size.width > px(0.) && size.height > px(0.));
}

#[gpui::test]
fn agent_chip_takes_its_role_color(cx: &mut TestAppContext) {
    check(
        cx,
        RefTarget::Agent("andy/fx1".into()),
        Color::Role("engineer".into()),
        true,
    );
    let context = ctx();
    assert_eq!(
        context.chip_color(&RefTarget::Agent("andy/listen".into())),
        Color::Role("listener".into())
    );
    assert_eq!(
        context.chip_color(&RefTarget::Agent("andy/rv2".into())),
        Color::Role("reviewer".into())
    );
    // console.py draws a handle it does not know as an engineer.
    assert_eq!(
        context.chip_color(&RefTarget::Agent("zed/x9".into())),
        Color::Role("engineer".into())
    );
}

#[gpui::test]
fn person_chip_is_bright_and_filled(cx: &mut TestAppContext) {
    check(cx, RefTarget::Person("andy".into()), Color::TextMuted, true);
}

#[gpui::test]
fn role_chip_takes_the_role_color(cx: &mut TestAppContext) {
    check(
        cx,
        RefTarget::Role("reviewer".into()),
        Color::Role("reviewer".into()),
        true,
    );
}

#[gpui::test]
fn branch_chip_is_green_and_filled(cx: &mut TestAppContext) {
    check(
        cx,
        branch("fix/c015-main-conflict", Some("9e2a71c")),
        Color::Success,
        true,
    );
}

#[gpui::test]
fn commit_chip_is_plain_accent(cx: &mut TestAppContext) {
    check(
        cx,
        RefTarget::Commit("4d9fdc8".into()),
        Color::Accent,
        false,
    );
}

#[gpui::test]
fn file_chip_is_plain_accent(cx: &mut TestAppContext) {
    check(cx, file("src/main.rs", Some((2, 8))), Color::Accent, false);
}

#[gpui::test]
fn item_chip_is_magenta_and_filled(cx: &mut TestAppContext) {
    check(cx, RefTarget::Item("C-015".into()), Color::Player(1), true);
}

#[gpui::test]
fn lane_chip_is_yellow_and_filled(cx: &mut TestAppContext) {
    check(cx, RefTarget::Lane("L-INFRA".into()), Color::Warning, true);
}

#[gpui::test]
fn pr_chip_is_plain_accent(cx: &mut TestAppContext) {
    check(cx, RefTarget::Pr(9), Color::Accent, false);
}

fn row<'a>(card: &'a RefCard, label: &str) -> Option<&'a str> {
    card.rows
        .iter()
        .find(|(l, _)| *l == label)
        .map(|(_, v)| v.as_str())
}

#[test]
fn hover_card_shows_agent_state_and_spend() {
    let card = ctx().card(&RefTarget::Agent("andy/fx1".into()));
    assert_eq!(card.kind, "Agent");
    assert_eq!(card.title, "andy/fx1");
    assert_eq!(row(&card, "Role"), Some("Engineer"));
    assert_eq!(row(&card, "Runs on"), Some("claude"));
    assert_eq!(row(&card, "State"), Some("working"));
    assert_eq!(row(&card, "Owner"), Some("andy (you)"));
    assert_eq!(row(&card, "Spent"), Some("$1.20 of $5.00"));
    assert_eq!(row(&card, "Branch"), Some("fix/c015-main-conflict"));

    let stranger = ctx().card(&RefTarget::Agent("zed/x9".into()));
    assert_eq!(
        row(&stranger, "State"),
        Some("not in this project's roster")
    );
}

#[test]
fn hover_card_shows_branch_worktree_and_last_done() {
    let card = ctx().card(&branch("fix/c015-main-conflict", Some("9e2a71c")));
    assert_eq!(card.kind, "Branch");
    assert_eq!(row(&card, "At"), Some("9e2a71c"));
    assert_eq!(
        row(&card, "Worktree"),
        Some("andy/fx1 · 1 commit, pushed, clean")
    );
    assert_eq!(row(&card, "Base"), Some("origin/main @4d9fdc8"));
    assert_eq!(row(&card, "Last DONE"), Some("andy/fx1 · C-015"));

    let elsewhere = ctx().card(&branch("test/b-415-replay-paging", None));
    assert_eq!(row(&elsewhere, "Worktree"), Some("none on this machine"));
}

#[test]
fn hover_card_shows_item_claim_done_and_review() {
    let card = ctx().card(&RefTarget::Item("C-015".into()));
    assert_eq!(card.kind, "Item");
    assert_eq!(card.title, "C-015");
    assert_eq!(row(&card, "Claimed by"), Some("andy/fx1"));
    assert_eq!(
        row(&card, "Done"),
        Some("andy/fx1 · fix/c015-main-conflict@9e2a71c")
    );
    assert_eq!(row(&card, "Review"), Some("pass · andy/rv2"));

    let unseen = ctx().card(&RefTarget::Item("Z-999".into()));
    assert_eq!(row(&unseen, "Claim"), Some("none in this chat"));
}

#[test]
fn hover_card_for_the_other_kinds() {
    let c = ctx();
    let commit = c.card(&RefTarget::Commit("9e2a71c".into()));
    assert_eq!(
        row(&commit, "In DONE"),
        Some("andy/fx1 · fix/c015-main-conflict")
    );
    let base = c.card(&RefTarget::Commit("4d9fdc8".into()));
    assert_eq!(
        row(&base, "Base of"),
        Some("andy/a1 · feat/c014-release-matrix, andy/fx1 · fix/c015-main-conflict")
    );

    let person = c.card(&RefTarget::Person("andy".into()));
    assert_eq!(person.title, "andy (you)");
    assert_eq!(row(&person, "Agents"), Some("listen, fx1, t7k, rv2, a1"));

    let f = c.card(&file("src/main.rs", Some((2, 8))));
    assert_eq!(f.title, "src/main.rs");
    assert_eq!(row(&f, "Lines"), Some("2 to 8"));
    assert_eq!(
        row(&f, "Changed in"),
        Some("andy/a1 · feat/c014-release-matrix, andy/fx1 · fix/c015-main-conflict")
    );
    let one = c.card(&file("src/main.rs", Some((4, 4))));
    assert_eq!(row(&one, "Lines"), Some("4"));

    let lane = c.card(&RefTarget::Lane("L-CORE".into()));
    assert_eq!(row(&lane, "Edited by"), Some("andy/fx1"));

    let role = c.card(&RefTarget::Role("engineer".into()));
    assert_eq!(row(&role, "Held by"), Some("andy/fx1, andy/a1, sean/a1"));

    let pr = c.card(&RefTarget::Pr(10));
    assert_eq!(pr.title, "#10");
    assert_eq!(row(&pr, "GitHub"), Some("not connected yet"));
}

/// Draws what `make` builds every frame.
struct Host(Box<dyn Fn() -> AnyElement>);

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div().p(px(20.)).child((self.0)())
    }
}

fn host(
    cx: &mut TestAppContext,
    make: impl Fn() -> AnyElement + 'static,
) -> &mut VisualTestContext {
    init(cx);
    let (_, cx) = cx.add_window_view(move |_, _| Host(Box::new(make)));
    redraw(cx);
    cx
}

type Heard = Rc<RefCell<Vec<RefEvent>>>;

fn listening_chip(cx: &mut TestAppContext) -> (Heard, &mut VisualTestContext) {
    let heard: Heard = Rc::new(RefCell::new(Vec::new()));
    let sink = heard.clone();
    let context = ctx();
    let cx = host(cx, move || {
        let sink = sink.clone();
        RefChip::new(
            "m1:ref:0",
            "andy/fx1",
            RefTarget::Agent("andy/fx1".into()),
            Color::Role("engineer".into()),
        )
        .card(context.clone())
        .on_ref(move |event, _, _| sink.borrow_mut().push(event))
        .into_any_element()
    });
    (heard, cx)
}

#[gpui::test]
fn click_opens_ref(cx: &mut TestAppContext) {
    let (heard, cx) = listening_chip(cx);
    click(cx, "m1:ref:0");
    assert_eq!(
        *heard.borrow(),
        [RefEvent::OpenRef(RefTarget::Agent("andy/fx1".into()))]
    );
}

#[gpui::test]
fn cmd_click_opens_ref_in_split(cx: &mut TestAppContext) {
    let (heard, cx) = listening_chip(cx);
    let center = cx.debug_bounds("m1:ref:0").unwrap().center();
    let secondary = cx.update(|_, cx| Modifiers::secondary_key(cx));
    cx.simulate_click(center, secondary);
    assert_eq!(
        *heard.borrow(),
        [RefEvent::OpenRefInSplit(RefTarget::Agent(
            "andy/fx1".into()
        ))]
    );
}

#[gpui::test]
fn hovering_shows_the_card(cx: &mut TestAppContext) {
    let (_, cx) = listening_chip(cx);
    assert!(cx.debug_bounds(REF_CARD_SELECTOR).is_none());
    let center = cx.debug_bounds("m1:ref:0").unwrap().center();
    cx.simulate_mouse_move(center, None, Modifiers::none());
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    cx.run_until_parked();
    redraw(cx);
    assert!(cx.debug_bounds(REF_CARD_SELECTOR).is_some());
}

#[gpui::test]
fn flush_side_drops_its_padding(cx: &mut TestAppContext) {
    // Mockup fix 54: no gap between a chip and the punctuation after it.
    let make = |flush: bool| {
        move || {
            let chip = RefChip::new(
                "chip",
                "4d9fdc8",
                RefTarget::Commit("4d9fdc8".into()),
                Color::Accent,
            );
            if flush { chip.flush_end() } else { chip }.into_any_element()
        }
    };
    let padded = rendered_size(cx, make(false));
    let mut cx2 = TestAppContext::single();
    let flush = rendered_size(&mut cx2, make(true));
    assert_eq!(padded.width - flush.width, px(4.));
}
