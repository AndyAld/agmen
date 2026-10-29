use std::time::Duration;

use agmen_proto::Timestamp;

use super::{
    BillingEvent, BusinessStep, MAX_OWNED_GROUPS, PAID_READ_ONLY_DAYS, PAST_DUE_GRACE_DAYS, Plan,
    PlanEffect, PlanRecord, PlanState, ReadOnlyCause, Retention, TEAM_INCLUDED_SEATS, TRIAL_DAYS,
    TRIAL_READ_ONLY_DAYS, TRIAL_REMINDER_OFFSETS_DAYS,
};
use crate::billing::{Catalog, LineItem};

fn at(text: &str) -> Timestamp {
    text.parse().expect("timestamp")
}

fn after(start: Timestamp, days: u32) -> Timestamp {
    start + Duration::from_secs(u64::from(days) * 86_400)
}

fn active(plan: Plan) -> PlanRecord {
    PlanRecord {
        state: PlanState::Active,
        plan: Some(plan),
        trial_started_at: None,
        past_due_since: None,
        read_only_cause: None,
        read_only_since: None,
        reminders_sent: 0,
        last_event_at: None,
    }
}

#[test]
fn plan_state_round_trips() {
    for state in [
        PlanState::Trial,
        PlanState::Active,
        PlanState::PastDue,
        PlanState::ReadOnly,
        PlanState::Deleted,
    ] {
        assert_eq!(state.as_str().parse::<PlanState>().unwrap(), state);
        assert_eq!(state.to_string(), state.as_str());
    }
    for bad in ["", "past-due", "PastDue", "trial "] {
        assert!(bad.parse::<PlanState>().is_err(), "{bad}");
    }
}

#[test]
fn trial_has_four_seats_and_5000_messages() {
    assert_eq!(TRIAL_DAYS, 7);
    assert_eq!(TRIAL_REMINDER_OFFSETS_DAYS, [4, 6]);
    assert_eq!(TRIAL_READ_ONLY_DAYS, 14);
    assert_eq!(TEAM_INCLUDED_SEATS, 4);
    assert_eq!(MAX_OWNED_GROUPS, 3);

    let t0 = at("2026-11-02T15:04:05Z");
    let trial = PlanRecord::trial(t0);
    assert_eq!(trial.state, PlanState::Trial);
    assert_eq!(trial.seats_paid(), 4);
    assert_eq!(trial.daily_quota(), 5_000);
    assert_eq!(trial.retention(), Retention::Days(90));
    assert!(trial.writable());
    assert_eq!(trial.trial_ends_at(), Some(after(t0, 7)));
    assert_eq!(trial.read_only_at(), Some(after(t0, 7)));
    assert_eq!(trial.delete_at(), Some(after(t0, 21)));
}

#[test]
fn team_quota_and_seats() {
    let included = active(Plan::Team { extra_seats: 0 });
    assert_eq!(included.seats_paid(), 4);
    assert_eq!(included.daily_quota(), 10_000);
    assert_eq!(included.retention(), Retention::Days(90));
    assert!(included.read_only_at().is_none());
    assert!(included.delete_at().is_none());

    let extra = active(Plan::Team { extra_seats: 3 });
    assert_eq!(extra.seats_paid(), 7);
    assert_eq!(extra.daily_quota(), 13_000);
    assert_eq!(extra.retention(), Retention::Days(90));
}

#[test]
fn business_steps() {
    for (step, seats, quota) in [
        (BusinessStep::Seats10, 10, 50_000),
        (BusinessStep::Seats25, 25, 125_000),
        (BusinessStep::Seats50, 50, 250_000),
    ] {
        assert_eq!(step.seats(), seats);
        let record = active(Plan::Business(step));
        assert_eq!(record.seats_paid(), seats);
        assert_eq!(record.daily_quota(), quota);
        assert_eq!(record.retention(), Retention::Days(365));
    }
}

