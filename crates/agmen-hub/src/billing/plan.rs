//! Pure hosted plan rules: trials, seats, quotas, retention, and the
//! read-only and deletion clocks.
//!
//! This module does no I/O. Every instant is passed in. A day is 86400
//! seconds. Nothing here calls Paddle, and nothing prunes stored messages
//! (hosted Task 9).

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use agmen_proto::Timestamp;

const SECS_PER_DAY: u64 = 86_400;

/// Messages a trial group may post per UTC day.
const TRIAL_DAILY_QUOTA: u32 = 5_000;
/// Messages a Team group may post per UTC day before extra seats.
const TEAM_DAILY_QUOTA: u32 = 10_000;
/// Extra daily messages for each Team seat past the included four.
const EXTRA_SEAT_DAILY_QUOTA: u32 = 1_000;
/// Daily messages for the Business 10-seat step. Larger steps scale by seats / 10.
const BUSINESS_DAILY_QUOTA_FOR_10: u32 = 50_000;
/// Days of history a trial or Team group keeps.
const TEAM_RETENTION_DAYS: u32 = 90;
/// Days of history a Business group keeps.
const BUSINESS_RETENTION_DAYS: u32 = 365;

/// How long a hosted trial lasts.
pub const TRIAL_DAYS: u32 = 7;

/// Days after the trial start when the day-5 and day-7 reminders begin.
///
/// Day 5 begins 4 days after the start, and day 7 begins 6 days after it.
pub const TRIAL_REMINDER_OFFSETS_DAYS: [u32; 2] = [4, 6];

/// How long a trial stays read-only after it ends, before deletion.
pub const TRIAL_READ_ONLY_DAYS: u32 = 14;

/// How long a past-due group keeps full access while the card is retried.
pub const PAST_DUE_GRACE_DAYS: u32 = 14;

/// How long a paid group stays read-only after a failed payment or a cancel,
/// before deletion.
pub const PAID_READ_ONLY_DAYS: u32 = 90;

/// Seats included in a trial and in Team before extra seats.
pub const TEAM_INCLUDED_SEATS: u32 = 4;

/// The most groups one account may own.
pub const MAX_OWNED_GROUPS: u32 = 3;

/// Where a hosted group is on the trial and payment timeline.
///
/// ```
/// use agmen_hub::PlanState;
/// # fn main() -> Result<(), agmen_hub::UnknownPlanState> {
/// let state: PlanState = "past_due".parse()?;
/// assert_eq!(state.as_str(), "past_due");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlanState {
    /// The 7-day trial, with no card on file.
    Trial,
    /// A paid plan in good standing.
    Active,
    /// The provider is retrying a failed card. Access continues.
    PastDue,
    /// Writes are refused. The group is deleted when its clock runs out.
    ReadOnly,
    /// The group is gone. Later events do not bring it back.
    Deleted,
}

impl PlanState {
    /// The stored name: `trial`, `active`, `past_due`, `read_only`, or `deleted`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trial => "trial",
            Self::Active => "active",
            Self::PastDue => "past_due",
            Self::ReadOnly => "read_only",
            Self::Deleted => "deleted",
        }
    }
}

impl fmt::Display for PlanState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PlanState {
    type Err = UnknownPlanState;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "trial" => Ok(Self::Trial),
            "active" => Ok(Self::Active),
            "past_due" => Ok(Self::PastDue),
            "read_only" => Ok(Self::ReadOnly),
            "deleted" => Ok(Self::Deleted),
            other => Err(UnknownPlanState(other.to_owned())),
        }
    }
}

/// Text that names no [`PlanState`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown plan state {0:?}")]
pub struct UnknownPlanState(String);

/// A Business seat step. Custom pricing above 50 seats is not a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BusinessStep {
    /// 10 seats.
    Seats10,
    /// 25 seats.
    Seats25,
    /// 50 seats.
    Seats50,
}

impl BusinessStep {
    /// Seats this step sells.
    #[must_use]
    pub const fn seats(self) -> u32 {
        match self {
            Self::Seats10 => 10,
            Self::Seats25 => 25,
            Self::Seats50 => 50,
        }
    }

