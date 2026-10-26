//! The sweeper (protocol 5.2 step 5): publishes `publish_outbox` rows in id
//! order, waits for the broker to acknowledge them, publishes each touched
//! project's retained `state/head`, then deletes the rows.
//!
//! A crash anywhere before the delete republishes the rows on the next run,
//! so readers can see a message twice; they drop the repeat by
//! `(stream, seq)` (protocol 5.3).

use std::collections::BTreeSet;
use std::time::Duration;

use agmen_proto::{Stream, Timestamp, Topics};
use rumqttc_next::{AsyncClient, PublishOptions};
use serde::Serialize;

use crate::http::Hub;
use crate::ingest::head_key;
use crate::mqtt::MessagingError;

/// The most rows one pass publishes.
const BATCH: i64 = 100;
/// How long the sweeper sleeps with nothing to do before it looks again,
/// in case a row came from a process that could not wake it.
const IDLE_POLL: Duration = Duration::from_secs(1);
/// How long a publish may wait for its PUBACK.
const PUBLISH_WAIT: Duration = Duration::from_secs(10);
/// Failed passes in a row before the sweeper gives up and the hub exits.
const MAX_FAILURES: u32 = 60;
/// The pause after a failed pass.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Why one pass failed.
#[derive(Debug, thiserror::Error)]
enum SweepError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("queueing a publish failed")]
    Queue(#[from] rumqttc_next::ClientError),
    #[error("the broker refused a publish")]
    Publish(#[from] rumqttc_next::PublishNoticeError),
    #[error("no PUBACK within {PUBLISH_WAIT:?}")]
    Timeout,
    #[error("encoding state/head failed")]
    Encode(#[from] serde_json::Error),
    #[error("an outbox row names an invalid project: {0}")]
    Stored(String),
}

/// The retained `state/head` (protocol section 2). Nothing versions rules,
/// lanes or items yet, so their versions stay 0 until the tasks that add
/// them bump them.
#[derive(Debug, Serialize)]
struct StateHead {
    feed_seq: u64,
    rules_v: u64,
    lanes_v: u64,
    items_v: u64,
    hub_at: Timestamp,
}

/// Runs passes until [`MAX_FAILURES`] fail in a row.
///
/// shortcut: one sweeper per database. Two hub replicas would each publish
/// the same rows, out of order with each other; take a Postgres advisory
/// lock here before running more than one.
pub(crate) async fn run(hub: Hub, client: AsyncClient) -> Result<(), MessagingError> {
    let mut failures = 0_u32;
    loop {
        match sweep(&hub, &client).await {
            Ok(published) => {
                failures = 0;
                if published < usize::try_from(BATCH).unwrap_or(usize::MAX) {
                    tokio::select! {
                        () = hub.ingest_state().outbox_ready().notified() => {}
                        () = tokio::time::sleep(IDLE_POLL) => {}
                    }
                }
            }
            Err(err) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    return Err(MessagingError::Sweeper(Box::new(err)));
                }
                tracing::warn!(error = %err, failures, "sweep failed; retrying");
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
}

type OutboxRow = (i64, String, String, String, Vec<u8>);

/// One pass: returns how many rows it published.
async fn sweep(hub: &Hub, client: &AsyncClient) -> Result<usize, SweepError> {
    let rows: Vec<OutboxRow> = sqlx::query_as(
        "SELECT id, tenant_id, project_id, topic, payload FROM publish_outbox
         ORDER BY id LIMIT $1",
    )
    .bind(BATCH)
    .fetch_all(hub.pool())
    .await?;
    if rows.is_empty() {
        return Ok(0);
    }
    // Queue the whole batch before waiting: one connection keeps QoS 1
    // publishes in order, and waiting on each in turn would cost a round
    // trip per row.
    let mut notices = Vec::with_capacity(rows.len());
    let mut projects = BTreeSet::new();
    for (_, tenant, project, topic, payload) in &rows {
        notices.push(
            client
                .publish_tracked(
                    topic.clone(),
                    payload.clone(),
                    PublishOptions::at_least_once(),
                )
                .await?,
        );
        projects.insert((tenant.as_str(), project.as_str()));
    }
    for notice in notices {
        tokio::time::timeout(PUBLISH_WAIT, notice.wait_completion_async())
            .await
            .map_err(|_| SweepError::Timeout)??;
    }
    for (tenant, project) in projects {
        let topics = Topics::new(
            tenant
                .parse()
                .map_err(|err| SweepError::Stored(format!("{err}")))?,
            project
                .parse()
                .map_err(|err| SweepError::Stored(format!("{err}")))?,
        );
        publish_head(hub, client, &topics).await?;
    }
    let ids: Vec<i64> = rows.iter().map(|(id, ..)| *id).collect();
    sqlx::query("DELETE FROM publish_outbox WHERE id = ANY($1)")
        .bind(&ids)
        .execute(hub.pool())
        .await?;
    tracing::debug!(rows = ids.len(), "swept");
    Ok(ids.len())
}

/// Publishes `topics`' retained `state/head` with the feed's current seq.
async fn publish_head(hub: &Hub, client: &AsyncClient, topics: &Topics) -> Result<(), SweepError> {
    let feed_seq: Option<i64> =
        sqlx::query_scalar("SELECT seq FROM stream_heads WHERE stream = $1")
            .bind(head_key(topics, &Stream::Feed))
            .fetch_optional(hub.pool())
            .await?;
    let head = StateHead {
        feed_seq: feed_seq.map_or(0, |seq| u64::try_from(seq).unwrap_or(0)),
        rules_v: 0,
        lanes_v: 0,
        items_v: 0,
        hub_at: Timestamp::now(),
    };
    let notice = client
        .publish_tracked(
            topics.state_head(),
            serde_json::to_vec(&head)?,
            PublishOptions::at_least_once().retained(),
        )
        .await?;
    tokio::time::timeout(PUBLISH_WAIT, notice.wait_completion_async())
        .await
        .map_err(|_| SweepError::Timeout)??;
    Ok(())
}
