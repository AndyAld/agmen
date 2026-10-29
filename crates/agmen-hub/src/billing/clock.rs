//! The hosted plan clock. Once a minute it walks the groups whose plan has
//! a deadline due: trial reminders, read-only, and deletion. The rules live
//! in [`PlanRecord::tick`]; this module reads the plan row under a lock,
//! stores what `tick` returns, and then carries out its effects.

use std::time::Duration;

use agmen_proto::{ProjectId, TenantId, Timestamp, Topics};

use crate::PostError;
use crate::billing::{
    PAID_READ_ONLY_DAYS, PAST_DUE_GRACE_DAYS, PlanEffect, PlanState, TRIAL_READ_ONLY_DAYS,
    TRIAL_REMINDER_OFFSETS_DAYS,
};
use crate::config::HubMode;
use crate::groups::{load_plan, store_plan};
use crate::http::{Hub, error_chain};
use crate::identities::{ban_and_kick, ban_until};
use crate::usage::post_owner_alert;

/// Tenants loaded per keyset page.
const PAGE_SIZE: i64 = 100;
/// Pages one pass reads at most.
const MAX_PAGES: u32 = 100;
/// Tenants one pass changes or fails at most.
const MAX_TENANTS: usize = 100;
/// Times one deletion revokes and bans again because an identity was minted
/// while it ran.
const MAX_REVOKE_ROUNDS: u32 = 5;
/// Identities one group may have when it is deleted.
const MAX_IDENTITIES: i64 = 10_000;
/// Message rows one delete statement removes.
const MESSAGE_BATCH: i64 = 10_000;
/// Message delete statements one deletion runs at most.
const MAX_MESSAGE_BATCHES: u32 = 1_000;
/// Projects whose roster is republished after a deletion.
const MAX_ROSTER_PROJECTS: i64 = 1_000;
/// Failed passes in a row before the task gives up. Only a pass-level failure
/// (the candidate query) counts; a failed group does not.
const MAX_FAILURES: u32 = 10;
const EVERY: Duration = Duration::from_secs(60);
const RETRY_DELAY: Duration = Duration::from_secs(1);
const SECS_PER_DAY: u64 = 86_400;

/// Locks run in the order `mint` takes its locks: plan, then membership,
/// then project. Every path that takes more than one keeps that order.
const LOCK_PLAN: &str = "SELECT count(*) FROM (
         SELECT 1 FROM tenant_plans WHERE tenant_id = $1 FOR UPDATE) locked";
const LOCK_MEMBERSHIPS: &str = "SELECT count(*) FROM (
         SELECT 1 FROM memberships
         WHERE project_id IN (SELECT id FROM projects WHERE tenant_id = $1) FOR UPDATE) locked";
const LOCK_PROJECTS: &str =
    "SELECT count(*) FROM (SELECT 1 FROM projects WHERE tenant_id = $1 FOR UPDATE) locked";

// shortcut: SQL picks a superset by the earliest offset and Rust decides; store a
// next_deadline_at column if candidate scans show in clock latency
const CANDIDATES: &str = "SELECT tenant_id FROM tenant_plans
     WHERE tenant_id > $1 AND (
         state = 'deleted'
         OR (state = 'trial' AND trial_started_at <= $2::text::timestamptz)
         OR (state = 'past_due' AND past_due_since <= $3::text::timestamptz)
         OR (state = 'read_only' AND read_only_since <= $4::text::timestamptz)
     )
     ORDER BY tenant_id LIMIT $5";

/// Why a clock pass or a group deletion failed, or [`Hub::run_plan_clock`] stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlanClockError {
    /// A query failed, or a stored value was out of range or over a bound.
    #[error("plan clock database query failed")]
    Database(#[from] sqlx::Error),
    /// Posting a trial reminder failed.
    #[error("posting a trial reminder failed")]
    Post(#[from] PostError),
    /// The broker refused to ban or kick identities of a group being
    /// deleted, so nothing of the group was deleted.
    #[error("the broker failed for {failed} identities of group {tenant}")]
    Broker {
        /// The group.
        tenant: String,
        /// Identities whose ban or kick failed.
        failed: usize,
    },
    /// Passes failed too many times in a row.
    #[error("the plan clock failed {failures} times in a row")]
    GaveUp {
        /// Passes that failed in a row.
        failures: u32,
        /// The last failure.
        #[source]
        source: Box<PlanClockError>,
    },
}

/// The SQL cutoffs of one pass: a plan whose deadline is later than its
/// cutoff cannot have anything due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cutoffs {
    trial: Timestamp,
    past_due: Timestamp,
    read_only: Timestamp,
}

