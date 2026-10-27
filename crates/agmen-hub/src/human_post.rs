//! `POST /v1/projects/{p}/messages`: a signed-in member posts to the
//! project's chat as themselves (protocol section 6). A human has no broker
//! identity, so the web console and agmend post here instead of on `up/`.
//!
//! The body is a client envelope. The route runs [`Hub::ingest`], the same
//! path as an `up/` publish (protocol 5.2), with the token's human as the
//! sender, and answers with the ACK or NACK body. The status says which:
//!
//! | Answer | Status |
//! |---|---|
//! | ACK | 200 |
//! | NACK `rate_limited` | 429, with `Retry-After` in whole seconds |
//! | NACK `hub_busy` | 503 |
//! | NACK `conflict` | 409 |
//! | any other NACK (final) | 422 |
//!
//! A body with no readable `id` cannot be named in a NACK and gets 400.
//! Only an access token may call it; an identity token gets 403, because
//! identities post over MQTT.

use std::time::Duration;

use agmen_proto::{DeliveryReply, IdentityId, NackReason, Topics};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::auth::Caller;
use crate::http::{ApiError, Hub};
use crate::ingest::IngestError;
use crate::lanes::member_of;

/// `POST /v1/projects/{p}/messages` (a member's access token).
pub(crate) async fn post(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
    body: Bytes,
) -> Result<Response, ApiError> {
    let human = caller.human()?;
    let (project, _) = member_of(&hub, Caller::Human(human.clone()), &project).await?;
    let tenant: String = sqlx::query_scalar("SELECT tenant_id FROM projects WHERE id = $1")
        .bind(project.as_str())
        .fetch_optional(hub.pool())
        .await?
        .ok_or_else(|| ApiError::not_found("no such project"))?;
    let topics = Topics::new(
        tenant.parse().map_err(ApiError::stored)?,
        project.as_str().parse().map_err(ApiError::stored)?,
    );
    let from: IdentityId = human.as_str().parse().map_err(ApiError::stored)?;
    let reply = hub
        .ingest(&topics, &from, &body)
        .await
        .map_err(|err| match err {
            IngestError::NoMessageId => ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "the body has no readable message id",
            ),
            IngestError::UnknownProject => ApiError::not_found("no such project"),
            IngestError::Database(err) => ApiError::Database(err),
        })?;
    Ok(respond(&reply))
}

/// The response for `reply`: its status and the reply as the JSON body.
fn respond(reply: &DeliveryReply) -> Response {
    let (status, retry_after) = match reply {
        DeliveryReply::Nack {
            reason,
            retry_after_ms,
            ..
        } => (nack_status(*reason), *retry_after_ms),
        _ => (StatusCode::OK, None),
    };
    let mut response = (status, Json(reply)).into_response();
    if status == StatusCode::TOO_MANY_REQUESTS
        && let Some(ms) = retry_after
    {
        let secs = Duration::from_millis(ms).as_secs() + u64::from(ms % 1000 != 0);
        if let Ok(value) = HeaderValue::from_str(&secs.max(1).to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

/// The status for a NACK: the retryable reasons say so, a reused id is a
/// conflict, and every other reason is final.
const fn nack_status(reason: NackReason) -> StatusCode {
    match reason {
        NackReason::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        NackReason::HubBusy => StatusCode::SERVICE_UNAVAILABLE,
        NackReason::Conflict => StatusCode::CONFLICT,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    }
}