#[test]
fn step_for_seats() {
    assert_eq!(BusinessStep::for_seats(1), Some(BusinessStep::Seats10));
    assert_eq!(BusinessStep::for_seats(10), Some(BusinessStep::Seats10));
    assert_eq!(BusinessStep::for_seats(11), Some(BusinessStep::Seats25));
    assert_eq!(BusinessStep::for_seats(25), Some(BusinessStep::Seats25));
    assert_eq!(BusinessStep::for_seats(26), Some(BusinessStep::Seats50));
    assert_eq!(BusinessStep::for_seats(50), Some(BusinessStep::Seats50));
    assert_eq!(BusinessStep::for_seats(51), None);
}

#[test]
fn trial_reminders_on_days_5_and_7() {
    let t0 = at("2026-11-02T15:04:05Z");
    let ends_at = after(t0, 7);
    let (once, effects) = PlanRecord::trial(t0).tick(after(t0, 4));
    assert_eq!(effects, vec![PlanEffect::TrialReminder { day: 5, ends_at }]);
    assert_eq!(once.reminders_sent, 1);
    assert_eq!(once.state, PlanState::Trial);

    let (again, effects) = once.tick(after(t0, 4));
    assert!(effects.is_empty());
    assert_eq!(again, once);

    let (day7, effects) = again.tick(after(t0, 6));
    assert_eq!(effects, vec![PlanEffect::TrialReminder { day: 7, ends_at }]);
    assert_eq!(day7.reminders_sent, 2);

    let (both, effects) = PlanRecord::trial(t0).tick(after(t0, 6));
    assert_eq!(
        effects,
        vec![
            PlanEffect::TrialReminder { day: 5, ends_at },
            PlanEffect::TrialReminder { day: 7, ends_at },
        ]
    );
    assert_eq!(both.reminders_sent, 2);
}

#[test]
fn trial_end_is_read_only_for_14_days() {
    let t0 = at("2026-11-02T15:04:05Z");
    let (read_only, effects) = PlanRecord::trial(t0).tick(after(t0, 7));
    assert_eq!(read_only.state, PlanState::ReadOnly);
    assert_eq!(read_only.read_only_cause, Some(ReadOnlyCause::TrialEnded));
    assert_eq!(read_only.read_only_since, Some(after(t0, 7)));
    assert!(!read_only.writable());
    assert_eq!(read_only.daily_quota(), 0);
    assert_eq!(read_only.delete_at(), Some(after(t0, 21)));
    assert_eq!(
        effects,
        vec![PlanEffect::BecameReadOnly(ReadOnlyCause::TrialEnded)]
    );

    let (late, _) = PlanRecord::trial(t0).tick(after(t0, 10));
    assert_eq!(late.read_only_since, Some(after(t0, 7)));
    assert_eq!(late.delete_at(), Some(after(t0, 21)));

    let (deleted, effects) = read_only.tick(after(t0, 21));
    assert_eq!(deleted.state, PlanState::Deleted);
    assert_eq!(effects, vec![PlanEffect::Delete]);

    let (jumped, effects) = PlanRecord::trial(t0).tick(after(t0, 21));
    assert_eq!(jumped.state, PlanState::Deleted);
    assert_eq!(
        effects,
        vec![
            PlanEffect::BecameReadOnly(ReadOnlyCause::TrialEnded),
            PlanEffect::Delete,
        ]
    );
}

#[test]
fn awaiting_payment_deletes_after_14_days() {
    let t0 = at("2026-11-02T15:04:05Z");
    let waiting = PlanRecord::awaiting_payment(t0);
    assert_eq!(waiting.state, PlanState::ReadOnly);
    assert_eq!(
        waiting.read_only_cause,
        Some(ReadOnlyCause::AwaitingPayment)
    );
    assert_eq!(waiting.read_only_at(), Some(t0));
    assert_eq!(waiting.delete_at(), Some(after(t0, 14)));
    assert!(!waiting.writable());
    assert_eq!(waiting.daily_quota(), 0);

    let (same, effects) = waiting.tick(after(t0, 13));
    assert_eq!(same, waiting);
    assert!(effects.is_empty());

    let (deleted, effects) = waiting.tick(after(t0, 14));
    assert_eq!(deleted.state, PlanState::Deleted);
    assert_eq!(effects, vec![PlanEffect::Delete]);
}