impl Cutoffs {
    fn at(now: Timestamp) -> Self {
        let before = |days: u32| {
            now.checked_sub(Duration::from_secs(u64::from(days) * SECS_PER_DAY))
                // Nothing is stored before MIN, so nothing is due when the
                // window reaches past it.
                .unwrap_or(Timestamp::MIN)
        };
        Self {
            trial: before(TRIAL_REMINDER_OFFSETS_DAYS[0]),
            past_due: before(PAST_DUE_GRACE_DAYS),
            read_only: before(TRIAL_READ_ONLY_DAYS.min(PAID_READ_ONLY_DAYS)),
        }
    }
}

/// The text `groups::trial_notice` shows in `/v1/me`.
fn reminder_text(name: &str, ends_at: Timestamp) -> String {
    format!(
        "Your agmen hosted trial of {name} ends on {ends_at}. Run agmen group billing to keep going."
    )
}

impl Hub {
    /// Runs the plan clock every minute, retrying a failed pass after one
    /// second. Ten consecutive failures end the task.
    /// [`Hub::start_messaging`] starts it for hosted hubs. Dropping
    /// messaging stops the task.
    ///
    /// # Errors
    ///
    /// [`PlanClockError::GaveUp`] with the last failed pass.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(hub: agmen_hub::Hub) -> Result<(), agmen_hub::PlanClockError> {
    /// hub.run_plan_clock().await?;
    /// # Ok(()) }
    /// ```
    pub async fn run_plan_clock(&self) -> Result<(), PlanClockError> {
        let mut failures = 0;
        loop {
            match tick_plans(self, Timestamp::now()).await {
                Ok(changed) => {
                    failures = 0;
                    if changed > 0 {
                        tracing::info!(changed, "plan clock pass finished");
                    }
                    tokio::time::sleep(EVERY).await;
                }
                Err(source) => {
                    failures += 1;
                    if failures >= MAX_FAILURES {
                        return Err(PlanClockError::GaveUp {
                            failures,
                            source: Box::new(source),
                        });
                    }
                    tracing::warn!(error = %error_chain(&source), failures, "plan clock failed; retrying");
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
    }
}

/// Runs the clock for every group with something due at `now` and returns
/// how many it changed. One pass looks at most at [`MAX_PAGES`] pages of
/// [`PAGE_SIZE`] candidates and stops after [`MAX_TENANTS`] changed or failed
/// groups. A group that fails is logged and skipped (see [`tenant_outcome`]);
/// only a failed candidate query fails the pass. Self-host hubs return 0
/// without a query.
pub(crate) async fn tick_plans(hub: &Hub, now: Timestamp) -> Result<usize, PlanClockError> {
    if hub.mode() != HubMode::Hosted {
        return Ok(0);
    }
    let cutoffs = Cutoffs::at(now);
    let mut after = String::new();
    let mut handled = 0;
    'pages: for _ in 0..MAX_PAGES {
        let page: Vec<String> = sqlx::query_scalar(CANDIDATES)
            .bind(&after)
            .bind(cutoffs.trial.to_string())
            .bind(cutoffs.past_due.to_string())
            .bind(cutoffs.read_only.to_string())
            .bind(PAGE_SIZE)
            .fetch_all(hub.pool())
            .await?;
        for tenant in &page {
            if tenant_outcome(tenant, tick_one(hub, tenant, now).await) {
                handled += 1;
            }
            if handled >= MAX_TENANTS {
                break 'pages;
            }
        }
        match page.last() {
            Some(last) if page.len() == usize::try_from(PAGE_SIZE).unwrap_or(usize::MAX) => {
                after.clone_from(last);
            }
            _ => break,
        }
    }
    Ok(handled)
}

/// Ticks one group under its plan row lock, then carries out the effects
/// after the commit. Returns whether the tick changed the record or
/// emitted an effect. A group whose plan row is gone is skipped.
async fn tick_one(hub: &Hub, tenant: &str, now: Timestamp) -> Result<bool, PlanClockError> {
    let id: TenantId = tenant
        .parse()
        .map_err(|err| sqlx::Error::Decode(Box::new(err)))?;
    let mut tx = hub.pool().begin().await?;
    let locked: Option<String> =
        sqlx::query_scalar("SELECT tenant_id FROM tenant_plans WHERE tenant_id = $1 FOR UPDATE")
            .bind(tenant)
            .fetch_optional(&mut *tx)
            .await?;
    let record = match locked {
        Some(_) => load_plan(&mut *tx, &id).await?,
        None => None,
    };
    let Some(record) = record else {
        return Ok(false);
    };
    let (next, mut effects) = record.tick(now);
    if record.state == PlanState::Deleted {
        // Stored as deleted, so an earlier delete did not finish.
        effects.push(PlanEffect::Delete);
    }
    if next != record {
        store_plan(&mut tx, &id, &next).await?;
    }
    tx.commit().await?;
    let counted = next != record || !effects.is_empty();
    for effect in effects {
        match effect {
            PlanEffect::TrialReminder { ends_at, .. } => remind(hub, &id, ends_at).await?,
            PlanEffect::BecameReadOnly(cause) => {
                tracing::info!(%id, ?cause, "group became read-only");
            }
            PlanEffect::Delete => delete_group(hub, &id).await?,
        }
    }
    Ok(counted)
}

/// What one pass does with the result of ticking one group, and whether the
/// group counts toward the pass limit. A group that fails (a stuck deletion,
/// a broker refusal, a bad row) is logged and skipped, and still counts: its
/// stored state makes the next pass retry it. Returning the failure would end
/// the pass for every group after it, and ten such passes in a row end the
/// whole hub (`main` exits when messaging stops).
fn tenant_outcome(tenant: &str, result: Result<bool, PlanClockError>) -> bool {
    match result {
        Ok(counted) => counted,
        Err(err) => {
            tracing::warn!(tenant, error = %error_chain(&err), "plan clock failed for a group; the next pass retries");
            true
        }
    }
}

/// Posts the trial reminder. A failure is logged: the record already counts
/// the reminder as sent.
async fn remind(hub: &Hub, tenant: &TenantId, ends_at: Timestamp) -> Result<(), PlanClockError> {
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM tenants WHERE id = $1")
        .bind(tenant.as_str())
        .fetch_optional(hub.pool())
        .await?;
    let Some(name) = name else {
        tracing::warn!(%tenant, "no trial reminder: the group is gone");
        return Ok(());
    };
    if let Err(err) = post_owner_alert(hub, tenant, &reminder_text(&name, ends_at)).await {
        tracing::warn!(%tenant, error = %error_chain(&err), "posting a trial reminder failed");
    }
    Ok(())
}

/// Deletes `tenant`: revokes, bans and kicks every identity (stopping if the
/// broker fails, before any row goes), then removes every row that names the
/// group or its projects. Keeps `trial_grants` and everything per person.
/// Repeating it for a group that is gone does nothing. An identity minted
/// while it runs is revoked and banned in a further round, at most
/// [`MAX_REVOKE_ROUNDS`] (see [`delete_rows`]).
pub(crate) async fn delete_group(hub: &Hub, tenant: &TenantId) -> Result<(), PlanClockError> {
    let mut projects = None;
    for _ in 0..MAX_REVOKE_ROUNDS {
        revoke_and_ban(hub, tenant).await?;
        delete_messages(hub, tenant).await?;
        projects = delete_rows(hub, tenant).await?;
        if projects.is_some() {
            break;
        }
    }
    let Some(projects) = projects else {
        return Err(PlanClockError::Database(sqlx::Error::Decode(
            format!("identities kept appearing after {MAX_REVOKE_ROUNDS} revoke rounds").into(),
        )));
    };
    for project in projects {
        match project.parse::<ProjectId>() {
            // The roster query finds nothing, so the retained head becomes empty.
            Ok(project) => hub
                .presence()
                .roster_changed(&Topics::new(tenant.clone(), project)),
            Err(err) => {
                tracing::error!(%tenant, %project, error = %err, "a stored project id is invalid; its roster was not published")
            }
        }
    }
    tracing::info!(%tenant, "deleted group");
    Ok(())
}

/// Revokes every identity of the group, then bans and kicks each one. Stops
/// with [`PlanClockError::Broker`] if the broker fails, before any row goes.
async fn revoke_and_ban(hub: &Hub, tenant: &TenantId) -> Result<(), PlanClockError> {
    let pool = hub.pool();
    let known: i64 = sqlx::query_scalar("SELECT count(*) FROM identities WHERE tenant_id = $1")
        .bind(tenant.as_str())
        .fetch_one(pool)
        .await?;
    if known > MAX_IDENTITIES {
        return Err(too_many_identities());
    }
    // Commit the revocation before banning, so no identity can get a fresh
    // broker token while the rest runs (as `identities::revoke` does).
    let revoked: Vec<(String, i64)> = sqlx::query_as(
        "UPDATE identities SET revoked_at = coalesce(revoked_at, now()) WHERE tenant_id = $1
         RETURNING id, extract(epoch FROM revoked_at)::bigint",
    )
    .bind(tenant.as_str())
    .fetch_all(pool)
    .await?;
    if i64::try_from(revoked.len()).map_or(true, |len| len > MAX_IDENTITIES) {
        return Err(too_many_identities());
    }
    let mut failed = 0;
    for (id, revoked_at) in &revoked {
        if ban_and_kick(hub, id, ban_until(*revoked_at))
            .await
            .is_some()
        {
            failed += 1;
        }
    }
    if failed > 0 {
        return Err(PlanClockError::Broker {
            tenant: tenant.to_string(),
            failed,
        });
    }
    Ok(())
}

fn too_many_identities() -> PlanClockError {
    PlanClockError::Database(sqlx::Error::Decode(
        format!("a group has more than {MAX_IDENTITIES} identities").into(),
    ))
}

/// Deletes the group's messages in batches, so no statement holds its locks
/// for a year of history. Past [`MAX_MESSAGE_BATCHES`] it stops with an
/// error and the next pass continues.
async fn delete_messages(hub: &Hub, tenant: &TenantId) -> Result<(), PlanClockError> {
    for _ in 0..MAX_MESSAGE_BATCHES {
        let deleted = sqlx::query(
            "DELETE FROM messages WHERE ctid IN (
                 SELECT m.ctid FROM messages m JOIN projects p ON p.id = m.project_id
                 WHERE p.tenant_id = $1 LIMIT $2)",
        )
        .bind(tenant.as_str())
        .bind(MESSAGE_BATCH)
        .execute(hub.pool())
        .await?
        .rows_affected();
        if deleted == 0 {
            return Ok(());
        }
    }
    Err(PlanClockError::Database(sqlx::Error::Decode(
        format!("{MAX_MESSAGE_BATCHES} message batches did not empty the group").into(),
    )))
}

/// Deletes every other row of the group in one transaction, children before
/// parents, and returns the ids of the projects it deleted.
///
/// `mint` is the only way an identity appears, and it share-locks the
/// group's `tenant_plans` row, then the member's `memberships` row and,
/// through the foreign key, the project row, and refuses a group whose plan
/// is `deleted`. Locking all three here, in that order, makes a mint that is
/// not yet committed finish first and a later one fail. Then any identity
/// still unrevoked was minted after the last [`revoke_and_ban`], so this
/// returns `None` and deletes nothing, and the caller revokes and bans again.
/// Taking the plan lock first also keeps the final `DELETE FROM tenant_plans`
/// from inverting the order against a mint.
async fn delete_rows(hub: &Hub, tenant: &TenantId) -> Result<Option<Vec<String>>, PlanClockError> {
    // `stream_heads` is keyed by "<project_id>/<stream>". `left` compares
    // the prefix as text, where LIKE would treat `_` in an id as a wildcard.
    const DELETES: [&str; 18] = [
        "DELETE FROM lane_lends WHERE project_id IN (SELECT id FROM projects WHERE tenant_id = $1)",
        "DELETE FROM claims WHERE project_id IN (SELECT id FROM projects WHERE tenant_id = $1)",
        "DELETE FROM items WHERE project_id IN (SELECT id FROM projects WHERE tenant_id = $1)",
        "DELETE FROM item_counters WHERE project_id IN (SELECT id FROM projects WHERE tenant_id = $1)",
        "DELETE FROM lanes WHERE project_id IN (SELECT id FROM projects WHERE tenant_id = $1)",
        "DELETE FROM publish_outbox WHERE tenant_id = $1",
        "DELETE FROM stream_heads s USING projects p
         WHERE p.tenant_id = $1 AND left(s.stream, length(p.id) + 1) = p.id || '/'",
        "DELETE FROM tokens WHERE identity_id IN (SELECT id FROM identities WHERE tenant_id = $1)",
        "DELETE FROM identities WHERE tenant_id = $1",
        "DELETE FROM invites WHERE tenant_id = $1",
        "DELETE FROM memberships WHERE project_id IN (SELECT id FROM projects WHERE tenant_id = $1)",
        "DELETE FROM bootstrap_admins WHERE project_id IN (SELECT id FROM projects WHERE tenant_id = $1)",
        "DELETE FROM hub_group WHERE tenant_id = $1",
        "DELETE FROM tenant_members WHERE tenant_id = $1",
        "DELETE FROM tenant_usage WHERE tenant_id = $1",
        "DELETE FROM tenant_plans WHERE tenant_id = $1",
        "DELETE FROM projects WHERE tenant_id = $1",
        "DELETE FROM tenants WHERE id = $1",
    ];
    let mut tx = hub.pool().begin().await?;
    for lock in [LOCK_PLAN, LOCK_MEMBERSHIPS, LOCK_PROJECTS] {
        sqlx::query(lock)
            .bind(tenant.as_str())
            .execute(&mut *tx)
            .await?;
    }
    let unrevoked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM identities WHERE tenant_id = $1 AND revoked_at IS NULL",
    )
    .bind(tenant.as_str())
    .fetch_one(&mut *tx)
    .await?;
    if unrevoked > 0 {
        return Ok(None);
    }
    let projects: Vec<String> =
        sqlx::query_scalar("SELECT id FROM projects WHERE tenant_id = $1 ORDER BY id LIMIT $2")
            .bind(tenant.as_str())
            .bind(MAX_ROSTER_PROJECTS)
            .fetch_all(&mut *tx)
            .await?;
    for sql in DELETES {
        sqlx::query(sql)
            .bind(tenant.as_str())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Some(projects))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> Timestamp {
        value.parse().unwrap()
    }

