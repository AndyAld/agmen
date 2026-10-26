use super::*;
use crate::backend::{Backend as _, FixtureBackend, LaneInfo, Screen};

fn plan(screen: Screen) -> PlanInfo {
    FixtureBackend::for_screen(screen)
        .unwrap()
        .snapshot()
        .plan
        .unwrap()
}

fn labels(checks: &[Check]) -> Vec<(&str, bool)> {
    checks.iter().map(|c| (c.label.as_str(), c.ok)).collect()
}

#[test]
fn lanes_overlap_detected() {
    assert!(paths_overlap("core/**", "core/x/**"));
    assert!(paths_overlap("core/x/**", "core/**"));
    assert!(!paths_overlap("core/**", "cli/**"));
    assert!(paths_overlap("core/**", "core/lib.rs"));
    assert!(!paths_overlap("core/lib.rs", "core/main.rs"));
    assert!(paths_overlap("core/lib.rs", "./core//lib.rs"));
    assert!(!paths_overlap("core/lib.rs", "core/lib.rs/x"));
    assert!(paths_overlap("*/src/**", "core/src/lib.rs"));
    assert!(paths_overlap("core/", "core/x/y"));
    assert!(paths_overlap("**", "anything/at/all"));
    // A path past the segment limit is treated as covering everything below
    // the limit, so a too-deep path can only make the check stricter.
    let deep = "a/".repeat(MAX_SEGMENTS + 10);
    assert!(paths_overlap(&deep, &format!("{deep}b")));

    let mut p = plan(Screen::PlanReady);
    assert!(kickoff_enabled(&p));
    p.lanes.push(LaneInfo {
        name: "L-MAPS".into(),
        paths: vec!["docs/**".into(), "core/maps/**".into()],
    });
    let lanes = &checklist(&p, "andy")[1];
    assert_eq!(
        (lanes.label.as_str(), lanes.ok),
        ("Lanes L-CORE and L-MAPS overlap", false)
    );
    assert!(!kickoff_enabled(&p));
    assert_eq!(kickoff_label(&p), "Kick off · lanes overlap");
}

#[test]
fn checklist_matches_the_plan_panel() {
    assert_eq!(
        labels(&checklist(&plan(Screen::Plan), "andy")),
        [
            ("Repo ridgeline exists on the git host", true),
            ("Lanes don't overlap", true),
            ("Specs approved by andy and sean", false),
            ("andy approves 4 agents, $65/day", true),
            ("sean approves 3 agents, $40/day", false),
        ]
    );
    let ready = checklist(&plan(Screen::PlanReady), "andy");
    assert!(ready.iter().all(|c| c.ok), "{ready:?}");
    // My share comes first.
    let sean = checklist(&plan(Screen::Plan), "sean");
    assert_eq!(sean[3].label, "sean approves 3 agents, $40/day");
    assert_eq!(sean[4].label, "andy approves 4 agents, $65/day");
}

#[test]
fn kickoff_disabled_until_all_approve() {
    let draft = plan(Screen::Plan);
    assert!(!kickoff_enabled(&draft));
    assert_eq!(kickoff_label(&draft), "Kick off · waiting on sean");
    for screen in [Screen::PlanReady, Screen::KickOff] {
        let p = plan(screen);
        assert!(kickoff_enabled(&p), "{screen:?}");
        assert_eq!(kickoff_label(&p), "Kick off · 7 agents");
    }
    // Everyone approved, but the plan branch is not merged yet.
    let mut unmerged = plan(Screen::PlanReady);
    unmerged.branch_merged = false;
    assert!(!kickoff_enabled(&unmerged));
    assert_eq!(
        kickoff_label(&unmerged),
        "Kick off · waiting on the plan branch merge"
    );
    let mut no_repo = plan(Screen::PlanReady);
    no_repo.repo_exists = false;
    assert!(!kickoff_enabled(&no_repo));
    assert_eq!(kickoff_label(&no_repo), "Kick off · no repo");
    // A plan that is running is not kicked off again.
    assert!(!kickoff_enabled(&plan(Screen::Running)));
    // Several people waiting are all named.
    let mut two = plan(Screen::Plan);
    two.shares_approved.insert("andy".into(), false);
    assert_eq!(kickoff_label(&two), "Kick off · waiting on andy and sean");
}

#[test]
fn my_share_sums_my_agents() {
    let p = plan(Screen::Plan);
    assert_eq!(
        share(&p, "andy"),
        Share {
            agents: 4,
            usd_per_day: 65
        }
    );
    assert_eq!(
        share(&p, "sean"),
        Share {
            agents: 3,
            usd_per_day: 40
        }
    );
    assert_eq!(share(&p, "adam"), Share::default());
}

#[test]
fn spec_approve_dispatches_decision() {
    // Dispatching needs a view, and Action::ApproveSpec needs the spec's sha,
    // which SpecState does not carry yet; this tests the state change.
    let mut p = plan(Screen::Plan);
    let design = "docs/specs/ridgeline-design.md";
    let approved = |p: &PlanInfo| {
        p.specs
            .iter()
            .find(|s| s.path == design)
            .unwrap()
            .approved_by_all()
    };
    assert!(!approved(&p));
    assert!(approve_spec(&mut p, design, "sean"));
    assert!(approved(&p));
    // Approving again, an unknown file, someone who is not an approver, or a
    // draft changes nothing.
    assert!(!approve_spec(&mut p, design, "sean"));
    assert!(!approve_spec(&mut p, "docs/nope.md", "sean"));
    assert!(!approve_spec(&mut p, design, "adam"));
    assert!(!approve_spec(&mut p, "docs/specs/tile-sources.md", "sean"));
    // Specs are approved now; sean's share still waits.
    assert!(checklist(&p, "andy")[2].ok);
    assert_eq!(kickoff_label(&p), "Kick off · waiting on sean");
}

#[test]
fn names_join_in_plain_english() {
    assert_eq!(join_names::<&str>(&[]), "");
    assert_eq!(join_names(&["andy"]), "andy");
    assert_eq!(join_names(&["andy", "sean"]), "andy and sean");
    assert_eq!(join_names(&["adam", "andy", "sean"]), "adam, andy and sean");
}