#[test]
fn payment_failure_timeline() {
    assert_eq!(PAST_DUE_GRACE_DAYS, 14);
    assert_eq!(PAID_READ_ONLY_DAYS, 90);

    let t1 = at("2026-12-01T15:04:05Z");
    let past_due = active(Plan::Team { extra_seats: 0 }).apply(BillingEvent::PastDue, t1);
    assert_eq!(past_due.state, PlanState::PastDue);
    assert_eq!(past_due.past_due_since, Some(t1));
    assert!(past_due.writable());
    assert_eq!(past_due.daily_quota(), 10_000);
    assert_eq!(past_due.read_only_at(), Some(after(t1, 14)));
    assert_eq!(past_due.delete_at(), Some(after(t1, 104)));

    let (read_only, effects) = past_due.tick(after(t1, 14));
    assert_eq!(read_only.state, PlanState::ReadOnly);
    assert_eq!(
        read_only.read_only_cause,
        Some(ReadOnlyCause::PaymentFailed)
    );
    assert_eq!(read_only.delete_at(), Some(after(t1, 104)));
    assert!(!read_only.writable());
    assert_eq!(read_only.daily_quota(), 0);
    assert_eq!(
        effects,
        vec![PlanEffect::BecameReadOnly(ReadOnlyCause::PaymentFailed)]
    );

    let (late, _) = past_due.tick(after(t1, 20));
    assert_eq!(late.read_only_since, Some(after(t1, 14)));
    assert_eq!(late.delete_at(), Some(after(t1, 104)));

    let (deleted, effects) = read_only.tick(after(t1, 104));
    assert_eq!(deleted.state, PlanState::Deleted);
    assert_eq!(effects, vec![PlanEffect::Delete]);
}

#[test]
fn paying_restores() {
    let t0 = at("2026-11-02T15:04:05Z");
    let (read_only, _) = PlanRecord::trial(t0).tick(after(t0, 7));
    let restored = read_only.apply(
        BillingEvent::Active(Plan::Business(BusinessStep::Seats10)),
        after(t0, 8),
    );
    assert_eq!(restored.state, PlanState::Active);
    assert_eq!(restored.plan, Some(Plan::Business(BusinessStep::Seats10)));
    assert_eq!(restored.read_only_cause, None);
    assert_eq!(restored.read_only_since, None);
    assert!(restored.writable());
    assert_eq!(restored.daily_quota(), 50_000);

    let team = Plan::Team { extra_seats: 3 };
    let t1 = at("2026-12-01T15:04:05Z");
    let (failed, _) = active(team)
        .apply(BillingEvent::PastDue, t1)
        .tick(after(t1, 14));
    assert_eq!(failed.read_only_cause, Some(ReadOnlyCause::PaymentFailed));
    let paid = failed.apply(BillingEvent::Paid, after(t1, 20));
    assert_eq!(paid.state, PlanState::Active);
    assert_eq!(paid.plan, Some(team));
    assert_eq!(paid.read_only_cause, None);
    assert_eq!(paid.read_only_since, None);
    assert_eq!(paid.past_due_since, None);
    assert!(paid.writable());
    assert_eq!(paid.seats_paid(), 7);
    assert_eq!(paid.daily_quota(), 13_000);

    let during_trial = PlanRecord::trial(t0).apply(BillingEvent::Active(team), t0);
    assert_eq!(during_trial.state, PlanState::Active);
    assert_eq!(during_trial.plan, Some(team));
    assert!(during_trial.writable());
}

#[test]
fn trial_ignores_past_due_and_cancel() {
    let trial = PlanRecord::trial(at("2026-11-02T15:04:05Z"));
    assert_eq!(
        trial.apply(BillingEvent::PastDue, trial.trial_started_at.unwrap()),
        trial
    );
    assert_eq!(
        trial.apply(BillingEvent::Canceled, trial.trial_started_at.unwrap()),
        trial
    );
}

