use std::cell::RefCell;
use std::rc::Rc;

use agmen_proto::IdentityId;
use gpui::{Entity, TestAppContext, VisualTestContext, px};

use super::*;
use crate::backend::{Backend as _, FixtureBackend, Screen};
use crate::ui::plan_test_support::{plan, plan_globals};
use crate::ui::test_support::{click, redraw};

type Selected = Rc<RefCell<Vec<IdentityId>>>;

fn open(
    cx: &mut TestAppContext,
    screen: Screen,
) -> (Entity<PlanRoom>, Selected, &mut VisualTestContext) {
    let (_, model) = plan_globals(cx, screen, None);
    let roster = FixtureBackend::for_screen(screen)
        .unwrap()
        .snapshot()
        .roster;
    let selected = Selected::default();
    let sink = selected.clone();
    let (room, cx) = cx.add_window_view(|_, cx| PlanRoom::new(roster, model, cx));
    cx.update(|_, cx| {
        cx.subscribe(&room, move |_, event: &AgentsPanelEvent, _| {
            let AgentsPanelEvent::Selected(id) = event else {
                panic!("unexpected agent action")
            };
            sink.borrow_mut().push(id.clone());
        })
        .detach();
    });
    redraw(cx);
    (room, selected, cx)
}

fn y(cx: &mut VisualTestContext, selector: &'static str) -> gpui::Pixels {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not drawn"))
        .origin
        .y
}

#[gpui::test]
fn planning_room_lists_people_then_the_planner_then_agents(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Screen::Plan);
    assert!(cx.debug_bounds("planning-room:count").is_some());
    let order = [
        "planning-room",
        "room:person:andy",
        "room:person:sean",
        "room:agent:sean/plan",
        "room:agent:andy/listen",
        "plan-team",
    ];
    for pair in order.windows(2) {
        assert!(
            y(cx, pair[0]) < y(cx, pair[1]),
            "{} before {}",
            pair[0],
            pair[1]
        );
    }
    // V67: people show their status glyph in the agents' leading slot.
    assert!(cx.debug_bounds("room:person:sean:leading").is_some());
}

#[test]
fn people_are_tagged_you_or_human() {
    let roster = FixtureBackend::for_screen(Screen::Plan)
        .unwrap()
        .snapshot()
        .roster;
    assert_eq!(person_tag(&roster[0]), "you");
    assert_eq!(person_tag(&roster[1]), "human");
}

#[test]
fn team_count_is_planned_until_it_runs() {
    assert_eq!(team_count(&plan(Screen::Plan)), "planned");
    assert_eq!(team_count(&plan(Screen::Running)), "7");
}

#[gpui::test]
fn team_is_folded_until_the_plan_runs(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Screen::Plan);
    assert!(cx.debug_bounds("team:person:andy").is_none());
}

#[gpui::test]
fn running_team_groups_agents_by_owner(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Screen::Running);
    let order = [
        "team:person:andy",
        "team:agent:andy/coord",
        "team:agent:andy/core",
        "team:agent:andy/app",
        "team:agent:andy/rev",
        "team:person:sean",
        "team:agent:sean/sync",
        "team:agent:sean/qa",
        "team:agent:sean/tiles",
    ];
    for pair in order.windows(2) {
        assert!(
            y(cx, pair[0]) < y(cx, pair[1]),
            "{} before {}",
            pair[0],
            pair[1]
        );
    }
    // Mockup fix 46: person rows are 26 px, as every other row.
    assert_eq!(
        cx.debug_bounds("team:person:andy").unwrap().size.height,
        crate::ui::tokens::AGENT_ROW_HEIGHT
    );
}

#[gpui::test]
fn team_person_rows_set_the_chevron_and_name_as_node_10400(cx: &mut TestAppContext) {
    // Nodes 10400 to 10403: padding 24, the 12 px chevron at x 24 to 36,
    // then a 6 px gap to the name at x 42. The Agents tree's group row (12
    // px chevron, 6 px gap), one tree step in.
    let (_, _, cx) = open(cx, Screen::Running);
    for (row, leading, name) in [
        (
            "team:person:andy",
            "team:person:andy:leading",
            "team:person:andy:name",
        ),
        (
            "team:person:sean",
            "team:person:sean:leading",
            "team:person:sean:name",
        ),
    ] {
        let row = cx.debug_bounds(row).expect("the person row");
        let chevron = cx.debug_bounds(leading).expect("the chevron");
        let name = cx.debug_bounds(name).expect("the name");
        assert_eq!(chevron.left() - row.left(), px(24.), "{leading}");
        assert_eq!(chevron.right() - row.left(), px(36.), "{leading}");
        assert_eq!(name.left() - row.left(), px(42.), "{leading}");
    }
}

#[gpui::test]
fn folding_a_person_hides_their_agents(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Screen::Running);
    click(cx, "team:person:sean");
    assert!(cx.debug_bounds("team:agent:sean/sync").is_none());
    assert!(cx.debug_bounds("team:agent:andy/coord").is_some());
}

#[gpui::test]
fn folding_the_room_hides_its_rows(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Screen::Plan);
    click(cx, "planning-room");
    assert!(cx.debug_bounds("room:person:andy").is_none());
    click(cx, "plan-team");
    assert!(cx.debug_bounds("team:person:andy").is_some());
}

#[gpui::test]
fn clicking_a_room_agent_selects_it(cx: &mut TestAppContext) {
    let (_, selected, cx) = open(cx, Screen::Plan);
    click(cx, "room:agent:sean/plan");
    assert_eq!(
        *selected.borrow(),
        [crate::backend::fixture_id("sean/plan")]
    );
}
