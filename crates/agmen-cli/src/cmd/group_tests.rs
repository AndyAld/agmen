use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use super::{
    Billed, BillingStep, PlanChoice, SeatDelta, absent_state_failure, billing_error, billing_step,
    no_billing_to_show, portal_then_plan, repay_choice, seat_change_outcome, seat_target,
    seats_lines,
};
use crate::client::{Api, Group, Human, Login, Refusal};
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
fn group_repay_choice_team_preserves_paid_seats() {
    assert_eq!(repay_choice(Some("team"), 6), ("team", Some(6)));
}

#[test]
fn group_repay_choice_business_25_preserves_paid_seats() {
    assert_eq!(
        repay_choice(Some("business-25"), 25),
        ("business", Some(25))
    );
}

#[test]
fn group_repay_choice_business_50_preserves_paid_seats() {
    assert_eq!(
        repay_choice(Some("business-50"), 50),
        ("business", Some(50))
    );
}

#[test]
fn group_repay_choice_null_starts_team() {
    assert_eq!(repay_choice(None, 0), ("team", None));
}

#[test]
fn group_repay_choice_unknown_starts_team() {
    assert_eq!(repay_choice(Some("unknown"), 25), ("team", None));
}

/// Runs the read-only re-pay against a stub hub whose portal answers 409
/// `no_subscription`, whose `GET /seats` answers `seats` and whose `POST /plan`
/// answers `plan`. Returns the outcome and every body `/plan` received.
async fn repay_against_stub(
    seats: (u16, Value),
    plan: (u16, Value),
) -> (anyhow::Result<()>, Vec<Value>) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let recorded = sent.clone();
    let reply = |(status, body): (u16, Value)| (StatusCode::from_u16(status).unwrap(), Json(body));
    let app = Router::new()
        .route(
            "/v1/tenants/tenant01/portal",
            post(move || async move { reply((409, json!({"error": "no_subscription"}))) }),
        )
        .route(
            "/v1/tenants/tenant01/seats",
            get(move || async move { reply(seats) }),
        )
        .route(
            "/v1/tenants/tenant01/plan",
            post(move |Json(body): Json<Value>| async move {
                recorded.lock().unwrap().push(body);
                reply(plan)
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let login = Login {
        hub_url: format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap(),
        human_id: "hmn7k2q9".parse().unwrap(),
        handle: "andy".to_owned(),
    };
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let api = Api::new(login.hub_url.clone(), "token").unwrap();
    let human = Human { login, api };
    let group: Group = serde_json::from_value(
        json!({"tenant": "tenant01", "name": "andy/owed", "role": "owner", "state": "read_only"}),
    )
    .unwrap();
    let outcome =
        tokio::time::timeout(Duration::from_secs(5), portal_then_plan(&human, &group)).await;
    server.abort();
    let outcome = outcome.expect("the re-pay finishes within 5 seconds");
    (outcome, sent.lock().unwrap().clone())
}

/// `GET /seats` for a read-only group. `used` differs from every `paid` here,
/// so a body built from the wrong field shows.
fn seats_answer(plan: Option<&str>, paid: u32) -> (u16, Value) {
    let body = json!({
        "used": 2, "paid": paid, "plan": plan, "state": "read_only", "quota": 0, "used_today": 0
    });
    (200, body)
}

#[tokio::test]
async fn group_repay_wiring_posts_previous_plan_and_paid_seats() {
    let checkout = || (200, json!({"checkout_url": "https://pay.example/c"}));
    let plan_body = |plan: &str, seats: u32| json!({"plan": plan, "seats": seats});
    for (stored, paid, want) in [
        (Some("team"), 6, plan_body("team", 6)),
        (Some("business-10"), 10, plan_body("business", 10)),
        (Some("business-25"), 25, plan_body("business", 25)),
        (Some("business-50"), 50, plan_body("business", 50)),
        // No known plan: today's body, with no `seats` key.
        (None, 0, json!({"plan": "team"})),
        (Some("mystery"), 7, json!({"plan": "team"})),
    ] {
        let (outcome, sent) = repay_against_stub(seats_answer(stored, paid), checkout()).await;
        outcome.unwrap();
        assert_eq!(sent, [want], "plan {stored:?} paid {paid}");
    }
}

#[tokio::test]
async fn group_repay_wiring_refusals_use_billing_errors() {
    let checkout = || (200, json!({"checkout_url": "https://pay.example/c"}));
    // A failed seats read sends no /plan.
    for (status, code, want) in [
        (500, "server_error", "reading seats failed"),
        (404, "not_found", "has no seats or billing"),
        (429, "rate_limited", "limited"),
    ] {
        let seats = (status, json!({"error": code}));
        let (outcome, sent) = repay_against_stub(seats, checkout()).await;
        let text = explain(&outcome.expect_err(&format!("{status} {code} must fail"))).0;
        assert!(text.contains(want), "{status} {code}: {text}");
        assert!(sent.is_empty(), "{status} {code}: {sent:?}");
    }
    // A refused /plan reaches the user through the billing mapping.
    let plan_refusal = (409, json!({"error": "seats_in_use"}));
    let seats = seats_answer(Some("business-10"), 10);
    let (outcome, sent) = repay_against_stub(seats, plan_refusal).await;
    let text = explain(&outcome.unwrap_err()).0;
    assert!(text.contains("remove members first"), "{text}");
    assert_eq!(sent.len(), 1);
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