#[test]
fn paid_without_a_plan_waits_for_the_plan() {
    let t0 = at("2026-11-02T15:04:05Z");
    let team = Plan::Team { extra_seats: 2 };
    let t1 = after(t0, 1);
    let t2 = t1 + Duration::from_millis(5);

    // A trial, an ended trial and a group awaiting payment hold no plan, so a
    // payment has nothing to restore.
    let trial = PlanRecord::trial(t0);
    let (ended, _) = trial.tick(after(t0, 8));
    for record in [trial.clone(), ended, PlanRecord::awaiting_payment(t0)] {
        assert_eq!(
            record.apply(BillingEvent::Paid, t2),
            record,
            "{:?}",
            record.state
        );
    }

    // One checkout sends `Active(plan)` and `Paid`. The payment can arrive
    // first even though it is stamped later, and the plan must still land.
    let paid_first = trial
        .apply(BillingEvent::Paid, t2)
        .apply(BillingEvent::Active(team), t1);
    let activated_first = trial
        .apply(BillingEvent::Active(team), t1)
        .apply(BillingEvent::Paid, t2);
    for record in [paid_first, activated_first] {
        assert_eq!(record.state, PlanState::Active);
        assert_eq!(record.plan, Some(team));
        assert_eq!(record.seats_paid(), 6);
        assert_eq!(record.daily_quota(), 12_000);
    }
}

#[test]
fn cancel_goes_read_only_for_90_days() {
    let t1 = at("2026-12-01T15:04:05Z");
    let team = Plan::Team { extra_seats: 1 };
    let canceled = active(team).apply(BillingEvent::Canceled, t1);
    assert_eq!(canceled.state, PlanState::ReadOnly);
    assert_eq!(canceled.read_only_cause, Some(ReadOnlyCause::Canceled));
    assert_eq!(canceled.read_only_since, Some(t1));
    assert_eq!(canceled.delete_at(), Some(after(t1, 90)));
    assert_eq!(canceled.plan, Some(team));
    assert!(!canceled.writable());
    assert_eq!(canceled.daily_quota(), 0);

    let from_past_due = active(team)
        .apply(BillingEvent::PastDue, t1)
        .apply(BillingEvent::Canceled, after(t1, 2));
    assert_eq!(from_past_due.state, PlanState::ReadOnly);
    assert_eq!(from_past_due.read_only_cause, Some(ReadOnlyCause::Canceled));
    assert_eq!(from_past_due.delete_at(), Some(after(after(t1, 2), 90)));
}

#[test]
fn older_events_are_ignored() {
    let t1 = at("2026-12-01T15:04:05Z");
    let t2 = after(t1, 3);
    let mut record = active(Plan::Team { extra_seats: 0 });
    record.last_event_at = Some(t2);
    assert!(t1 < t2);
    assert_eq!(record.apply(BillingEvent::PastDue, t1), record);
    assert_eq!(record.apply(BillingEvent::Canceled, t1), record);
    assert_eq!(
        record.apply(
            BillingEvent::Active(Plan::Business(BusinessStep::Seats50)),
            t1
        ),
        record
    );
    assert_eq!(record.apply(BillingEvent::Paid, t1), record);

    let applied = record.apply(BillingEvent::PastDue, t2);
    assert_eq!(applied.state, PlanState::PastDue);
    assert_eq!(applied.past_due_since, Some(t2));
}

#[test]
fn deleted_is_final() {
    let t1 = at("2026-12-01T15:04:05Z");
    let mut record = active(Plan::Business(BusinessStep::Seats25));
    record.state = PlanState::Deleted;
    record.read_only_cause = Some(ReadOnlyCause::TrialEnded);
    let later = after(t1, 1);
    assert_eq!(
        record.apply(BillingEvent::Active(Plan::Team { extra_seats: 0 }), later),
        record
    );
    assert_eq!(record.apply(BillingEvent::Paid, later), record);
    assert_eq!(record.apply(BillingEvent::PastDue, later), record);
    assert_eq!(record.apply(BillingEvent::Canceled, later), record);
    let (ticked, effects) = record.tick(later);
    assert_eq!(ticked, record);
    assert!(effects.is_empty());
}

