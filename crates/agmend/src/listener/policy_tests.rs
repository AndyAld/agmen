use super::*;

fn settings() -> Settings<'static> {
    Settings::new("andy", &["adam/coord"], &["engineer", "reviewer"])
}

fn request() -> DelegateRequest<'static> {
    DelegateRequest {
        agent: None,
        role: "engineer",
        model: Model::Tier(Tier::Standard),
        budget: UsdCents(500),
    }
}

#[test]
fn defaults_match_delegation_policy() {
    let policy = settings();
    assert_eq!(policy.per_delegation, UsdCents(500));
    assert_eq!(policy.daily, UsdCents(2_000));
    assert_eq!(policy.max_workers, 3);
    assert!(policy.accept_from.is_empty());
}

#[test]
fn per_delegation_boundary() {
    for (cents, expected) in [
        (0, Decision::Allow),
        (499, Decision::Allow),
        (500, Decision::Allow),
        (501, Decision::Pending(PendingReason::PerDelegation)),
        (600, Decision::Pending(PendingReason::PerDelegation)),
    ] {
        let req = DelegateRequest {
            budget: UsdCents(cents),
            ..request()
        };
        assert_eq!(
            check_delegate(&req, "andy", &settings(), UsdCents(0), 0),
            expected
        );
    }
}

#[test]
fn daily_boundary_includes_requested_budget() {
    for (spent, expected) in [
        (1_499, Decision::Allow),
        (1_500, Decision::Allow),
        (1_501, Decision::Pending(PendingReason::Daily)),
        (2_000, Decision::Pending(PendingReason::Daily)),
        (u64::MAX, Decision::Pending(PendingReason::Daily)),
    ] {
        assert_eq!(
            check_delegate(&request(), "andy", &settings(), UsdCents(spent), 0),
            expected
        );
    }
}

#[test]
fn worker_boundary_includes_new_worker() {
    for (running, expected) in [
        (0, Decision::Allow),
        (2, Decision::Allow),
        (3, Decision::Pending(PendingReason::MaxWorkers)),
        (4, Decision::Pending(PendingReason::MaxWorkers)),
        (usize::MAX, Decision::Pending(PendingReason::MaxWorkers)),
    ] {
        assert_eq!(
            check_delegate(&request(), "andy", &settings(), UsdCents(0), running),
            expected
        );
    }
}

#[test]
fn owner_and_coordinators_always_reach_listener() {
    for from in ["andy", "adam/coord"] {
        assert_eq!(
            check_delegate(&request(), from, &settings(), UsdCents(0), 0),
            Decision::Allow
        );
    }
}

#[test]
fn stranger_is_refused_even_when_over_cap() {
    assert_eq!(
        check_delegate(&request(), "sean", &settings(), UsdCents(2_000), 3),
        Decision::Refuse(RefuseReason::Requester)
    );
}

#[test]
fn project_accept_from_adds_requester_without_removing_owner_or_coordinators() {
    let policy = Settings {
        accept_from: &["sean"],
        ..settings()
    };
    for from in ["andy", "adam/coord", "sean"] {
        assert_eq!(
            check_delegate(&request(), from, &policy, UsdCents(0), 0),
            Decision::Allow
        );
    }
}

#[test]
fn coordinator_work_uses_same_caps() {
    let req = DelegateRequest {
        budget: UsdCents(600),
        ..request()
    };
    assert_eq!(
        check_delegate(&req, "adam/coord", &settings(), UsdCents(0), 0),
        Decision::Pending(PendingReason::PerDelegation)
    );
}

#[test]
fn role_outside_resolved_worker_roles_is_refused_before_caps() {
    for role in ["listener", "coordinator", "unknown", ""] {
        let req = DelegateRequest { role, ..request() };
        assert_eq!(
            check_delegate(&req, "andy", &settings(), UsdCents(2_000), 3),
            Decision::Refuse(RefuseReason::Role)
        );
    }
}

#[test]
fn project_roles_replace_resolved_worker_roles() {
    let policy = Settings {
        worker_roles: &["custom-worker"],
        ..settings()
    };
    assert_eq!(
        check_delegate(&request(), "andy", &policy, UsdCents(0), 0),
        Decision::Refuse(RefuseReason::Role)
    );
    let req = DelegateRequest {
        role: "custom-worker",
        ..request()
    };
    assert_eq!(
        check_delegate(&req, "andy", &policy, UsdCents(0), 0),
        Decision::Allow
    );
}

#[test]
fn project_caps_replace_defaults() {
    let policy = Settings {
        per_delegation: UsdCents(800),
        daily: UsdCents(3_000),
        max_workers: 5,
        ..settings()
    };
    let req = DelegateRequest {
        budget: UsdCents(800),
        ..request()
    };
    assert_eq!(
        check_delegate(&req, "andy", &policy, UsdCents(2_200), 4),
        Decision::Allow
    );
    let req = DelegateRequest {
        budget: UsdCents(801),
        ..req
    };
    assert_eq!(
        check_delegate(&req, "andy", &policy, UsdCents(0), 0),
        Decision::Pending(PendingReason::PerDelegation)
    );
    assert_eq!(
        check_delegate(&request(), "andy", &policy, UsdCents(2_501), 0),
        Decision::Pending(PendingReason::Daily)
    );
    assert_eq!(
        check_delegate(&request(), "andy", &policy, UsdCents(0), 5),
        Decision::Pending(PendingReason::MaxWorkers)
    );
}

#[test]
fn zero_project_caps_require_approval() {
    for (policy, reason) in [
        (
            Settings {
                per_delegation: UsdCents(0),
                ..settings()
            },
            PendingReason::PerDelegation,
        ),
        (
            Settings {
                daily: UsdCents(0),
                ..settings()
            },
            PendingReason::Daily,
        ),
        (
            Settings {
                max_workers: 0,
                ..settings()
            },
            PendingReason::MaxWorkers,
        ),
    ] {
        assert_eq!(
            check_delegate(&request(), "andy", &policy, UsdCents(0), 0),
            Decision::Pending(reason)
        );
    }
}

#[test]
fn agent_and_model_choice_do_not_bypass_caps() {
    for model in [
        Model::Tier(Tier::Small),
        Model::Tier(Tier::Standard),
        Model::Tier(Tier::Deep),
        Model::Named("project-model"),
    ] {
        let req = DelegateRequest {
            agent: Some("claude"),
            model,
            ..request()
        };
        assert_eq!(req.agent, Some("claude"));
        assert_eq!(req.model, model);
        assert_eq!(
            check_delegate(&req, "andy", &settings(), UsdCents(0), 0),
            Decision::Allow
        );
        assert_eq!(
            check_delegate(&req, "andy", &settings(), UsdCents(2_000), 0),
            Decision::Pending(PendingReason::Daily)
        );
    }
}
