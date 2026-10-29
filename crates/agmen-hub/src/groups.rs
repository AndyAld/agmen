//! Hosted groups: creating a tenant and its first project, the one trial
//! per GitHub account, the three-group ownership limit, and paid seats.
//!
//! `checkout_url` stays null until hosted Task 7. This module does not call
//! Paddle.

use std::collections::HashMap;
use std::time::Duration;

use agmen_proto::{
    Body, IdentityId, Needs, Payload, RepoUrl, Stream, TenantId, Timestamp, Topics, Urgency,
};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, Postgres, Transaction};

use crate::auth::Caller;
use crate::billing::{
    BusinessStep, MAX_OWNED_GROUPS, Plan, PlanRecord, PlanState, ReadOnlyCause,
    TRIAL_REMINDER_OFFSETS_DAYS,
};
use crate::db::{self, TenantRole, new_id};
use crate::http::{ApiError, Hub, error_chain};
use crate::identities::HumanId;
use crate::ingest::{HubPost, PostError};
use crate::projects::create_project;
use crate::tenants::{join_tenant, require_group_admin, tenant_role};
use crate::usage::refuse_read_only;

const GROUP_LIMIT: &str = "you already own 3 groups, the most one account can own";

const NO_FREE_SEAT: &str = "the group has no free seat; its owner or a group admin can add one with: agmen group seats add 1";

const SECS_PER_DAY: u64 = 86_400;

/// Upserts the plan columns only. Paddle ids and `no_seat_alerted_at` stay
/// as they are, including on insert where they start null.
const UPSERT_PLAN: &str = "\
INSERT INTO tenant_plans (
    tenant_id, state, plan, extra_seats, business_step,
    trial_started_at, past_due_since, read_only_cause, read_only_since,
    reminders_sent, last_event_at
) VALUES (
    $1, $2, $3, $4, $5,
    $6::timestamptz, $7::timestamptz, $8, $9::timestamptz,
    $10, $11::timestamptz
)
ON CONFLICT (tenant_id) DO UPDATE SET
    state = EXCLUDED.state,
    plan = EXCLUDED.plan,
    extra_seats = EXCLUDED.extra_seats,
    business_step = EXCLUDED.business_step,
    trial_started_at = EXCLUDED.trial_started_at,
    past_due_since = EXCLUDED.past_due_since,
    read_only_cause = EXCLUDED.read_only_cause,
    read_only_since = EXCLUDED.read_only_since,
    reminders_sent = EXCLUDED.reminders_sent,
    last_event_at = EXCLUDED.last_event_at";

const LOAD_PLANS: &str = "\
SELECT tenant_id,
       state,
       plan,
       extra_seats,
       business_step,
       (extract(epoch FROM trial_started_at) * 1000000)::bigint AS trial_started_at,
       (extract(epoch FROM past_due_since) * 1000000)::bigint AS past_due_since,
       read_only_cause,
       (extract(epoch FROM read_only_since) * 1000000)::bigint AS read_only_since,
       reminders_sent,
       (extract(epoch FROM last_event_at) * 1000000)::bigint AS last_event_at
FROM tenant_plans
WHERE tenant_id = ANY($1)";

/// Why `agmen-hub set-plan` could not store the plan.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SetPlanError {
    /// `tenant` is not a tenant id.
    #[error("not a tenant id")]
    BadTenant,
    /// `plan` is not `team`, `business-10`, `business-25`, or `business-50`.
    #[error("unknown plan")]
    BadPlan,
    /// No tenant has that id.
    #[error("no such tenant")]
    NoTenant,
    /// The group has a Paddle customer or subscription, so Paddle owns its plan.
    #[error("this group is billed through Paddle; change its plan there")]
    PaddleBilled,
    /// Connecting, migrating, or a query failed.
    #[error("database error")]
    Database(#[from] sqlx::Error),
}