    /// The smallest step that holds `seats`, or `None` when `seats` is above 50.
    #[must_use]
    pub const fn for_seats(seats: u32) -> Option<Self> {
        if seats <= 10 {
            Some(Self::Seats10)
        } else if seats <= 25 {
            Some(Self::Seats25)
        } else if seats <= 50 {
            Some(Self::Seats50)
        } else {
            None
        }
    }
}

/// A paid plan. The trial is not a plan: it lives on [`PlanRecord`] until checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Plan {
    /// Team, with `extra_seats` past the included four.
    Team {
        /// Seats past [`TEAM_INCLUDED_SEATS`], each billed on the seat price.
        extra_seats: u32,
    },
    /// Business at one seat step.
    Business(BusinessStep),
}

/// Why a group is read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReadOnlyCause {
    /// The trial ended without a payment.
    TrialEnded,
    /// A later group got no trial and is waiting for checkout.
    AwaitingPayment,
    /// The past-due grace ran out.
    PaymentFailed,
    /// The subscription was canceled.
    Canceled,
}

/// How long message history is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Retention {
    /// Drop messages older than this many days.
    Days(u32),
    /// Keep every message. Self-host uses this when no limit is set.
    Forever,
}

/// A billing notification, in the order the provider emitted it.
///
/// [`PlanRecord::apply`] is the only reader. It ignores anything older than
/// the record's last event, anything that arrives after deletion, `PastDue` or
/// `Canceled` while the group is still on a trial, and `Paid` while the record
/// holds no plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BillingEvent {
    /// Checkout or a plan change to `plan`. Restores a group that is not deleted.
    Active(Plan),
    /// The provider started retrying the card.
    PastDue,
    /// The subscription was canceled.
    Canceled,
    /// A payment succeeded. Restores the group's current plan. A record with
    /// no plan ignores it, because only [`BillingEvent::Active`] names one.
    Paid,
}

/// What a [`PlanRecord::tick`] changed, for the caller to post or delete.
///
/// `apply` does not produce effects. Its caller posts any alert itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlanEffect {
    /// Tell the owner the trial ends on `ends_at`. `day` is 5 or 7.
    TrialReminder {
        /// Trial day the reminder belongs to, 5 or 7.
        day: u8,
        /// When the trial ends.
        ends_at: Timestamp,
    },
    /// The group just became read-only for this cause.
    BecameReadOnly(ReadOnlyCause),
    /// The deletion clock ran out. The caller deletes the group.
    Delete,
}

/// One group's plan, as the hub stores it. Passive data: the clocks are methods.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlanRecord {
    /// Trial, active, past due, read-only, or deleted.
    pub state: PlanState,
    /// The paid plan. `None` during a trial and while awaiting a first payment.
    pub plan: Option<Plan>,
    /// When the trial started, if this group had one.
    pub trial_started_at: Option<Timestamp>,
    /// When the group first went past due. Later past-due events keep this.
    pub past_due_since: Option<Timestamp>,
    /// Why the group is read-only, once it is.
    pub read_only_cause: Option<ReadOnlyCause>,
    /// When the read-only period started.
    pub read_only_since: Option<Timestamp>,
    /// How many trial reminders have already been emitted. Day 5 is 1, day 7 is 2.
    pub reminders_sent: u8,
    /// When the newest applied billing event occurred.
    pub last_event_at: Option<Timestamp>,
}

impl PlanRecord {
    /// A 7-day trial starting at `now`, with Team's 4 seats and no card.
    ///
    /// # Examples
    ///
    /// ```
    /// use agmen_proto::Timestamp;
    /// use agmen_hub::PlanRecord;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let start: Timestamp = "2026-11-02T00:00:00Z".parse()?;
    /// let trial = PlanRecord::trial(start);
    /// assert_eq!(trial.seats_paid(), 4);
    /// assert_eq!(trial.daily_quota(), 5_000);
    /// assert!(trial.writable());
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn trial(now: Timestamp) -> Self {
        Self {
            state: PlanState::Trial,
            plan: None,
            trial_started_at: Some(now),
            past_due_since: None,
            read_only_cause: None,
            read_only_since: None,
            reminders_sent: 0,
            last_event_at: None,
        }
    }

