//! Hosted plan rules and the Paddle price catalog.
//!
//! Plan rules cover trials, seats, quotas, retention, and the read-only and
//! deletion clocks. They do no I/O: every instant is passed in. Nothing here
//! calls Paddle, except [`PaddleClient`], which the hub does not use yet.
//! Nothing prunes stored messages yet (hosted Task 9).

mod catalog;
mod paddle;
mod plan;

pub use catalog::{Catalog, LineItem};
pub use paddle::{PaddleClient, PaddleError, sync_catalog};
pub use plan::{
    BillingEvent, BusinessStep, MAX_OWNED_GROUPS, PAID_READ_ONLY_DAYS, PAST_DUE_GRACE_DAYS, Plan,
    PlanEffect, PlanRecord, PlanState, ReadOnlyCause, Retention, TEAM_INCLUDED_SEATS, TRIAL_DAYS,
    TRIAL_READ_ONLY_DAYS, TRIAL_REMINDER_OFFSETS_DAYS, UnknownPlanState, next_utc_midnight,
};
