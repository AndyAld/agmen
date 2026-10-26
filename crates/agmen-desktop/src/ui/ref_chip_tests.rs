use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Modifiers, Render, TestAppContext, VisualTestContext, Window, div, px,
};

use super::*;
use crate::backend::{chat_snapshot, fixture_id};
use crate::ui::chat_context::ChatContext;
use crate::ui::test_support::{click, init, redraw, rendered_size};
use crate::ui::tokens::MOCKUP_REM;

fn ctx() -> Rc<ChatContext> {
    Rc::new(ChatContext::new(Rc::new(chat_snapshot())))
}

fn branch(name: &str, at: Option<&str>) -> RefTarget {
    RefTarget::Branch {
        name: name.into(),
        at: at.map(|sha| sha.parse().unwrap()),
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
        RefTarget::Commit("4d9fdc8".parse().unwrap()),
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
        .on_ref(fixture_id("andy/listen"), move |event, _, _| {
            sink.borrow_mut().push(event);
        })
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
        [RefEvent::OpenRef {
            target: RefTarget::Agent("andy/fx1".into()),
            from: fixture_id("andy/listen"),
        }]
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
        [RefEvent::OpenRefInSplit {
            target: RefTarget::Agent("andy/fx1".into()),
            from: fixture_id("andy/listen"),
        }]
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
                RefTarget::Commit("4d9fdc8".parse().unwrap()),
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
