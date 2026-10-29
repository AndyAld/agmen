//! Hosted plan rules and the Paddle price catalog.
//!
//! Plan rules cover trials, seats, quotas, retention, and the read-only and
//! deletion clocks. They do no I/O: every instant is passed in. [`PaddleClient`]
//! is the only code here that calls Paddle. The billing routes in `groups.rs`
//! use it, and `agmen-hub paddle-catalog` uses [`sync_catalog`]. The webhook
//! handler does not call Paddle: it reads what Paddle sends. The hourly
//! retention pass in `retention.rs` prunes stored messages by these rules.

mod catalog;
pub(crate) mod clock;
mod limiter;
mod paddle;
mod plan;
pub(crate) mod webhook;

pub use catalog::{Catalog, LineItem};
pub use clock::PlanClockError;
pub(crate) use limiter::BillingLimiter;
pub use paddle::{PaddleClient, PaddleError, sync_catalog};
pub use plan::{
    BillingEvent, BusinessStep, MAX_OWNED_GROUPS, PAID_READ_ONLY_DAYS, PAST_DUE_GRACE_DAYS, Plan,
    PlanEffect, PlanRecord, PlanState, ReadOnlyCause, Retention, TEAM_INCLUDED_SEATS, TRIAL_DAYS,
    TRIAL_READ_ONLY_DAYS, TRIAL_REMINDER_OFFSETS_DAYS, UnknownPlanState, next_utc_midnight,
};
