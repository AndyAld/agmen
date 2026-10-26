use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Context, Entity, Render, TestAppContext, VisualTestContext, Window, div, px};

use super::*;
use crate::backend::{Backend as _, FixtureBackend, Screen};
use crate::ui::plan_test_support::plan_globals;
use crate::ui::test_support::{click, redraw};

fn plan(screen: Screen) -> PlanInfo {
    FixtureBackend::for_screen(screen)
        .unwrap()
        .snapshot()
        .plan
        .unwrap()
}

#[test]
fn pill_counts_what_the_plan_waits_on() {
    assert_eq!(
        state_pill(&plan(Screen::Plan)),
        ("waiting on 2 approvals".into(), Color::Warning)
    );
    assert_eq!(
        state_pill(&plan(Screen::PlanReady)),
        ("ready to kick off".into(), Color::Success)
    );
    assert_eq!(
        state_pill(&plan(Screen::Running)),
        ("running · 7 agents".into(), Color::Accent)
    );
    let mut one = plan(Screen::Plan);
    one.shares_approved.insert("sean".into(), true);
    assert_eq!(state_pill(&one).0, "waiting on 1 approval");
}

#[test]
fn status_context_names_the_plan() {
    assert_eq!(status_context(&plan(Screen::Plan)), "ridgeline · planning");
    assert_eq!(
        status_context(&plan(Screen::Running)),
        "ridgeline · 7 agents"
    );
}

#[test]
fn share_note_sums_each_owner() {
    assert_eq!(
        share_note(&plan(Screen::Plan)),
        "$105/day: andy $65, sean $40. Each agent runs on its owner's machine with that \
         person's keys, and each person approves their own share."
    );
}

#[test]
fn lanes_read_as_one_line() {
    assert_eq!(
        lanes_line(&plan(Screen::Plan)),
        "L-CORE core/** · L-APP app/** · L-SYNC sync/** · L-TESTS tests/**"
    );
}

#[test]
fn spec_state_reads_as_the_mockup() {
    let draft = plan(Screen::Plan);
    assert_eq!(
        spec_state(&draft.specs[0]),
        ("andy ✓ · sean ○".into(), Color::Warning)
    );
    assert_eq!(
        spec_state(&draft.specs[1]),
        ("draft, researcher fills in".into(), Color::Warning)
    );
    assert_eq!(
        spec_state(&draft.specs[2]),
        ("approved".into(), Color::Success)
    );
    assert_eq!(
        spec_state(&plan(Screen::PlanReady).specs[0]),
        ("approved".into(), Color::Success)
    );
}

/// The panel at its width, recording the spec tabs it asks to open.
struct Harness {
    model: Entity<PlanModel>,
    opened: Rc<RefCell<Vec<String>>>,
    focus: gpui::FocusHandle,
}

impl Render for Harness {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl gpui::IntoElement {
        // The workspace sets the rem from the UI font size, 15 px in the shots.
        window.set_rem_size(crate::ui::tokens::MOCKUP_REM);
        let opened = self.opened.clone();
        div()
            .track_focus(&self.focus)
            .on_action(move |action: &OpenSpec, _, _| opened.borrow_mut().push(action.0.clone()))
            .w(crate::ui::tokens::PLAN_PANEL_WIDTH)
            .h(px(900.))
            .child(PlanPanel::new(self.model.clone()))
    }
}

fn open(
    cx: &mut TestAppContext,
    screen: Screen,
) -> (
    Entity<PlanModel>,
    Rc<RefCell<Vec<String>>>,
    &mut VisualTestContext,
) {
    let (_, model) = plan_globals(cx, screen, None);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let (_, cx) = cx.add_window_view(|window, cx| {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Harness {
            model: model.clone(),
            opened: opened.clone(),
            focus,
        }
    });
    redraw(cx);
    (model, opened, cx)
}

#[gpui::test]
fn draft_panel_lists_the_team_checks_and_a_disabled_kick_off(cx: &mut TestAppContext) {
    let (model, _, cx) = open(cx, Screen::Plan);
    assert!(cx.debug_bounds("plan-panel").is_some());
    assert!(cx.debug_bounds("team:row-6").is_some());
    assert!(cx.debug_bounds("team:row-7").is_none());
    assert!(cx.debug_bounds("plan:check-4").is_some());
    assert!(cx.debug_bounds("plan:pause").is_none());
    click(cx, "plan:kickoff");
    model.read_with(cx, |model, _| assert!(!model.kickoff_open()));
}

#[gpui::test]
fn ready_kick_off_opens_the_dialog(cx: &mut TestAppContext) {
    let (model, _, cx) = open(cx, Screen::PlanReady);
    click(cx, "plan:kickoff");
    model.read_with(cx, |model, _| assert!(model.kickoff_open()));
}

#[gpui::test]
fn running_panel_shows_pause_all_and_running_rows(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Screen::Running);
    assert!(cx.debug_bounds("plan:pause").is_some());
    assert!(cx.debug_bounds("plan:kickoff").is_none());
    assert!(cx.debug_bounds("team:running-0").is_some());
}

#[gpui::test]
fn a_spec_with_text_opens_its_tab(cx: &mut TestAppContext) {
    let (_, opened, cx) = open(cx, Screen::Plan);
    click(cx, "plan:spec:docs/specs/ridgeline-design.md");
    // A spec the backend has no text for is not a link.
    click(cx, "plan:spec:docs/plans/m0.md");
    assert_eq!(*opened.borrow(), ["docs/specs/ridgeline-design.md"]);
}

// Shot diff (nodes 8951, 8969, 10852): "claude · sonnet" is not clipped
// at Andy's 13 px code font, the columns stay lined up, and the row still
// fits the panel.
#[gpui::test]
fn the_harness_column_fits_claude_sonnet(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Screen::Running);
    cx.update(|_, cx| {
        let mut appearance = cx.global::<crate::appearance::Appearance>().clone();
        appearance.fonts.code_size = px(13.);
        cx.set_global(appearance);
        crate::assets::load_fonts(cx).unwrap();
    });
    redraw(cx);
    let cell = cx.update(|window, cx| {
        let fonts = &cx.global::<crate::appearance::Appearance>().fonts;
        crate::ui::code_view::cell_width(window, fonts)
    });
    let mut bounds = |name: String| cx.debug_bounds(name.leak()).unwrap();
    let coord = bounds("team:harness-0".into());
    // "claude · sonnet" is 15 cells.
    assert!(coord.size.width >= cell * 15., "{coord:?} for {cell:?}");
    for ix in 0..7 {
        let harness = bounds(format!("team:harness-{ix}"));
        assert_eq!(harness.origin.x, coord.origin.x, "row {ix}");
        assert_eq!(harness.size.width, coord.size.width, "row {ix}");
        let row = bounds(format!("team:row-{ix}"));
        let cost = bounds(format!("team:cost-{ix}"));
        assert!(cost.right() <= row.right(), "row {ix}: {cost:?} in {row:?}");
    }
}
