//! Message history retention. Each batch selects the oldest expired rows
//! of one tenant and deletes a prefix of each stream they name. A later
//! seq can have an earlier hub timestamp, so a prefix may also remove
//! newer rows below its cut. Limiting candidates bounds the cut selection,
//! not the number of rows in those prefixes.

use std::collections::HashMap;
use std::time::Duration;

use agmen_proto::Timestamp;
use sqlx::PgPool;

use crate::billing::Retention;
use crate::config::HubMode;
use crate::http::{Hub, error_chain};

/// Expired rows considered by one batch.
const BATCH_SIZE: i64 = 5_000;
/// Transactions per pass; an hourly pass continues the remaining history.
const MAX_BATCHES: u32 = 200;
/// Tenants loaded per keyset page.
const TENANT_PAGE: i64 = 500;
/// Failed passes in a row before the task gives up.
const MAX_FAILURES: u32 = 10;
const EVERY: Duration = Duration::from_secs(3600);
const RETRY_DELAY: Duration = Duration::from_secs(1);

const CANDIDATES: &str = "SELECT project_id, stream, max(seq) FROM (
         SELECT m.project_id, m.stream, m.seq
         FROM messages m JOIN projects p ON p.id = m.project_id
         WHERE p.tenant_id = $1 AND m.hub_at < $2::text::timestamptz
         ORDER BY m.hub_at, m.project_id, m.stream, m.seq LIMIT $3
     ) candidates GROUP BY project_id, stream ORDER BY project_id, stream";

/// Why a retention pass failed or [`Hub::run_retention`] stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RetentionError {
    /// Reading the tenants or plans, or deleting a prefix, failed.
    #[error("retention database query failed")]
    Database(#[from] sqlx::Error),
    /// Passes failed too many times in a row.
    #[error("retention failed {failures} times in a row")]
    GaveUp {
        /// Passes that failed in a row.
        failures: u32,
        /// The last failure.
        #[source]
        source: Box<RetentionError>,
    },
}

impl Hub {
    /// Prunes message history at startup and hourly thereafter, retrying a
    /// failed pass after one second. Ten consecutive failures end the task.
    /// [`Hub::start_messaging`] starts it for hosted hubs and self-host hubs
    /// with a retention limit. Dropping messaging stops the task.
    ///
    /// # Errors
    ///
    /// [`RetentionError::GaveUp`] with the last failed pass.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(hub: agmen_hub::Hub) -> Result<(), agmen_hub::RetentionError> {
    /// hub.run_retention().await?;
    /// # Ok(()) }
    /// ```
    pub async fn run_retention(&self) -> Result<(), RetentionError> {
        let mut failures = 0;
        loop {
            match prune_once(self, Timestamp::now()).await {
                Ok(deleted) => {
                    failures = 0;
                    tracing::info!(deleted, "retention pass finished");
                    tokio::time::sleep(EVERY).await;
                }
                Err(source) => {
                    failures += 1;
                    if failures >= MAX_FAILURES {
                        return Err(RetentionError::GaveUp {
                            failures,
                            source: Box::new(source),
                        });
                    }
                    tracing::warn!(error = %error_chain(&source), failures, "retention failed; retrying");
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
    }
}

fn cutoff(now: Timestamp, retention: Retention) -> Option<Timestamp> {
    match retention {
        Retention::Days(days) => Some(
            now.checked_sub(Duration::from_secs(u64::from(days) * 86_400))
                // No stored timestamp can precede MIN, so nothing expires
                // when the retention window extends past the time range.
                .unwrap_or(Timestamp::MIN),
        ),
        Retention::Forever => None,
    }
}

/// Prunes at most [`MAX_BATCHES`] batches using the hub's retention rules.
/// Hosted tenants without a plan keep everything, in any plan state.
/// Self-host without a limit does no database work. The initial tenant
/// count bounds the keyset scan; tenants added during it may wait one pass.
pub(crate) async fn prune_once(hub: &Hub, now: Timestamp) -> Result<u64, RetentionError> {
    let mode = hub.mode();
    if mode == HubMode::SelfHost && hub.retention_days().is_none() {
        return Ok(0);
    }
    let tenants: i64 = sqlx::query_scalar("SELECT count(*) FROM tenants")
        .fetch_one(hub.pool())
        .await?;
    let mut after = String::new();
    let mut deleted = 0;
    let mut batches = 0;
    for _ in 0..=tenants / TENANT_PAGE {
        let tenants: Vec<String> =
            sqlx::query_scalar("SELECT id FROM tenants WHERE id > $1 ORDER BY id LIMIT $2")
                .bind(&after)
                .bind(TENANT_PAGE)
                .fetch_all(hub.pool())
                .await?;
        let Some(last) = tenants.last() else {
            break;
        };
        after.clone_from(last);
        let plans = if mode == HubMode::Hosted {
            crate::groups::load_plans(hub.pool(), &tenants).await?
        } else {
            HashMap::new()
        };
        for tenant in tenants {
            let retention = match mode {
                HubMode::Hosted => plans
                    .get(&tenant)
                    .map_or(Retention::Forever, |plan| plan.retention()),
                HubMode::SelfHost => hub
                    .retention_days()
                    .map_or(Retention::Forever, Retention::Days),
            };
            let Some(cutoff) = cutoff(now, retention) else {
                continue;
            };
            // Fresh tenants must not consume the transaction budget and
            // prevent an expired tenant on a later page from being pruned.
            let expired: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM messages m JOIN projects p ON p.id = m.project_id
                     WHERE p.tenant_id = $1 AND m.hub_at < $2::text::timestamptz)",
            )
            .bind(&tenant)
            .bind(cutoff.to_string())
            .fetch_one(hub.pool())
            .await?;
            if !expired {
                continue;
            }
            let remaining = MAX_BATCHES - batches;
            for _ in 0..remaining {
                let count = prune_batch(hub.pool(), &tenant, cutoff).await?;
                batches += 1;
                deleted += count;
                if batches == MAX_BATCHES {
                    return Ok(deleted);
                }
                if count == 0 {
                    break;
                }
            }
        }
    }
    Ok(deleted)
}

/// Selects at most [`BATCH_SIZE`] expired candidates, then deletes their
/// streams' full prefixes and raises their pruning markers in one transaction.
/// The prefixes can exceed the candidate count when timestamps go backwards.
async fn prune_batch(
    pool: &PgPool,
    tenant: &str,
    cutoff: Timestamp,
) -> Result<u64, RetentionError> {
    let mut tx = pool.begin().await?;
    let cuts: Vec<(String, String, i64)> = sqlx::query_as(CANDIDATES)
        .bind(tenant)
        .bind(cutoff.to_string())
        .bind(BATCH_SIZE)
        .fetch_all(&mut *tx)
        .await?;
    let (mut projects, mut streams, mut seqs) = (
        Vec::with_capacity(cuts.len()),
        Vec::with_capacity(cuts.len()),
        Vec::with_capacity(cuts.len()),
    );
    for (project, stream, seq) in cuts {
        projects.push(project);
        streams.push(stream);
        seqs.push(seq);
    }
    let deleted = sqlx::query(
        "DELETE FROM messages m
         USING UNNEST($1::text[], $2::text[], $3::bigint[]) AS cuts(project_id, stream, seq)
         WHERE m.project_id = cuts.project_id AND m.stream = cuts.stream AND m.seq <= cuts.seq",
    )
    .bind(&projects)
    .bind(&streams)
    .bind(&seqs)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // Ingest locks the head before inserting messages. Delete first so
    // pruning never holds a head lock while waiting for a message row.
    // Lock heads in ingest's stream order before the unordered bulk update.
    sqlx::query(
        "SELECT h.stream FROM stream_heads h
         JOIN UNNEST($1::text[], $2::text[]) AS cuts(project_id, stream)
           ON h.stream = cuts.project_id || '/' || cuts.stream
         ORDER BY h.stream FOR UPDATE OF h",
    )
    .bind(&projects)
    .bind(&streams)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE stream_heads h SET pruned_through = GREATEST(h.pruned_through, cuts.seq)
         FROM UNNEST($1::text[], $2::text[], $3::bigint[]) AS cuts(project_id, stream, seq)
         WHERE h.stream = cuts.project_id || '/' || cuts.stream",
    )
    .bind(&projects)
    .bind(&streams)
    .bind(&seqs)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::Retention;
    use agmen_proto::Timestamp;

