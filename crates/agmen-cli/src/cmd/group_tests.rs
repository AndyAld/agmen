use anyhow::anyhow;

use super::{
    Billed, BillingStep, PlanChoice, SeatDelta, absent_state_failure, billing_error, billing_step,
    no_billing_to_show, seat_change_outcome, seat_target, seats_lines,
};
use crate::client::Refusal;
use crate::output::explain;

fn refused(status: u16, code: &str, description: &str) -> anyhow::Error {
    anyhow!(Refusal {
        status,
        code: code.to_owned(),
        description: description.to_owned(),
    })
}

#[test]
fn group_seats_line_words() {
    let (seats, today) = seats_lines(2, 4, None, "trial", 5_000, 0);
    assert_eq!(seats, "seats: 2 used of 4 paid (trial, trial)");
    assert_eq!(today, "today: 0 of 5000 messages");

    let (seats, today) = seats_lines(3, 10, Some("business-10"), "active", 50_000, 9);
    assert_eq!(seats, "seats: 3 used of 10 paid (business-10, active)");
    assert_eq!(today, "today: 9 of 50000 messages");

    let (seats, _) = seats_lines(1, 0, None, "read_only", 0, 0);
    assert_eq!(seats, "seats: 1 used of 0 paid (no plan, read-only)");

    let (seats, _) = seats_lines(1, 4, Some("te\u{1b}am"), "tri\u{1b}al", 1, 1);
    assert_eq!(seats, "seats: 1 used of 4 paid (team, trial)");
    assert!(!seats.contains('\u{1b}'));
}

#[test]
fn group_seat_target_bounds() {
    assert_eq!(seat_target(4, SeatDelta::Add(1)).unwrap(), 5);
    assert_eq!(seat_target(4, SeatDelta::Remove(1)).unwrap(), 3);
    assert_eq!(seat_target(4, SeatDelta::Remove(4)).unwrap(), 0);

    let err = seat_target(4, SeatDelta::Remove(5)).unwrap_err();
    let (text, code) = explain(&err.into());
    assert_eq!(code, 2);
    assert_eq!(text, "the group has only 4 paid seats");

    let err = seat_target(0, SeatDelta::Remove(1)).unwrap_err();
    let (text, code) = explain(&err.into());
    assert_eq!(code, 2);
    assert_eq!(text, "the group has only 0 paid seats");

    let err = seat_target(u32::MAX, SeatDelta::Add(1)).unwrap_err();
    let (_, code) = explain(&err.into());
    assert_eq!(code, 2);
}

#[test]
fn group_billed_answers() {
    let line = seat_change_outcome(
        "acme",
        5,
        &Billed::Checkout {
            checkout_url: "https://pay.example/\u{1b}c".to_owned(),
        },
        Some("trial"),
    )
    .unwrap();
    assert_eq!(
        line,
        "Finish checkout to add the seats: https://pay.example/c"
    );
    assert!(!line.contains('\u{1b}'));

    let err = seat_change_outcome(
        "ac\u{1b}me",
        1,
        &Billed::Stored {
            plan: None,
            seats_paid: 0,
        },
        Some("read_only"),
    )
    .unwrap_err();
    let (text, code) = explain(&err);
    assert_eq!(code, 1);
    assert_eq!(
        text,
        "agmen: no seats changed: acme has no paid plan yet; agmen group billing starts the Team plan, which includes 4 seats"
    );

    let line = seat_change_outcome(
        "acme",
        5,
        &Billed::Stored {
            plan: Some("team".to_owned()),
            seats_paid: 5,
        },
        Some("active"),
    )
    .unwrap();
    assert_eq!(line, "seats: 5 paid (team)");
}

fn assert_billing_refusal(status: u16, code: &str, description: &str, text: &str, exit: u8) {
    let err = billing_error(
        refused(status, code, description),
        "ac\u{1b}me",
        "changing seats failed",
    );
    let (got, got_exit) = explain(&err);
    assert_eq!(got_exit, exit, "{code}: {got}");
    assert_eq!(got, text, "{code}");
    assert!(!got.contains('\u{1b}'), "{code}: {got}");
}

