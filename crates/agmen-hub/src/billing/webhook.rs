//! Signed Paddle events for hosted tenant plans.

use agmen_proto::{TenantId, Timestamp};
use aws_lc_rs::hmac::{self, HMAC_SHA256};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::Value;

use super::{BillingEvent, Catalog, PlanState};
use crate::config::{HubMode, Secret};
use crate::groups::{load_plan, owner_notice, store_plan};
use crate::http::{ApiError, Hub, error_chain};
use crate::usage::post_owner_alert;

pub(crate) const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_SIGNATURE_HEADER_BYTES: usize = 1_024;
const MAX_H1_VALUES: usize = 8;
const MAX_SIGNATURE_FIELDS: usize = MAX_H1_VALUES + 1;
const HMAC_TAG_BYTES: usize = 32;
const REPLAY_WINDOW_NANOS: i128 = 300 * 1_000_000_000;
// Even an array of one-byte JSON values cannot exceed the request byte limit.
const MAX_EVENT_ITEMS: usize = MAX_BODY_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SignatureError {
    #[error("malformed Paddle signature")]
    Malformed,
    #[error("stale Paddle signature")]
    Stale,
    #[error("Paddle signature mismatch")]
    Mismatch,
}

pub(crate) fn verify_signature(
    header: &str,
    body: &[u8],
    secret: &Secret,
    now: Timestamp,
) -> Result<(), SignatureError> {
    if header.len() > MAX_SIGNATURE_HEADER_BYTES || body.len() > MAX_BODY_BYTES {
        return Err(SignatureError::Malformed);
    }
    let mut timestamp = None;
    let mut tags = [[0; HMAC_TAG_BYTES]; MAX_H1_VALUES];
    let mut count = 0;
    // One extra field lets us reject, rather than ignore, an oversized list.
    for (index, field) in header.split(';').take(MAX_SIGNATURE_FIELDS + 1).enumerate() {
        if index == MAX_SIGNATURE_FIELDS {
            return Err(SignatureError::Malformed);
        }
        let (name, value) = field.split_once('=').ok_or(SignatureError::Malformed)?;
        match name {
            "ts" if timestamp.is_none() => timestamp = Some(value),
            "h1" => {
                let tag = tags.get_mut(count).ok_or(SignatureError::Malformed)?;
                *tag = decode_tag(value)?;
                count += 1;
            }
            _ => return Err(SignatureError::Malformed),
        }
    }
    let timestamp = timestamp.ok_or(SignatureError::Malformed)?;
    // Require decimal seconds and keep the original bytes for HMAC.
    if timestamp.is_empty() || !timestamp.bytes().all(|byte| byte.is_ascii_digit()) || count == 0 {
        return Err(SignatureError::Malformed);
    }
    let ts: i64 = timestamp.parse().map_err(|_| SignatureError::Malformed)?;
    let at = Timestamp::from_second(ts).map_err(|_| SignatureError::Malformed)?;
    if (now.as_nanosecond() - at.as_nanosecond()).abs() > REPLAY_WINDOW_NANOS {
        return Err(SignatureError::Stale);
    }
    // The header and body bounds above also bound this signed-message buffer.
    let mut message = format!("{timestamp}:").into_bytes();
    message.extend_from_slice(body);
    // An empty secret is a key anyone can sign with. Config loading refuses
    // one, but `PaddleConfig` is public, so refuse it here as well.
    if secret.expose().is_empty() {
        return Err(SignatureError::Mismatch);
    }
    let key = hmac::Key::new(HMAC_SHA256, secret.expose().as_bytes());
    if tags
        .iter()
        .take(count)
        .any(|tag| hmac::verify(&key, &message, tag).is_ok())
    {
        Ok(())
    } else {
        Err(SignatureError::Mismatch)
    }
}