    /// A group with no trial left, read-only until it pays or 14 days pass.
    #[must_use]
    pub fn awaiting_payment(now: Timestamp) -> Self {
        Self {
            state: PlanState::ReadOnly,
            plan: None,
            trial_started_at: None,
            past_due_since: None,
            read_only_cause: Some(ReadOnlyCause::AwaitingPayment),
            read_only_since: Some(now),
            reminders_sent: 0,
            last_event_at: None,
        }
    }

    /// Seats the group has paid for: 4 on a trial, `4 + extra_seats` on Team,
    /// the step on Business, and 0 when there is no plan and no trial.
    #[must_use]
    pub fn seats_paid(&self) -> u32 {
        if self.state == PlanState::Trial {
            return TEAM_INCLUDED_SEATS;
        }
        match self.plan {
            Some(Plan::Team { extra_seats }) => TEAM_INCLUDED_SEATS.saturating_add(extra_seats),
            Some(Plan::Business(step)) => step.seats(),
            None => 0,
        }
    }

    /// Messages the group may post until the next UTC midnight.
    ///
    /// A trial gets 5,000. Team gets 10,000 plus 1,000 for each extra seat.
    /// Business gets 50,000 for 10 seats, scaled by the step. A group that
    /// cannot write gets 0.
    #[must_use]
    pub fn daily_quota(&self) -> u32 {
        if !self.writable() {
            return 0;
        }
        if self.state == PlanState::Trial {
            return TRIAL_DAILY_QUOTA;
        }
        match self.plan {
            Some(Plan::Team { extra_seats }) => {
                TEAM_DAILY_QUOTA.saturating_add(EXTRA_SEAT_DAILY_QUOTA.saturating_mul(extra_seats))
            }
            Some(Plan::Business(step)) => {
                BUSINESS_DAILY_QUOTA_FOR_10.saturating_mul(step.seats()) / 10
            }
            None => 0,
        }
    }

    /// How long this group's messages are kept. Trial and Team keep 90 days.
    /// Business keeps 365.
    #[must_use]
    pub fn retention(&self) -> Retention {
        match self.plan {
            Some(Plan::Business(_)) => Retention::Days(BUSINESS_RETENTION_DAYS),
            _ => Retention::Days(TEAM_RETENTION_DAYS),
        }
    }

    /// Whether the group may post, claim, and invite.
    ///
    /// Trial, active, and past due may. Read-only and deleted may not.
    #[must_use]
    pub fn writable(&self) -> bool {
        matches!(
            self.state,
            PlanState::Trial | PlanState::Active | PlanState::PastDue
        )
    }

    /// When the trial ends, if one was started.
    #[must_use]
    pub fn trial_ends_at(&self) -> Option<Timestamp> {
        self.trial_started_at
            .map(|started| add_days(started, TRIAL_DAYS))
    }

    /// When the read-only period starts, or started.
    #[must_use]
    pub fn read_only_at(&self) -> Option<Timestamp> {
        match self.state {
            PlanState::Trial => self.trial_ends_at(),
            PlanState::PastDue => self
                .past_due_since
                .map(|since| add_days(since, PAST_DUE_GRACE_DAYS)),
            PlanState::ReadOnly | PlanState::Deleted => self.read_only_since,
            PlanState::Active => None,
        }
    }

