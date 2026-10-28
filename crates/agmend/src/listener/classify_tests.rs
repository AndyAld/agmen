use super::*;
use crate::listener::policy::{Settings, UsdCents};

fn settings() -> ChangeSettings<'static> {
    ChangeSettings {
        listener: Settings::new("andy", &["adam/coord"], &["engineer", "reviewer"]),
        listener_daily: UsdCents(200),
        budget: UsdCents(500),
        tools: &["read", "search"],
        lanes: &["backend", "frontend"],
        lends: &["lend-1", "lend-2"],
        push_approval: PushApproval::Required,
        state: AgentState::Stopped,
    }
}

type Mutation = fn(&mut ChangeSettings<'static>);

fn cases() -> [(Field, Mutation, Mutation); 12] {
    [
        (
            Field::PerDelegation,
            |s| s.listener.per_delegation = UsdCents(499),
            |s| s.listener.per_delegation = UsdCents(501),
        ),
        (
            Field::Daily,
            |s| s.listener.daily = UsdCents(1_999),
            |s| s.listener.daily = UsdCents(2_001),
        ),
        (
            Field::MaxWorkers,
            |s| s.listener.max_workers = 2,
            |s| s.listener.max_workers = 4,
        ),
        (
            Field::AcceptFrom,
            |s| s.listener.accept_from = &[],
            |s| s.listener.accept_from = &["sean"],
        ),
        (
            Field::WorkerRoles,
            |s| s.listener.worker_roles = &["engineer"],
            |s| s.listener.worker_roles = &["engineer", "reviewer", "tester"],
        ),
        (
            Field::ListenerDaily,
            |s| s.listener_daily = UsdCents(199),
            |s| s.listener_daily = UsdCents(201),
        ),
        (
            Field::Budget,
            |s| s.budget = UsdCents(499),
            |s| s.budget = UsdCents(501),
        ),
        (
            Field::Tools,
            |s| s.tools = &["read"],
            |s| s.tools = &["read", "search", "shell"],
        ),
        (
            Field::Lanes,
            |s| s.lanes = &["backend"],
            |s| s.lanes = &["backend", "frontend", "infra"],
        ),
        (
            Field::Lends,
            |s| s.lends = &["lend-1"],
            |s| s.lends = &["lend-1", "lend-2", "lend-3"],
        ),
        (
            Field::PushApproval,
            |s| s.push_approval = PushApproval::Required,
            |s| s.push_approval = PushApproval::Disabled,
        ),
        (
            Field::AgentState,
            |s| s.state = AgentState::Stopped,
            |s| s.state = AgentState::Running,
        ),
    ]
}

#[test]
fn each_field_tightened_loosened_and_unchanged() {
    for (field, tighten, loosen) in cases() {
        let mut before = settings();
        // Give the two binary settings and accept_from something to remove.
        before.listener.accept_from = &["sean"];
        before.push_approval = PushApproval::Disabled;
        before.state = AgentState::Running;
        let mut after = before;
        tighten(&mut after);
        assert_eq!(classify(&before, &after), Direction::Narrow, "{field:?}");
        assert_eq!(
            classify(&after, &before),
            Direction::Widen(vec![field]),
            "{field:?}"
        );

        let before = settings();
        let mut after = before;
        loosen(&mut after);
        assert_eq!(
            classify(&before, &after),
            Direction::Widen(vec![field]),
            "{field:?}"
        );
        assert_eq!(classify(&before, &before), Direction::Narrow, "{field:?}");
    }
}

#[test]
fn mixed_change_reports_every_widening_field_in_declaration_order() {
    let before = settings();
    let mut after = before;
    after.listener.per_delegation = UsdCents(1);
    after.listener.daily = UsdCents(3_000);
    after.tools = &["read", "shell"];
    after.lanes = &[];
    after.budget = UsdCents(600);
    after.push_approval = PushApproval::Disabled;
    assert_eq!(
        classify(&before, &after),
        Direction::Widen(vec![
            Field::Daily,
            Field::Budget,
            Field::Tools,
            Field::PushApproval
        ])
    );
}

#[test]
fn all_fields_widen_without_short_circuiting() {
    let before = settings();
    let mut after = before;
    for (_, _, loosen) in cases() {
        loosen(&mut after);
    }
    assert_eq!(
        classify(&before, &after),
        Direction::Widen(cases().map(|(field, _, _)| field).to_vec())
    );
}

#[test]
fn empty_change_and_empty_sets_are_narrow() {
    let mut before = settings();
    before.listener.worker_roles = &[];
    before.tools = &[];
    before.lanes = &[];
    before.lends = &[];
    assert_eq!(classify(&before, &before), Direction::Narrow);
}

#[test]
fn sets_ignore_order_and_duplicates_but_detect_replacement() {
    let before = settings();
    let mut after = before;
    after.listener.worker_roles = &["reviewer", "engineer", "engineer"];
    after.tools = &["search", "read", "read"];
    after.lanes = &["frontend", "backend", "backend"];
    after.lends = &["lend-2", "lend-1", "lend-1"];
    assert_eq!(classify(&before, &after), Direction::Narrow);
    assert_eq!(classify(&after, &before), Direction::Narrow);
    after.tools = &["shell"];
    assert_eq!(
        classify(&before, &after),
        Direction::Widen(vec![Field::Tools])
    );
}

#[test]
fn owner_and_coordinators_are_already_effective_accepted_senders() {
    let before = settings();
    let mut after = before;
    after.listener.accept_from = &["andy", "adam/coord", "andy"];
    assert_eq!(classify(&before, &after), Direction::Narrow);
    assert_eq!(classify(&after, &before), Direction::Narrow);
    after.listener.accept_from = &["andy", "adam/coord", "sean"];
    assert_eq!(
        classify(&before, &after),
        Direction::Widen(vec![Field::AcceptFrom])
    );
}

#[test]
fn numeric_boundaries_do_not_overflow() {
    let mut zero = settings();
    zero.listener.per_delegation = UsdCents(0);
    zero.listener.daily = UsdCents(0);
    zero.listener.max_workers = 0;
    zero.listener_daily = UsdCents(0);
    zero.budget = UsdCents(0);
    let mut max = zero;
    max.listener.per_delegation = UsdCents(u64::MAX);
    max.listener.daily = UsdCents(u64::MAX);
    max.listener.max_workers = usize::MAX;
    max.listener_daily = UsdCents(u64::MAX);
    max.budget = UsdCents(u64::MAX);
    assert_eq!(classify(&max, &zero), Direction::Narrow);
    assert_eq!(classify(&zero, &zero), Direction::Narrow);
    assert_eq!(classify(&max, &max), Direction::Narrow);
    assert_eq!(
        classify(&zero, &max),
        Direction::Widen(vec![
            Field::PerDelegation,
            Field::Daily,
            Field::MaxWorkers,
            Field::ListenerDaily,
            Field::Budget
        ])
    );
}