fn decode_tag(value: &str) -> Result<[u8; HMAC_TAG_BYTES], SignatureError> {
    if value.len() != HMAC_TAG_BYTES * 2 {
        return Err(SignatureError::Malformed);
    }
    let mut tag = [0; HMAC_TAG_BYTES];
    let mut digits = value.bytes();
    for byte in &mut tag {
        let high = digits
            .next()
            .and_then(hex_digit)
            .ok_or(SignatureError::Malformed)?;
        let low = digits
            .next()
            .and_then(hex_digit)
            .ok_or(SignatureError::Malformed)?;
        *byte = high * 16 + low;
    }
    Ok(tag)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct PaddleEvent {
    pub(crate) event_id: String,
    pub(crate) event_type: String,
    pub(crate) occurred_at: Timestamp,
    pub(crate) data: Value,
}

#[derive(Debug)]
pub(crate) struct PaddleIds {
    pub(crate) customer_id: String,
    pub(crate) subscription_id: String,
}

#[derive(Debug)]
pub(crate) struct Billing {
    pub(crate) tenant: Option<TenantId>,
    pub(crate) event: BillingEvent,
    pub(crate) ids: PaddleIds,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WebhookError {
    #[error("invalid Paddle event body")]
    Body(#[source] serde_json::Error),
    #[error("Paddle event has no tenant or subscription id")]
    NoTenant,
    #[error("invalid Paddle tenant {0:?}")]
    BadTenant(String),
    #[error("unknown Paddle price {0:?}")]
    UnknownPrice(String),
    #[error("invalid Paddle event field {0}")]
    Shape(&'static str),
}

pub(crate) fn billing_event(
    event: &PaddleEvent,
    catalog: &Catalog,
) -> Result<Option<Billing>, WebhookError> {
    let data = &event.data;
    let (event, subscription_field) = if event.event_type.starts_with("subscription.") {
        let event = match data.get("status").and_then(Value::as_str) {
            Some("active") => BillingEvent::Active(subscription_plan(data, catalog)?),
            Some("past_due") => BillingEvent::PastDue,
            Some("canceled") => BillingEvent::Canceled,
            Some(status @ ("trialing" | "paused")) => {
                tracing::warn!(event_id = %event.event_id, status, "ignoring Paddle subscription status");
                return Ok(None);
            }
            _ => return Err(WebhookError::Shape("status")),
        };
        (event, "id")
    } else if event.event_type == "transaction.completed" {
        if data.get("subscription_id").is_none_or(Value::is_null) {
            return Ok(None);
        }
        (BillingEvent::Paid, "subscription_id")
    } else {
        return Ok(None);
    };
    let tenant = match data.pointer("/custom_data/agmen_tenant") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(
            value
                .parse()
                .map_err(|_| WebhookError::BadTenant(value.clone()))?,
        ),
        Some(_) => return Err(WebhookError::Shape("custom_data.agmen_tenant")),
    };
    let subscription_id = data
        .get(subscription_field)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    if tenant.is_none() && subscription_id.is_none() {
        return Err(WebhookError::NoTenant);
    }
    let subscription_id = subscription_id
        .ok_or(WebhookError::Shape(subscription_field))?
        .to_owned();
    let customer_id = data
        .get("customer_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or(WebhookError::Shape("customer_id"))?
        .to_owned();
    Ok(Some(Billing {
        tenant,
        event,
        ids: PaddleIds {
            customer_id,
            subscription_id,
        },
    }))
}

fn subscription_plan(data: &Value, catalog: &Catalog) -> Result<super::Plan, WebhookError> {
    let count = data
        .get("items")
        .and_then(Value::as_array)
        .ok_or(WebhookError::Shape("items"))?
        .len();
    if count > MAX_EVENT_ITEMS {
        return Err(WebhookError::Shape("items"));
    }
    let items = super::paddle::entity_items(data).map_err(|_| WebhookError::Shape("items"))?;
    let known = [
        &catalog.team_base,
        &catalog.team_seat,
        &catalog.business_10,
        &catalog.business_25,
        &catalog.business_50,
    ];
    if let Some(item) = items.iter().find(|item| !known.contains(&&item.price_id)) {
        return Err(WebhookError::UnknownPrice(item.price_id.clone()));
    }
    let plan = catalog
        .plan_of(&items)
        .ok_or(WebhookError::Shape("items"))?;
    // tenant_plans stores extra seats as a signed Postgres integer.
    if let super::Plan::Team { extra_seats } = plan {
        i32::try_from(extra_seats).map_err(|_| WebhookError::Shape("items"))?;
    }
    Ok(plan)
}

pub(crate) async fn receive(
    State(hub): State<Hub>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let Some(hosted) = hub.hosted().filter(|_| hub.mode() == HubMode::Hosted) else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_hosted",
            "this hub is not agmen hosted",
        ));
    };
    let mut signatures = headers.get_all("Paddle-Signature").iter();
    let valid = signatures
        .next()
        .and_then(|header| header.to_str().ok())
        .is_some_and(|header| {
            verify_signature(
                header,
                &body,
                &hosted.paddle.webhook_secret,
                Timestamp::now(),
            )
            .is_ok()
        });
    if !valid || signatures.next().is_some() {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "bad_signature",
            "a valid Paddle signature within 300 seconds is required",
        ));
    }
    let event: PaddleEvent = match serde_json::from_slice(&body).map_err(WebhookError::Body) {
        Ok(event) => event,
        Err(error) => {
            tracing::error!(event_id = "unparsed", error = %error_chain(&error), "unusable Paddle event");
            return Ok(StatusCode::OK);
        }
    };
    match billing_event(&event, &hosted.catalog) {
        Ok(Some(billing)) => apply_billing(&hub, &event, billing).await?,
        Ok(None) => {}
        Err(error) => {
            tracing::error!(event_id = %event.event_id, error = %error_chain(&error), "unusable Paddle event")
        }
    }
    Ok(StatusCode::OK)
}