#[test]
fn past_due_keeps_first_since() {
    let t1 = at("2026-12-01T15:04:05Z");
    let first = active(Plan::Team { extra_seats: 0 }).apply(BillingEvent::PastDue, t1);
    assert_eq!(first.past_due_since, Some(t1));
    assert_eq!(first.last_event_at, Some(t1));
    let second = first.apply(BillingEvent::PastDue, after(t1, 2));
    assert_eq!(second.past_due_since, Some(t1));
    assert_eq!(second.state, PlanState::PastDue);
    assert_eq!(second.last_event_at, Some(after(t1, 2)));
    assert!(second.writable());
}

#[test]
fn midnight_utc() {
    let almost = at("2026-11-02T23:59:59Z");
    let midnight = at("2026-11-03T00:00:00Z");
    let next = at("2026-11-04T00:00:00Z");
    assert_eq!(super::next_utc_midnight(almost), midnight);
    assert_eq!(super::next_utc_midnight(midnight), next);
    assert_eq!(
        super::next_utc_midnight(at("2026-11-03T00:00:00.000000001Z")),
        next
    );
    let epoch = at("1970-01-01T00:00:00Z");
    assert_eq!(super::next_utc_midnight(at("1969-12-31T23:00:00Z")), epoch);
    assert_eq!(super::next_utc_midnight(at("1969-12-31T00:00:00Z")), epoch);
    assert_eq!(super::next_utc_midnight(epoch), at("1970-01-02T00:00:00Z"));
}

#[test]
fn catalog_round_trips() {
    let catalog = Catalog {
        team_base: "base".to_owned(),
        team_seat: "seat".to_owned(),
        business_10: "b10".to_owned(),
        business_25: "b25".to_owned(),
        business_50: "b50".to_owned(),
    };
    let team2 = Plan::Team { extra_seats: 2 };
    let team0 = Plan::Team { extra_seats: 0 };
    assert_eq!(
        catalog.items_for(team2),
        vec![
            LineItem {
                price_id: "base".to_owned(),
                quantity: 1,
            },
            LineItem {
                price_id: "seat".to_owned(),
                quantity: 2,
            },
        ]
    );
    assert_eq!(
        catalog.items_for(team0),
        vec![LineItem {
            price_id: "base".to_owned(),
            quantity: 1,
        }]
    );
    assert_eq!(catalog.plan_of(&catalog.items_for(team2)), Some(team2));
    assert_eq!(catalog.plan_of(&catalog.items_for(team0)), Some(team0));
    assert_eq!(
        catalog.plan_of(&[LineItem {
            price_id: "b25".to_owned(),
            quantity: 1,
        }]),
        Some(Plan::Business(BusinessStep::Seats25))
    );
    for step in [
        BusinessStep::Seats10,
        BusinessStep::Seats25,
        BusinessStep::Seats50,
    ] {
        let plan = Plan::Business(step);
        assert_eq!(catalog.plan_of(&catalog.items_for(plan)), Some(plan));
    }
    assert_eq!(
        catalog.plan_of(&[LineItem {
            price_id: "nope".to_owned(),
            quantity: 1,
        }]),
        None
    );
    assert_eq!(catalog.plan_of(&[]), None);
    assert_eq!(
        catalog.plan_of(&[LineItem {
            price_id: "b25".to_owned(),
            quantity: 2,
        }]),
        None
    );
    assert_eq!(
        catalog.env_lines(),
        "\
AGMEN_PADDLE_PRICE_TEAM_BASE=base
AGMEN_PADDLE_PRICE_TEAM_SEAT=seat
AGMEN_PADDLE_PRICE_BUSINESS_10=b10
AGMEN_PADDLE_PRICE_BUSINESS_25=b25
AGMEN_PADDLE_PRICE_BUSINESS_50=b50
"
    );
}