/// The plan fields `GET /v1/me` and `POST /v1/groups` show.
///
/// `trial_ends_at` is set only while `state` is `trial`.
#[must_use]
pub(crate) fn shown_plan(record: &PlanRecord) -> (String, Option<Timestamp>) {
    let trial_ends_at = if record.state == PlanState::Trial {
        record.trial_ends_at()
    } else {
        None
    };
    (record.state.as_str().to_owned(), trial_ends_at)
}

/// The owner's notice for this group, if the plan is in a window that has one.
///
/// Trial notices start on day 5 (4 days after the start). Past due and
/// read-only always have one, when their date is known. Active and deleted
/// have none.
#[must_use]
pub(crate) fn owner_notice(name: &str, record: &PlanRecord, now: Timestamp) -> Option<String> {
    match record.state {
        PlanState::Trial => trial_notice(name, record, now),
        PlanState::PastDue => {
            let at = record.read_only_at()?;
            Some(format!(
                "{name} is past due and becomes read-only on {at}. Run agmen group billing to keep it."
            ))
        }
        PlanState::ReadOnly => {
            let until = record.delete_at()?;
            Some(format!(
                "{name} is read-only and will be deleted on {until}. Run agmen group billing to restore it."
            ))
        }
        PlanState::Active | PlanState::Deleted => None,
    }
}

fn trial_notice(name: &str, record: &PlanRecord, now: Timestamp) -> Option<String> {
    let started = record.trial_started_at?;
    let offset = u64::from(TRIAL_REMINDER_OFFSETS_DAYS[0]).saturating_mul(SECS_PER_DAY);
    // A start so late that day 5 is past the last instant has no notice to show.
    let due = started.saturating_add(Duration::from_secs(offset)).ok()?;
    if now < due {
        return None;
    }
    let ends = record.trial_ends_at()?;
    Some(format!(
        "Your agmen hosted trial of {name} ends on {ends}. Run agmen group billing to keep going."
    ))
}

/// The tenant's plan, or `None` when it has no plan row.
///
/// # Errors
///
/// When a query fails or a stored value is outside the plan rules.
pub(crate) async fn load_plan(
    db: impl PgExecutor<'_>,
    tenant: &TenantId,
) -> Result<Option<PlanRecord>, sqlx::Error> {
    let mut found = load_plans(db, &[tenant.as_str().to_owned()]).await?;
    Ok(found.remove(tenant.as_str()))
}

/// Plans for `tenants`, keyed by tenant id. Missing tenants are absent.
///
/// # Errors
///
/// When a query fails or a stored value is outside the plan rules.
pub(crate) async fn load_plans(
    db: impl PgExecutor<'_>,
    tenants: &[String],
) -> Result<HashMap<String, PlanRecord>, sqlx::Error> {
    if tenants.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<PlanRow> = sqlx::query_as(LOAD_PLANS)
        .bind(tenants)
        .fetch_all(db)
        .await?;
    let mut plans = HashMap::with_capacity(rows.len());
    for row in rows {
        let id = row.tenant_id.clone();
        plans.insert(id, record_from_row(row)?);
    }
    Ok(plans)
}

/// Writes `record` for `tenant`. Does not change Paddle ids or
/// `no_seat_alerted_at`.
///
/// # Errors
///
/// When a query fails or a seat count does not fit the column.
pub(crate) async fn store_plan(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &TenantId,
    record: &PlanRecord,
) -> Result<(), sqlx::Error> {
    let columns = plan_columns(record.plan)?;
    sqlx::query(UPSERT_PLAN)
        .bind(tenant.as_str())
        .bind(record.state.as_str())
        .bind(columns.plan)
        .bind(columns.extra_seats)
        .bind(columns.business_step)
        .bind(stamp(record.trial_started_at))
        .bind(stamp(record.past_due_since))
        .bind(cause_str(record.read_only_cause))
        .bind(stamp(record.read_only_since))
        .bind(i16::from(record.reminders_sent))
        .bind(stamp(record.last_event_at))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// How many humans belong to `tenant`. Agents are identities and are not counted.
///
/// # Errors
///
/// When a query fails or the count does not fit in `u32`.
pub(crate) async fn seats_used(
    db: impl PgExecutor<'_>,
    tenant: &TenantId,
) -> Result<u32, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tenant_members WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .fetch_one(db)
        .await?;
    u32::try_from(count).map_err(decode)
}