    #[test]
    fn cutoff_is_now_minus_days() {
        let now: Timestamp = "2026-11-02T00:00:00Z".parse().unwrap();
        assert_eq!(
            cutoff(now, Retention::Days(90)),
            Some("2026-08-04T00:00:00Z".parse().unwrap())
        );
        assert_eq!(cutoff(now, Retention::Forever), None);
    }

    #[tokio::test]
    async fn it_batch_deletes_at_most_5000_of_ordered_rows() {
        let base = std::env::var("AGMEN_IT_PG").expect("AGMEN_IT_PG is set for it tests");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&base)
            .await
            .unwrap();
        let name = format!("agmen_retention_{}", crate::db::new_id().unwrap());
        // The id uses only lowercase letters and digits, so this DDL is safe.
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&admin)
            .await
            .unwrap();
        let mut url = url::Url::parse(&base).unwrap();
        url.set_path(&name);
        let pool = crate::db::connect(url.as_str()).await.unwrap();
        let outcome = tokio::spawn(async move {
            sqlx::query("INSERT INTO tenants (id, name) VALUES ('retention', 'retention')")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO projects (id, tenant_id, name) VALUES ('retention', 'retention', 'retention')")
                .execute(&pool).await.unwrap();
            sqlx::query(
                "INSERT INTO messages
                    (project_id, stream, seq, id, from_id, payload_hash, envelope, hub_at, ack_row)
                 SELECT 'retention', 'feed', s, s::text, 'retention', '\\x00'::bytea, '{}'::jsonb,
                        '2026-01-01T00:00:00Z'::timestamptz + s * interval '1 second', true
                 FROM generate_series(1, 12000) s",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO stream_heads (stream, seq) VALUES ('retention/feed', 12000)")
                .execute(&pool)
                .await
                .unwrap();
            let cut: Timestamp = "2026-08-04T00:00:00Z".parse().unwrap();
            for expected in [5000, 5000, 2000, 0] {
                assert_eq!(
                    prune_batch(&pool, "retention", cut).await.unwrap(),
                    expected
                );
            }
            let marker: i64 = sqlx::query_scalar(
                "SELECT pruned_through FROM stream_heads WHERE stream = 'retention/feed'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(marker, 12000);
            pool.close().await;
        })
        .await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE \"{name}\" WITH (FORCE)"
        )))
        .execute(&admin)
        .await
        .unwrap();
        admin.close().await;
        if let Err(err) = outcome {
            std::panic::resume_unwind(err.into_panic());
        }
    }
}