    /// When the group is deleted, if a deletion is scheduled.
    ///
    /// Read-only lasts 14 days after a trial ends or while awaiting payment,
    /// and 90 days after a failed payment or a cancel. An active plan has no
    /// deletion date. A deleted group has already passed it.
    #[must_use]
    pub fn delete_at(&self) -> Option<Timestamp> {
        match self.state {
            PlanState::Trial => self
                .trial_started_at
                .map(|started| add_days(started, TRIAL_DAYS.saturating_add(TRIAL_READ_ONLY_DAYS))),
            PlanState::PastDue => self.past_due_since.map(|since| {
                add_days(
                    since,
                    PAST_DUE_GRACE_DAYS.saturating_add(PAID_READ_ONLY_DAYS),
                )
            }),
            PlanState::ReadOnly => {
                let since = self.read_only_since?;
                let days = read_only_days(self.read_only_cause?);
                Some(add_days(since, days))
            }
            PlanState::Active | PlanState::Deleted => None,
        }
    }

    /// Advance the clocks to `now`.
    ///
    /// One call emits at most the reminders that are due, one transition into
    /// read-only, and one deletion, in that order. A second call at the same
    /// instant emits nothing new. A deleted record stays deleted.
    #[must_use]
    pub fn tick(&self, now: Timestamp) -> (Self, Vec<PlanEffect>) {
        if self.state == PlanState::Deleted {
            return (self.clone(), Vec::new());
        }
        let mut next = self.clone();
        let mut effects = Vec::new();
        if next.state == PlanState::Trial {
            tick_trial(&mut next, now, &mut effects);
        }
        if next.state == PlanState::PastDue {
            tick_past_due(&mut next, now, &mut effects);
        }
        if next.state == PlanState::ReadOnly {
            tick_read_only(&mut next, now, &mut effects);
        }
        (next, effects)
    }

    /// Apply one provider event that occurred at `occurred_at`.
    ///
    /// Callers should run `tick(now)` first and apply the event to the returned
    /// record.
    ///
    /// An event older than [`Self::last_event_at`] is ignored, and so is every
    /// event once the group is deleted. `PastDue` and `Canceled` do not change
    /// a trial. `Active` restores any other state to active, and so does
    /// `Paid` for a record that holds a plan. A record with no plan ignores
    /// `Paid`, without recording it, so the checkout's `Active(plan)` still
    /// lands when the payment event is stamped later but arrives first. A
    /// repeated `PastDue` keeps the first `past_due_since`. `Canceled` moves
    /// an active or past-due group to read-only.
    #[must_use]
    pub fn apply(&self, event: BillingEvent, occurred_at: Timestamp) -> Self {
        if self.state == PlanState::Deleted {
            return self.clone();
        }
        if self.last_event_at.is_some_and(|at| occurred_at < at) {
            return self.clone();
        }
        if self.state == PlanState::Trial
            && matches!(event, BillingEvent::PastDue | BillingEvent::Canceled)
        {
            return self.clone();
        }
        // Active with no plan is writable, yet has 0 seats, a 0 quota and no
        // clock to read-only or deletion. Only Active(plan) names a plan.
        if self.plan.is_none() && event == BillingEvent::Paid {
            return self.clone();
        }
        let mut next = self.clone();
        next.last_event_at = Some(occurred_at);
        match event {
            BillingEvent::Active(plan) => {
                next.state = PlanState::Active;
                next.plan = Some(plan);
                clear_blocks(&mut next);
            }
            BillingEvent::Paid => {
                next.state = PlanState::Active;
                clear_blocks(&mut next);
            }
            BillingEvent::PastDue => {
                if next.state == PlanState::Active {
                    next.state = PlanState::PastDue;
                    next.past_due_since = Some(occurred_at);
                }
            }
            BillingEvent::Canceled => {
                if matches!(next.state, PlanState::Active | PlanState::PastDue) {
                    next.state = PlanState::ReadOnly;
                    next.read_only_cause = Some(ReadOnlyCause::Canceled);
                    next.read_only_since = Some(occurred_at);
                }
            }
        }
        next
    }
}

