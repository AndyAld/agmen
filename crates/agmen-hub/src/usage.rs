//! Hosted plan write gates, UTC daily usage, and owner alerts.

use agmen_proto::{Body, IdentityId, Needs, Payload, Stream, TenantId, Timestamp, Topics, Urgency};
use sqlx::{Postgres, Transaction};

use crate::groups::load_plan;
use crate::http::{ApiError, Hub};
use crate::{HubMode, HubPost, PostError};

/// Whether a tenant may write, with its hosted daily message limit when set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Operator-made tenants and self-host hubs have no quota or counting.
    Open,
    /// Writable hosted tenants count recorded messages against this limit.
    Quota { limit: u32 },
    /// Only RELEASE and BYE may be ingested until the owner pays.
    ReadOnly,
}

/// Reads and ticks a hosted plan without persisting the tick's effects.
pub(crate) async fn write_gate(hub: &Hub, tenant: &TenantId) -> Result<Gate, sqlx::Error> {
    if hub.mode() == HubMode::SelfHost {
        return Ok(Gate::Open);
    }
    // shortcut: one plan read per message, cache per tenant for a few seconds if ingest latency shows it
    let Some(record) = load_plan(hub.pool(), tenant).await? else {
        return Ok(Gate::Open);
    };
    let (record, _) = record.tick(Timestamp::now());
    let limit = record.daily_quota();
    if record.writable() && limit > 0 {
        Ok(Gate::Quota { limit })
    } else {
        Ok(Gate::ReadOnly)
    }
}

/// The result of taking one message from the tenant's daily quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Taken {
    /// This transaction counted a message and may own today's owner warning.
    Counted { messages: u32, warn: bool },
    /// No message remains today; the counter was not changed.
    Exhausted,
}

/// Takes the usage row before stream heads, counting only if `tx` commits.
pub(crate) async fn take_quota(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &TenantId,
    limit: u32,
) -> Result<Taken, sqlx::Error> {
    if limit == 0 {
        return Ok(Taken::Exhausted);
    }
    let messages: Option<i32> = sqlx::query_scalar(
        "INSERT INTO tenant_usage (tenant_id, day, messages)
         VALUES ($1, (now() AT TIME ZONE 'UTC')::date, 1)
         ON CONFLICT (tenant_id, day) DO UPDATE
         SET messages = tenant_usage.messages + 1
         WHERE tenant_usage.messages < $2
         RETURNING messages",
    )
    .bind(tenant.as_str())
    .bind(i64::from(limit))
    .fetch_optional(&mut **tx)
    .await?;
    let Some(messages) = messages else {
        return Ok(Taken::Exhausted);
    };
    let messages = u32::try_from(messages).map_err(|err| sqlx::Error::Decode(Box::new(err)))?;
    // The upsert holds this row until commit, including through this update.
    // Bigint arithmetic keeps the threshold safe for every u32 limit.
    let warn: Option<bool> = sqlx::query_scalar(
        "UPDATE tenant_usage SET warned = true
         WHERE tenant_id = $1 AND day = (now() AT TIME ZONE 'UTC')::date
         AND NOT warned AND messages::bigint * 5 >= $2::bigint * 4
         RETURNING warned",
    )
    .bind(tenant.as_str())
    .bind(i64::from(limit))
    .fetch_optional(&mut **tx)
    .await?;
    Ok(Taken::Counted {
        messages,
        warn: warn.is_some(),
    })
}

/// Posts an ALERT mentioning the owner to at most 50 of the tenant's projects.
pub(crate) async fn post_owner_alert(
    hub: &Hub,
    tenant: &TenantId,
    body: &str,
) -> Result<(), PostError> {
    const PROJECT_LIMIT: i64 = 50;
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT human_id FROM tenant_members WHERE tenant_id = $1 AND role = 'owner' LIMIT 1",
    )
    .bind(tenant.as_str())
    .fetch_optional(hub.pool())
    .await
    .map_err(|err| PostError::Store(Box::new(err)))?;
    let mentioned: Vec<IdentityId> = match owner {
        Some(owner) => vec![
            owner
                .parse()
                .map_err(|err| PostError::Store(Box::new(err)))?,
        ],
        None => {
            tracing::warn!(%tenant, "posting an owner alert without an owner mention");
            Vec::new()
        }
    };
    let projects: Vec<(String, i64)> = sqlx::query_as(
        "SELECT id, count(*) OVER () FROM projects WHERE tenant_id = $1 ORDER BY id LIMIT $2",
    )
    .bind(tenant.as_str())
    .bind(PROJECT_LIMIT)
    .fetch_all(hub.pool())
    .await
    .map_err(|err| PostError::Store(Box::new(err)))?;
    if let Some((_, total)) = projects.first()
        && *total > PROJECT_LIMIT
    {
        tracing::warn!(%tenant, skipped = *total - PROJECT_LIMIT, "owner alert project limit reached");
    }
    let body = Body::try_from(body.to_owned()).map_err(|err| PostError::Store(Box::new(err)))?;
    let need = body.as_str().lines().next().unwrap_or_default().to_owned();
    for (project, _) in projects {
        let topics = Topics::new(
            tenant.clone(),
            project
                .parse()
                .map_err(|err| PostError::Store(Box::new(err)))?,
        );
        hub.post_as_hub(
            &topics,
            HubPost {
                payload: Payload::Alert {
                    need: need.clone(),
                    urgency: Urgency::Normal,
                    needs: Needs::Anyone,
                    actions: Vec::new(),
                },
                body: body.clone(),
                reply_to: None,
                items: Vec::new(),
                lane: None,
                mentioned: mentioned.clone(),
                streams: vec![Stream::Feed],
            },
        )
        .await?;
    }
    Ok(())
}

/// The HTTP refusal for a hosted group that cannot write until its owner pays.
pub(crate) fn refuse_read_only() -> ApiError {
    ApiError::forbidden(
        "group_read_only",
        "the group is read-only until its owner pays: agmen group billing",
    )
}

/// Formats a quota with decimal thousands separators.
pub(crate) fn with_thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(13);
    // A u32 has at most ten decimal digits.
    for (index, digit) in digits.chars().take(10).enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}