async fn apply_billing(hub: &Hub, event: &PaddleEvent, billing: Billing) -> Result<(), ApiError> {
    let mut tx = hub.pool().begin().await?;
    // shortcut: never pruned; prune rows older than the 300 s replay window plus Paddle's retry horizon if the table grows
    let inserted: Option<String> = sqlx::query_scalar(
        "INSERT INTO paddle_events (event_id) VALUES ($1)
         ON CONFLICT (event_id) DO NOTHING RETURNING event_id",
    )
    .bind(&event.event_id)
    .fetch_optional(&mut *tx)
    .await?;
    if inserted.is_none() {
        tx.commit().await?;
        return Ok(());
    }
    let tenant = match billing.tenant {
        Some(tenant) => tenant,
        None => {
            let found: Option<String> = sqlx::query_scalar(
                "SELECT tenant_id FROM tenant_plans WHERE paddle_subscription_id = $1",
            )
            .bind(&billing.ids.subscription_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(tenant) = found else {
                tracing::error!(event_id = %event.event_id, "Paddle event has no stored subscription tenant");
                tx.rollback().await?;
                return Ok(());
            };
            tenant.parse().map_err(ApiError::stored)?
        }
    };
    let locked: Option<String> =
        sqlx::query_scalar("SELECT tenant_id FROM tenant_plans WHERE tenant_id = $1 FOR UPDATE")
            .bind(tenant.as_str())
            .fetch_optional(&mut *tx)
            .await?;
    if locked.is_none() {
        tracing::error!(event_id = %event.event_id, %tenant, "Paddle event tenant has no plan to lock");
        tx.rollback().await?;
        return Ok(());
    }
    let Some(before) = load_plan(&mut *tx, &tenant).await? else {
        tracing::error!(event_id = %event.event_id, %tenant, "Paddle event tenant has no plan");
        tx.rollback().await?;
        return Ok(());
    };
    // Do not tick here: storing a tick would discard reminders and deletion
    // effects that only the plan clock can post or carry out.
    let after = before.apply(billing.event, event.occurred_at);
    store_plan(&mut tx, &tenant, &after).await?;
    if before
        .last_event_at
        .is_none_or(|at| at <= event.occurred_at)
    {
        let updated = sqlx::query(
            "UPDATE tenant_plans SET paddle_customer_id = $2, paddle_subscription_id = $3
             WHERE tenant_id = $1",
        )
        .bind(tenant.as_str())
        .bind(&billing.ids.customer_id)
        .bind(&billing.ids.subscription_id)
        .execute(&mut *tx)
        .await;
        if let Err(error) = updated {
            if !crate::db::is_unique_violation(&error) {
                return Err(error.into());
            }
            tracing::error!(event_id = %event.event_id, %tenant, %error, "Paddle subscription already belongs to another tenant");
            tx.rollback().await?;
            return Ok(());
        }
    }
    let notice = if before.state != PlanState::PastDue && after.state == PlanState::PastDue {
        let name: String = sqlx::query_scalar("SELECT name FROM tenants WHERE id = $1")
            .bind(tenant.as_str())
            .fetch_one(&mut *tx)
            .await?;
        owner_notice(&name, &after, Timestamp::now())
    } else {
        None
    };
    tx.commit().await?;
    if let Some(text) = notice
        && let Err(error) = post_owner_alert(hub, &tenant, &text).await
    {
        tracing::warn!(event_id = %event.event_id, %tenant, error = %error_chain(&error), "past-due owner alert was not posted");
    }
    Ok(())
}

#[cfg(test)]
#[path = "webhook_tests.rs"]
mod tests;