/// Locks the tenant's plan row, then lets `human` take a seat.
///
/// A group whose ticked plan is not writable answers 403 `group_read_only`
/// first. A human who already belongs to the group takes no seat. A group with
/// no plan row is not seat-limited (a hosted bootstrap tenant has none). When
/// every paid seat is taken, the invite stays unused: this commits the
/// `no_seat_alerted_at` update and returns 403 `no_free_seat`. The alert
/// posts after that commit, at most once an hour.
///
/// # Errors
///
/// 403 `group_read_only` when the group cannot write, 403 `no_free_seat` when
/// the group is full, or a query error.
pub(crate) async fn ensure_free_seat<'a>(
    hub: &Hub,
    mut tx: Transaction<'a, Postgres>,
    tenant: &str,
    human: &HumanId,
    topics: &Topics,
) -> Result<Transaction<'a, Postgres>, ApiError> {
    let locked: Option<String> =
        sqlx::query_scalar("SELECT tenant_id FROM tenant_plans WHERE tenant_id = $1 FOR UPDATE")
            .bind(tenant)
            .fetch_optional(&mut *tx)
            .await?;
    if locked.is_none() {
        return Ok(tx);
    }
    let tenant_id: TenantId = tenant.parse().map_err(ApiError::stored)?;
    let Some(record) = load_plan(&mut *tx, &tenant_id).await? else {
        return Ok(tx);
    };
    // Before the seat check: a read-only record has no paid seats and would
    // otherwise answer a misleading `no_free_seat`.
    let (record, _) = record.tick(Timestamp::now());
    if !record.writable() {
        return Err(refuse_read_only());
    }
    let member: Option<String> = sqlx::query_scalar(
        "SELECT human_id FROM tenant_members WHERE tenant_id = $1 AND human_id = $2",
    )
    .bind(tenant)
    .bind(human.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    if member.is_some() {
        return Ok(tx);
    }
    let used = seats_used(&mut *tx, &tenant_id).await?;
    if used < record.seats_paid() {
        return Ok(tx);
    }
    refuse_full(hub, tx, tenant, human, topics).await?;
    Err(no_free_seat())
}

fn no_free_seat() -> ApiError {
    ApiError::forbidden("no_free_seat", NO_FREE_SEAT)
}

/// Commits the hourly alert stamp. The caller still answers 403.
async fn refuse_full(
    hub: &Hub,
    mut tx: Transaction<'_, Postgres>,
    tenant: &str,
    human: &HumanId,
    topics: &Topics,
) -> Result<(), ApiError> {
    let claimed = claim_seat_alert(&mut tx, tenant).await?;
    let handle: Option<String> = sqlx::query_scalar("SELECT handle FROM humans WHERE id = $1")
        .bind(human.as_str())
        .fetch_optional(&mut *tx)
        .await?;
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT human_id FROM tenant_members WHERE tenant_id = $1 AND role = 'owner'",
    )
    .bind(tenant)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    if claimed {
        warn_on_alert(alert_owner(hub, topics, owner.as_deref(), handle.as_deref()).await);
    }
    Ok(())
}

async fn claim_seat_alert(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
) -> Result<bool, sqlx::Error> {
    let claimed: Option<String> = sqlx::query_scalar(
        "UPDATE tenant_plans SET no_seat_alerted_at = now()
         WHERE tenant_id = $1
           AND (no_seat_alerted_at IS NULL OR no_seat_alerted_at <= now() - interval '1 hour')
         RETURNING tenant_id",
    )
    .bind(tenant)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(claimed.is_some())
}