    #[test]
    fn reminder_text_matches_the_trial_notice() {
        assert_eq!(
            reminder_text("andy/acme", at("2026-11-05T00:00:00Z")),
            "Your agmen hosted trial of andy/acme ends on 2026-11-05T00:00:00Z. Run agmen group billing to keep going."
        );
    }

    #[test]
    fn a_failed_group_does_not_stop_the_pass() {
        let broker = PlanClockError::Broker {
            tenant: "acmeacme".into(),
            failed: 1,
        };
        let database = PlanClockError::Database(sqlx::Error::Decode(
            format!("a group has more than {MAX_IDENTITIES} identities").into(),
        ));
        // A failed group counts toward the pass limit, so stuck groups
        // cannot make one pass do unbounded work.
        assert!(tenant_outcome("acmeacme", Err(broker)));
        assert!(tenant_outcome("acmeacme", Err(database)));
        assert!(tenant_outcome("acmeacme", Ok(true)));
        assert!(!tenant_outcome("acmeacme", Ok(false)));
    }

    #[test]
    fn cutoffs_come_from_the_plan_constants() {
        let now = at("2026-10-29T12:00:00Z");
        let back = |days: u32| now - Duration::from_secs(u64::from(days) * SECS_PER_DAY);
        let cutoffs = Cutoffs::at(now);
        assert_eq!(cutoffs.trial, back(TRIAL_REMINDER_OFFSETS_DAYS[0]));
        assert_eq!(cutoffs.past_due, back(PAST_DUE_GRACE_DAYS));
        assert_eq!(
            cutoffs.read_only,
            back(TRIAL_READ_ONLY_DAYS.min(PAID_READ_ONLY_DAYS))
        );
        assert_eq!(cutoffs.trial, at("2026-10-25T12:00:00Z"));
    }
}
