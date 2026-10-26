use std::rc::Rc;

use agmen_proto::GitSha;
use gpui::{Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{Action, Backend as _, FixtureBackend, Screen, Snapshot};
use crate::ui::plan_test_support::{Recording, plan_globals};
use crate::ui::test_support::{assert_breadcrumb_glyphs, click, redraw};

const DESIGN: &str = "docs/specs/ridgeline-design.md";
const DRAFT: &str = "docs/specs/tile-sources.md";
const PLAN: &str = "docs/plans/m0.md";

/// The spec tab for `path` on `screen`, over `snapshot` when a test changed
/// the screen's own.
fn open<'a>(
    cx: &'a mut TestAppContext,
    screen: Screen,
    snapshot: Option<Snapshot>,
    path: &'static str,
) -> (Rc<Recording>, Entity<SpecView>, &'a mut VisualTestContext) {
    let (recording, model) = plan_globals(cx, screen, snapshot);
    let (view, cx) = cx.add_window_view(|_, cx| SpecView::new(model, path, cx));
    redraw(cx);
    (recording, view, cx)
}

/// The spec screen as sean sees it before he approves: his approval waits.
fn as_sean() -> Snapshot {
    let mut snapshot = FixtureBackend::for_screen(Screen::Spec).unwrap().snapshot();
    snapshot.me = "sean".into();
    snapshot
}

#[gpui::test]
fn spec_approve_dispatches_decision(cx: &mut TestAppContext) {
    let (recording, _, cx) = open(cx, Screen::Spec, Some(as_sean()), DESIGN);
    assert!(cx.debug_bounds("spec:approved").is_none());
    click(cx, "spec:approve");
    let sha = recording.snapshot().plan.unwrap().specs[0].sha.clone();
    assert_eq!(
        *recording.acts.borrow(),
        vec![Action::ApproveSpec {
            path: DESIGN.into(),
            sha,
        }]
    );
    assert!(cx.debug_bounds("spec:error").is_none());
}

#[gpui::test]
fn approve_names_the_commit_the_tab_shows(cx: &mut TestAppContext) {
    // The backend sent the review text of an older commit than its spec's
    // head, so Approve names that older commit and the backend refuses it.
    let mut snapshot = as_sean();
    let shown: GitSha = "0000000".parse().unwrap();
    snapshot.plan.as_mut().unwrap().reviews[0].sha = shown.clone();
    let (recording, _, cx) = open(cx, Screen::Spec, Some(snapshot), DESIGN);
    click(cx, "spec:approve");
    assert_eq!(
        *recording.acts.borrow(),
        vec![Action::ApproveSpec {
            path: DESIGN.into(),
            sha: shown,
        }]
    );
    assert!(cx.debug_bounds("spec:error").is_some());
}

#[gpui::test]
fn a_new_commit_reloads_the_text_approve_names(cx: &mut TestAppContext) {
    let (recording, view, cx) = open(cx, Screen::Spec, Some(as_sean()), DESIGN);
    let (model, before) = view.read_with(cx, |view, _| (view.model.clone(), view.code.entity_id()));
    let next: GitSha = "abcdef1".parse().unwrap();
    cx.update(|_, cx| {
        model.update(cx, |model, cx| {
            let mut plan = model.plan().cloned().unwrap();
            plan.specs[0].sha = next.clone();
            plan.reviews[0].sha = next.clone();
            plan.reviews[0].text = "# Ridgeline design v4\n".into();
            model.apply(Some(plan), cx);
        });
    });
    redraw(cx);
    assert_ne!(view.read_with(cx, |view, _| view.code.entity_id()), before);
    click(cx, "spec:approve");
    assert_eq!(
        *recording.acts.borrow(),
        vec![Action::ApproveSpec {
            path: DESIGN.into(),
            sha: next,
        }]
    );
}

#[gpui::test]
fn a_refusal_shows_only_where_it_happened(cx: &mut TestAppContext) {
    // The plan tab is open; kicking off a plan that is not ready and
    // approving the design at a stale commit are both refused.
    let stale: GitSha = "0000000".parse().unwrap();
    let mut snapshot = FixtureBackend::for_screen(Screen::Spec).unwrap().snapshot();
    snapshot.plan.as_mut().unwrap().specs[0].sha = stale.clone();
    let (recording, view, cx) = open(cx, Screen::Spec, Some(snapshot), PLAN);
    let model = view.read_with(cx, |view, _| view.model.clone());
    cx.update(|window, cx| {
        model.update(cx, |model, cx| {
            model.kick_off(window, cx);
            model.approve_spec(DESIGN, stale, cx);
        });
    });
    redraw(cx);
    assert_eq!(recording.acts.borrow().len(), 2);
    assert!(cx.debug_bounds("spec:error").is_none());
}

#[gpui::test]
fn approved_spec_shows_approved_and_request_changes(cx: &mut TestAppContext) {
    // andy approved the design at 15:47 (frame 9089).
    let (recording, _, cx) = open(cx, Screen::Spec, None, DESIGN);
    assert!(cx.debug_bounds("spec:approved").is_some());
    assert!(cx.debug_bounds("spec:approve").is_none());
    assert!(cx.debug_bounds("spec:request-changes").is_some());
    click(cx, "spec:approved");
    assert!(recording.acts.borrow().is_empty());
    // ridgeline › docs › specs › ridgeline-design.md
    assert_breadcrumb_glyphs(cx, 4);
    assert!(cx.debug_bounds("spec:code").is_some());
}

#[gpui::test]
fn draft_spec_has_no_review_buttons(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Screen::Spec, None, DRAFT);
    for selector in ["spec:approve", "spec:approved", "spec:request-changes"] {
        assert!(cx.debug_bounds(selector).is_none(), "{selector}");
    }
}

#[test]
fn breadcrumb_splits_the_path_after_the_project() {
    assert_eq!(
        breadcrumb_parts("ridgeline", DESIGN),
        ["ridgeline", "docs", "specs", "ridgeline-design.md"]
    );
}