async fn alert_owner(
    hub: &Hub,
    topics: &Topics,
    owner: Option<&str>,
    handle: Option<&str>,
) -> Result<(), PostError> {
    let (Some(owner), Some(handle)) = (owner, handle) else {
        return Err(PostError::Store(Box::new(std::io::Error::other(
            "no-seat alert is missing the owner or the person's handle",
        ))));
    };
    let owner_id: IdentityId = owner.parse().map_err(store_post)?;
    let body =
        Body::try_from(format!("{handle} could not join: {NO_FREE_SEAT}")).map_err(store_post)?;
    hub.post_as_hub(
        topics,
        HubPost {
            payload: Payload::Alert {
                need: format!("a seat for {handle}"),
                urgency: Urgency::Normal,
                needs: Needs::Anyone,
                actions: Vec::new(),
            },
            body,
            reply_to: None,
            items: Vec::new(),
            lane: None,
            mentioned: vec![owner_id.clone()],
            streams: vec![Stream::Inbox(owner_id)],
        },
    )
    .await?;
    Ok(())
}

fn store_post(err: impl std::error::Error + Send + Sync + 'static) -> PostError {
    PostError::Store(Box::new(err))
}

fn warn_on_alert(result: Result<(), PostError>) {
    if let Err(err) = result {
        tracing::warn!(error = %error_chain(&err), "no-seat alert was not posted");
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Seats {
    used: u32,
    paid: u32,
    plan: Option<String>,
    state: &'static str,
    quota: u32,
    used_today: u32,
}

/// `GET /v1/tenants/{tenant}/seats`. Hosted only. Owner or group admin.
pub(crate) async fn seats(
    State(hub): State<Hub>,
    Path(tenant): Path<String>,
    caller: Caller,
) -> Result<Json<Seats>, ApiError> {
    if hub.mode() != crate::config::HubMode::Hosted {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_hosted",
            "this hub is not agmen hosted",
        ));
    }
    let human = caller.human()?;
    require_group_admin(tenant_role(hub.pool(), &tenant, &human).await?)?;
    let tenant_id: TenantId = tenant.parse().map_err(ApiError::stored)?;
    let Some(record) = load_plan(hub.pool(), &tenant_id).await? else {
        return Err(ApiError::not_found("this group has no seat plan"));
    };
    let used = seats_used(hub.pool(), &tenant_id).await?;
    let used_today: i32 = sqlx::query_scalar(
        "SELECT coalesce((SELECT messages FROM tenant_usage
         WHERE tenant_id = $1 AND day = (now() AT TIME ZONE 'UTC')::date), 0)",
    )
    .bind(tenant_id.as_str())
    .fetch_one(hub.pool())
    .await?;
    let used_today = u32::try_from(used_today).map_err(ApiError::stored)?;
    Ok(Json(Seats {
        used,
        paid: record.seats_paid(),
        plan: plan_name(record.plan),
        state: record.state.as_str(),
        quota: record.daily_quota(),
        used_today,
    }))
}

fn plan_name(plan: Option<Plan>) -> Option<String> {
    match plan {
        None => None,
        Some(Plan::Team { .. }) => Some("team".to_owned()),
        Some(Plan::Business(step)) => Some(format!("business-{}", step.seats())),
    }
}