/// The next UTC midnight strictly after `now`.
///
/// An instant that is already midnight resolves to the following day. Quota
/// counters reset at that instant.
///
/// # Examples
///
/// ```
/// use agmen_proto::Timestamp;
/// use agmen_hub::next_utc_midnight;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let almost: Timestamp = "2026-11-02T23:59:59Z".parse()?;
/// let midnight: Timestamp = "2026-11-03T00:00:00Z".parse()?;
/// assert_eq!(next_utc_midnight(almost), midnight);
/// assert_eq!(next_utc_midnight(midnight).to_string(), "2026-11-04T00:00:00Z");
/// # Ok(())
/// # }
/// ```
#[must_use]
pub fn next_utc_midnight(now: Timestamp) -> Timestamp {
    let day = i64::try_from(SECS_PER_DAY).unwrap_or(i64::MAX);
    let next_day = floor_second(now).div_euclid(day).saturating_add(1);
    let next = next_day.saturating_mul(day);
    match Timestamp::from_second(next) {
        Ok(midnight) => midnight,
        Err(_) => {
            if next < 0 {
                Timestamp::MIN
            } else {
                Timestamp::MAX
            }
        }
    }
}

fn floor_second(now: Timestamp) -> i64 {
    let second = now.as_second();
    if now.subsec_nanosecond() < 0 {
        second.saturating_sub(1)
    } else {
        second
    }
}

fn add_days(instant: Timestamp, days: u32) -> Timestamp {
    let seconds = u64::from(days).saturating_mul(SECS_PER_DAY);
    instant
        .saturating_add(Duration::from_secs(seconds))
        .unwrap_or(Timestamp::MAX)
}

fn reminder_day(offset_days: u32) -> u8 {
    u8::try_from(offset_days.saturating_add(1)).unwrap_or(u8::MAX)
}

fn read_only_days(cause: ReadOnlyCause) -> u32 {
    match cause {
        ReadOnlyCause::TrialEnded | ReadOnlyCause::AwaitingPayment => TRIAL_READ_ONLY_DAYS,
        ReadOnlyCause::PaymentFailed | ReadOnlyCause::Canceled => PAID_READ_ONLY_DAYS,
    }
}

fn clear_blocks(record: &mut PlanRecord) {
    record.past_due_since = None;
    record.read_only_cause = None;
    record.read_only_since = None;
}

fn tick_trial(record: &mut PlanRecord, now: Timestamp, effects: &mut Vec<PlanEffect>) {
    let Some(started) = record.trial_started_at else {
        return;
    };
    let trial_end = add_days(started, TRIAL_DAYS);
    if now < trial_end {
        let already_sent = usize::from(record.reminders_sent);
        for (index, offset) in TRIAL_REMINDER_OFFSETS_DAYS.iter().copied().enumerate() {
            if index < already_sent {
                continue;
            }
            if now < add_days(started, offset) {
                break;
            }
            effects.push(PlanEffect::TrialReminder {
                day: reminder_day(offset),
                ends_at: trial_end,
            });
            record.reminders_sent = record.reminders_sent.saturating_add(1);
        }
        return;
    }
    record.state = PlanState::ReadOnly;
    record.read_only_cause = Some(ReadOnlyCause::TrialEnded);
    record.read_only_since = Some(trial_end);
    effects.push(PlanEffect::BecameReadOnly(ReadOnlyCause::TrialEnded));
}

fn tick_past_due(record: &mut PlanRecord, now: Timestamp, effects: &mut Vec<PlanEffect>) {
    let Some(since) = record.past_due_since else {
        return;
    };
    let read_only_at = add_days(since, PAST_DUE_GRACE_DAYS);
    if now < read_only_at {
        return;
    }
    record.state = PlanState::ReadOnly;
    record.read_only_cause = Some(ReadOnlyCause::PaymentFailed);
    record.read_only_since = Some(read_only_at);
    effects.push(PlanEffect::BecameReadOnly(ReadOnlyCause::PaymentFailed));
}

fn tick_read_only(record: &mut PlanRecord, now: Timestamp, effects: &mut Vec<PlanEffect>) {
    let Some(delete_at) = record.delete_at() else {
        return;
    };
    if now < delete_at {
        return;
    }
    record.state = PlanState::Deleted;
    effects.push(PlanEffect::Delete);
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