#[test]
fn group_billing_refusals_map() {
    for case in [
        (
            404,
            "not_hosted",
            "this hub is not agmen hosted",
            "this hub is self-hosted: no seats or billing",
            2,
        ),
        (
            404,
            "not_found",
            "this group has no seat plan",
            "acme has no seats or billing",
            2,
        ),
        (
            403,
            "group_admin_required",
            "only an owner or admin",
            "agmen: only the group's owner or an admin can do that",
            1,
        ),
        (
            403,
            "owner_required",
            "only the owner can change billing",
            "agmen: only the group's owner can change billing",
            1,
        ),
        (
            403,
            "not_a_group_member",
            "not a member",
            "you are not a member of that group",
            3,
        ),
        (
            409,
            "seats_in_use",
            "remove members first",
            "agmen: the group has more members than that: remove members first (agmen group members lists them)",
            1,
        ),
        // Both descriptions release/next sends with this code. In the second
        // the hub kept Paddle's seats and posted an owner alert, so the line
        // must not say that nothing was stored.
        (
            409,
            "seats_below_used",
            "a member joined\u{1b} while seats were changing",
            "agmen: a member joined while seats were changing; check agmen group seats and try again",
            1,
        ),
        (
            409,
            "seats_below_used",
            "paid seats are below the members in the group, and Paddle could not be restored",
            "agmen: paid seats are below the members in the group, and Paddle could not be restored; check agmen group seats and try again",
            1,
        ),
    ] {
        assert_billing_refusal(case.0, case.1, case.2, case.3, case.4);
    }
}

#[test]
fn group_billing_refusals_map_paddle() {
    for case in [
        (
            409,
            "no_subscription",
            "this group is billed outside Paddle",
            "agmen: this group is billed outside Paddle",
            1,
        ),
        (
            409,
            "no_subscription",
            "the subscription is not active yet; try again in a minute",
            "agmen: the subscription is not active yet; try again in a minute",
            1,
        ),
        (
            409,
            "no_subscription",
            "this group has no subscription yet: agmen group seats add 1 starts one",
            "agmen: this group has no subscription yet: agmen group seats add 1 starts one",
            1,
        ),
        (
            422,
            "seats_over_50",
            "more than 50 seats is custom: write to sales@getagmen.ai",
            "agmen: more than 50 seats is custom: write to sales@getagmen.ai",
            1,
        ),
        (
            429,
            "rate_limited",
            "slow down",
            "agmen: Paddle calls for this group are limited; try again in a minute",
            1,
        ),
        (
            502,
            "paddle_bad_url",
            "bad url",
            "agmen: Paddle returned a link the hub will not pass on; nothing changed, try again later",
            1,
        ),
        (
            502,
            "upstream_unavailable",
            "timeout",
            "agmen: Paddle did not answer; try again later",
            1,
        ),
        (
            500,
            "server_error",
            "internal error",
            "agmen: changing seats failed: internal error (the hub answered 500 server_error)",
            1,
        ),
    ] {
        assert_billing_refusal(case.0, case.1, case.2, case.3, case.4);
    }
}

#[test]
fn group_billing_step_by_state() {
    assert_eq!(
        billing_step(Some("active"), None, None),
        BillingStep::Portal
    );
    assert_eq!(
        billing_step(Some("past_due"), None, None),
        BillingStep::Portal
    );
    assert_eq!(
        billing_step(Some("trial"), None, None),
        BillingStep::Plan {
            name: "team",
            seats: None
        }
    );
    assert_eq!(
        billing_step(Some("read_only"), None, None),
        BillingStep::PortalThenPlan
    );
    assert_eq!(
        billing_step(Some("active"), Some(PlanChoice::Business), Some(12)),
        BillingStep::Plan {
            name: "business",
            seats: Some(12)
        }
    );
    assert_eq!(
        billing_step(Some("trial"), Some(PlanChoice::Team), None),
        BillingStep::Plan {
            name: "team",
            seats: None
        }
    );
    assert_eq!(billing_step(None, None, None), BillingStep::Absent);
    // A group with no plan row has nothing to bill, so `--plan` does not skip
    // the self-host and no-plan answers (coordinator ruling 2).
    assert_eq!(
        billing_step(None, Some(PlanChoice::Team), None),
        BillingStep::Absent
    );
    assert_eq!(
        billing_step(None, Some(PlanChoice::Business), Some(10)),
        BillingStep::Absent
    );
    assert_eq!(
        billing_step(Some("deleted"), None, None),
        BillingStep::Unsupported
    );

    let (text, code) = explain(&absent_state_failure(
        refused(404, "not_hosted", "this hub is not agmen hosted"),
        "acme",
    ));
    assert_eq!(code, 2);
    assert_eq!(text, "this hub is self-hosted: no seats or billing");

    let (text, code) = explain(&absent_state_failure(
        refused(404, "not_found", "this group has no seat plan"),
        "acme",
    ));
    assert_eq!(code, 2);
    assert_eq!(
        text,
        "this group has no paid plan, so it has no seats or billing"
    );

    let (text, code) = explain(&no_billing_to_show("ac\u{1b}me", "deleted"));
    assert_eq!(code, 1);
    assert_eq!(text, "agmen: acme is deleted: no billing to show");
}