/// Refuses when `human` already owns [`MAX_OWNED_GROUPS`] groups.
///
/// Takes a transaction advisory lock on `hashtext` of the human id, then
/// counts owner rows. A `SELECT ... FOR UPDATE` of those rows does not block
/// a phantom insert of a new owner row under READ COMMITTED, so two creates
/// at two owned groups could both pass. The lock serializes the check and
/// the following insert. A `hashtext` collision only serializes an extra
/// human; it cannot let a fourth group through.
///
/// Create takes this lock and then inserts a new tenant. Transfer locks the
/// transferred tenant and then takes this lock on the receiver. Those orders
/// do not cycle. A sender who owns three and races their own transfer against
/// a create can be refused; they retry.
///
/// # Errors
///
/// 403 `group_limit` at the cap, or a query error.
pub(crate) async fn check_owner_limit(
    tx: &mut Transaction<'_, Postgres>,
    human: &HumanId,
) -> Result<(), ApiError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
        .bind(human.as_str())
        .execute(&mut **tx)
        .await?;
    let owned: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tenant_members WHERE human_id = $1 AND role = 'owner'",
    )
    .bind(human.as_str())
    .fetch_one(&mut **tx)
    .await?;
    if owned >= i64::from(MAX_OWNED_GROUPS) {
        return Err(ApiError::forbidden("group_limit", GROUP_LIMIT));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateGroup {
    name: String,
    project: String,
    repo_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CreatedGroup {
    tenant: String,
    group: String,
    project: String,
    state: String,
    trial_ends_at: Option<Timestamp>,
    checkout_url: Option<String>,
}

/// `POST /v1/groups`. Hosted only. One transaction creates the tenant, its
/// plan, the trial grant when this GitHub account has none, the owner
/// membership, and the first project.
pub(crate) async fn create(
    State(hub): State<Hub>,
    caller: Caller,
    Json(request): Json<CreateGroup>,
) -> Result<(StatusCode, Json<CreatedGroup>), ApiError> {
    if hub.mode() != crate::config::HubMode::Hosted {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_hosted",
            "this hub is not agmen hosted",
        ));
    }
    let human = caller.human()?;
    checked_name(&request.name)?;
    let repo_url = checked_repo(request.repo_url.as_deref())?;
    let now = Timestamp::now();
    let mut tx = hub.pool().begin().await?;
    let made = insert_group(&mut tx, &human, &request, repo_url, now).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(made)))
}

fn checked_name(name: &str) -> Result<(), ApiError> {
    if crate::auth::is_human_handle(name) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_group_name",
            "a group name is 1 to 39 characters of lowercase letters, digits and hyphens, and does not start with a hyphen",
        ))
    }
}

fn checked_repo(value: Option<&str>) -> Result<Option<RepoUrl>, ApiError> {
    value
        .map(std::str::FromStr::from_str)
        .transpose()
        .map_err(|error: agmen_proto::BadRepoUrl| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "bad_repo_url",
                error.to_string(),
            )
        })
}

async fn insert_group(
    tx: &mut Transaction<'_, Postgres>,
    human: &HumanId,
    request: &CreateGroup,
    repo_url: Option<RepoUrl>,
    now: Timestamp,
) -> Result<CreatedGroup, ApiError> {
    let (handle, github_id) = human_row(tx, human).await?;
    check_owner_limit(tx, human).await?;
    let tenant = new_id()?;
    let stored = format!("{handle}/{}", request.name);
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, $2)")
        .bind(&tenant)
        .bind(&stored)
        .execute(&mut **tx)
        .await
        .map_err(map_tenant_insert)?;
    // The plan kind depends on whether this insert wins the one trial, so
    // the grant is written before the plan, in this same transaction.
    let trial = grant_trial(tx, &github_id, now).await?;
    let record = if trial {
        PlanRecord::trial(now)
    } else {
        PlanRecord::awaiting_payment(now)
    };
    let tenant_id: TenantId = tenant.parse().map_err(ApiError::stored)?;
    store_plan(tx, &tenant_id, &record).await?;
    join_tenant(&mut *tx, &tenant, human.as_str(), TenantRole::Owner).await?;
    let project = create_project(&mut *tx, &tenant, &request.project, repo_url, human).await?;
    let (state, trial_ends_at) = shown_plan(&record);
    Ok(CreatedGroup {
        tenant,
        group: stored,
        project: project.project,
        state,
        trial_ends_at,
        checkout_url: None,
    })
}

async fn human_row(
    tx: &mut Transaction<'_, Postgres>,
    human: &HumanId,
) -> Result<(String, String), ApiError> {
    let row: (String, String) =
        sqlx::query_as("SELECT handle, provider_sub FROM humans WHERE id = $1")
            .bind(human.as_str())
            .fetch_one(&mut **tx)
            .await?;
    Ok(row)
}

async fn grant_trial(
    tx: &mut Transaction<'_, Postgres>,
    github_id: &str,
    now: Timestamp,
) -> Result<bool, sqlx::Error> {
    let won: Option<String> = sqlx::query_scalar(
        "INSERT INTO trial_grants (github_id, granted_at) VALUES ($1, $2::timestamptz)
         ON CONFLICT (github_id) DO NOTHING
         RETURNING github_id",
    )
    .bind(github_id)
    .bind(now.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    Ok(won.is_some())
}

fn map_tenant_insert(err: sqlx::Error) -> ApiError {
    if let sqlx::Error::Database(db) = &err
        && db.is_unique_violation()
        && db.constraint() == Some("tenants_name_key")
    {
        return ApiError::conflict("group_exists", "you already have a group with that name");
    }
    ApiError::Database(err)
}

/// Sets `tenant`, which Paddle does not bill, to an active `plan`.
///
/// `plan` is `team`, `business-10`, `business-25`, or `business-50`. Connects
/// to `database_url`, runs migrations, and closes the pool before returning.
///
/// # Errors
///
/// [`SetPlanError::BadTenant`] when `tenant` is not a tenant id,
/// [`SetPlanError::BadPlan`] when `plan` is not one of those four names,
/// [`SetPlanError::NoTenant`] when no tenant has that id,
/// [`SetPlanError::PaddleBilled`] when the group has a Paddle customer or
/// subscription, and [`SetPlanError::Database`] when connecting or a query
/// fails.
///
/// # Examples
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// agmen_hub::apply_operator_plan("postgres://localhost/agmen", "abcdefgh", "team").await?;
/// # Ok(()) }
/// ```
pub async fn apply_operator_plan(
    database_url: &str,
    tenant: &str,
    plan: &str,
) -> Result<(), SetPlanError> {
    let tenant: TenantId = tenant.parse().map_err(|_| SetPlanError::BadTenant)?;
    let record = active_record(plan)?;
    let pool = db::connect(database_url).await?;
    let written = write_operator_plan(&pool, &tenant, &record).await;
    pool.close().await;
    written
}

fn active_record(plan: &str) -> Result<PlanRecord, SetPlanError> {
    let plan = match plan {
        "team" => Plan::Team { extra_seats: 0 },
        "business-10" => Plan::Business(BusinessStep::Seats10),
        "business-25" => Plan::Business(BusinessStep::Seats25),
        "business-50" => Plan::Business(BusinessStep::Seats50),
        _ => return Err(SetPlanError::BadPlan),
    };
    Ok(PlanRecord {
        state: PlanState::Active,
        plan: Some(plan),
        trial_started_at: None,
        past_due_since: None,
        read_only_cause: None,
        read_only_since: None,
        reminders_sent: 0,
        last_event_at: None,
    })
}

async fn write_operator_plan(
    pool: &sqlx::PgPool,
    tenant: &TenantId,
    record: &PlanRecord,
) -> Result<(), SetPlanError> {
    let mut tx = pool.begin().await?;
    let found: Option<String> =
        sqlx::query_scalar("SELECT id FROM tenants WHERE id = $1 FOR UPDATE")
            .bind(tenant.as_str())
            .fetch_optional(&mut *tx)
            .await?;
    if found.is_none() {
        return Err(SetPlanError::NoTenant);
    }
    // Overwriting a Paddle-billed plan would orphan its subscription: later
    // webhooks could no longer find the group.
    let billed: Option<bool> = sqlx::query_scalar(
        "SELECT paddle_customer_id IS NOT NULL OR paddle_subscription_id IS NOT NULL
         FROM tenant_plans WHERE tenant_id = $1",
    )
    .bind(tenant.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    if billed == Some(true) {
        return Err(SetPlanError::PaddleBilled);
    }
    store_plan(&mut tx, tenant, record).await?;
    tx.commit().await?;
    Ok(())
}

struct PlanColumns {
    plan: Option<&'static str>,
    extra_seats: i32,
    business_step: Option<i32>,
}

fn plan_columns(plan: Option<Plan>) -> Result<PlanColumns, sqlx::Error> {
    match plan {
        None => Ok(PlanColumns {
            plan: None,
            extra_seats: 0,
            business_step: None,
        }),
        Some(Plan::Team { extra_seats }) => Ok(PlanColumns {
            plan: Some("team"),
            extra_seats: i32::try_from(extra_seats).map_err(decode)?,
            business_step: None,
        }),
        Some(Plan::Business(step)) => Ok(PlanColumns {
            plan: Some("business"),
            extra_seats: 0,
            business_step: Some(i32::try_from(step.seats()).map_err(decode)?),
        }),
    }
}

fn cause_str(cause: Option<ReadOnlyCause>) -> Option<&'static str> {
    cause.map(|cause| match cause {
        ReadOnlyCause::TrialEnded => "trial_ended",
        ReadOnlyCause::AwaitingPayment => "awaiting_payment",
        ReadOnlyCause::PaymentFailed => "payment_failed",
        ReadOnlyCause::Canceled => "canceled",
    })
}

#[derive(sqlx::FromRow)]
struct PlanRow {
    tenant_id: String,
    state: String,
    plan: Option<String>,
    extra_seats: i32,
    business_step: Option<i32>,
    trial_started_at: Option<i64>,
    past_due_since: Option<i64>,
    read_only_cause: Option<String>,
    read_only_since: Option<i64>,
    reminders_sent: i16,
    last_event_at: Option<i64>,
}

fn record_from_row(row: PlanRow) -> Result<PlanRecord, sqlx::Error> {
    Ok(PlanRecord {
        state: row.state.parse().map_err(decode)?,
        plan: plan_from_columns(row.plan, row.extra_seats, row.business_step)?,
        trial_started_at: read_stamp(row.trial_started_at)?,
        past_due_since: read_stamp(row.past_due_since)?,
        read_only_cause: read_cause(row.read_only_cause)?,
        read_only_since: read_stamp(row.read_only_since)?,
        reminders_sent: u8::try_from(row.reminders_sent).map_err(decode)?,
        last_event_at: read_stamp(row.last_event_at)?,
    })
}

fn plan_from_columns(
    plan: Option<String>,
    extra_seats: i32,
    business_step: Option<i32>,
) -> Result<Option<Plan>, sqlx::Error> {
    let seats = u32::try_from(extra_seats).map_err(decode)?;
    match plan.as_deref() {
        None => Ok(None),
        Some("team") => Ok(Some(Plan::Team { extra_seats: seats })),
        Some("business") => {
            let step = business_step.ok_or_else(|| decode_msg("business plan has no step"))?;
            let step = match step {
                10 => BusinessStep::Seats10,
                25 => BusinessStep::Seats25,
                50 => BusinessStep::Seats50,
                _ => return Err(decode_msg("business step is not 10, 25, or 50")),
            };
            Ok(Some(Plan::Business(step)))
        }
        Some(_) => Err(decode_msg("plan is not team or business")),
    }
}

fn read_cause(value: Option<String>) -> Result<Option<ReadOnlyCause>, sqlx::Error> {
    value
        .map(|value| match value.as_str() {
            "trial_ended" => Ok(ReadOnlyCause::TrialEnded),
            "awaiting_payment" => Ok(ReadOnlyCause::AwaitingPayment),
            "payment_failed" => Ok(ReadOnlyCause::PaymentFailed),
            "canceled" => Ok(ReadOnlyCause::Canceled),
            _ => Err(decode_msg("read_only_cause is not a known cause")),
        })
        .transpose()
}

fn stamp(value: Option<Timestamp>) -> Option<String> {
    value.map(|value| value.to_string())
}

fn read_stamp(micros: Option<i64>) -> Result<Option<Timestamp>, sqlx::Error> {
    micros
        .map(|micros| Timestamp::from_microsecond(micros).map_err(decode))
        .transpose()
}

fn decode(err: impl std::error::Error + Send + Sync + 'static) -> sqlx::Error {
    sqlx::Error::Decode(Box::new(err))
}

fn decode_msg(message: &'static str) -> sqlx::Error {
    decode(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> Timestamp {
        value.parse().unwrap()
    }

    #[test]
    fn group_name_rule() {
        assert!(checked_name("acme").is_ok());
        assert!(checked_name(&"a".repeat(39)).is_ok());
        assert!(checked_name("a-b").is_ok());
        for name in ["Acme", "-acme", "", "has space", "has_underscore"] {
            assert!(matches!(
                checked_name(name),
                Err(ApiError::Client {
                    code: "bad_group_name",
                    ..
                })
            ));
        }
        assert!(checked_name(&"a".repeat(40)).is_err());
    }

    #[test]
    fn operator_plan_names() {
        let team = active_record("team").unwrap();
        assert_eq!(team.state, PlanState::Active);
        assert_eq!(team.plan, Some(Plan::Team { extra_seats: 0 }));
        assert!(team.trial_started_at.is_none());
        assert!(team.read_only_since.is_none());
        assert_eq!(
            active_record("business-10").unwrap().plan,
            Some(Plan::Business(BusinessStep::Seats10))
        );
        assert_eq!(
            active_record("business-25").unwrap().plan,
            Some(Plan::Business(BusinessStep::Seats25))
        );
        assert_eq!(
            active_record("business-50").unwrap().plan,
            Some(Plan::Business(BusinessStep::Seats50))
        );
        assert!(matches!(active_record("free"), Err(SetPlanError::BadPlan)));
    }

    #[test]
    fn trial_notice_starts_on_day_5() {
        let start = at("2026-11-01T00:00:00Z");
        let record = PlanRecord::trial(start);
        let before = start + Duration::from_secs(4 * SECS_PER_DAY - 1);
        assert!(owner_notice("andy/acme", &record, before).is_none());
        let day5 = start + Duration::from_secs(4 * SECS_PER_DAY);
        let notice = owner_notice("andy/acme", &record, day5).unwrap();
        assert!(notice.contains("andy/acme"), "{notice}");
        assert!(notice.contains("agmen group billing"), "{notice}");
        assert!(notice.contains("2026-11-08"), "{notice}");
        assert!(owner_notice("andy/acme", &PlanRecord::trial(start), start).is_none());
    }

    #[test]
    fn past_due_and_read_only_notices_name_the_date() {
        let start = at("2026-11-01T00:00:00Z");
        let past_due = PlanRecord {
            state: PlanState::PastDue,
            plan: Some(Plan::Team { extra_seats: 0 }),
            trial_started_at: None,
            past_due_since: Some(start),
            read_only_cause: None,
            read_only_since: None,
            reminders_sent: 0,
            last_event_at: None,
        };
        let notice = owner_notice("andy/acme", &past_due, start).unwrap();
        assert!(notice.contains("andy/acme"), "{notice}");
        assert!(notice.contains("agmen group billing"), "{notice}");
        assert!(notice.contains("2026-11-15"), "{notice}");
        let waiting = PlanRecord::awaiting_payment(start);
        let notice = owner_notice("andy/beta", &waiting, start).unwrap();
        assert!(notice.contains("andy/beta"), "{notice}");
        assert!(notice.contains("agmen group billing"), "{notice}");
        assert!(notice.contains("2026-11-15"), "{notice}");
        // The date is the deletion date, and the notice must say so: "read-only
        // until" alone reads as if writes come back on that day.
        assert!(notice.contains("deleted on 2026-11-15"), "{notice}");
        let active = active_record("team").unwrap();
        assert!(owner_notice("andy/acme", &active, start).is_none());
    }
}
