//! Device sign-in: the hub starts and polls GitHub, then issues hub tokens.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use url::Url;

use super::github::{DevicePoll, upsert_human};
use super::{TokenResponse, issue_human_tokens};
use crate::db::{hash_secret, new_secret};
use crate::http::{ApiError, Hub};

const MAX_OPEN_DEVICE_LOGINS: i64 = 1000;
const MAX_EXPIRES_IN_SECS: u32 = 900;
// The agmen namespace and device-start discriminator keep this separate from other advisory locks.
const DEVICE_START_LOCK: i64 = 0x6167_6d65_6e00_0001;

#[derive(Debug, Serialize)]
pub(super) struct DeviceStart {
    device_code: String,
    user_code: String,
    verification_uri: Url,
    expires_in: u32,
    interval: u32,
}

fn unavailable(description: &str) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "temporarily_unavailable",
        description,
    )
}

pub(super) async fn start(State(hub): State<Hub>) -> Result<Json<DeviceStart>, ApiError> {
    let mut tx = hub.pool().begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '10s'")
        .execute(&mut *tx)
        .await?;
    // Serialize starts so concurrent requests cannot exceed the open-login cap.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(DEVICE_START_LOCK)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM device_authorizations WHERE expires_at <= now()")
        .execute(&mut *tx)
        .await?;
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM device_authorizations")
        .fetch_one(&mut *tx)
        .await?;
    if open >= MAX_OPEN_DEVICE_LOGINS {
        tx.commit().await?;
        return Err(unavailable("too many open device logins, try again later"));
    }
    let code = hub.github().start_device().await?;
    let expires_in = code.expires_in.min(MAX_EXPIRES_IN_SECS);
    let interval = i32::try_from(code.interval).map_err(|err| ApiError::Stored(Box::new(err)))?;
    let device_code = new_secret()?;
    let inserted = sqlx::query(
        "INSERT INTO device_authorizations
         (device_code_hash, user_code, github_device_code, interval_secs, next_poll_at, expires_at)
         VALUES ($1, $2, $3, $4, now(), now() + make_interval(secs => $5))
         ON CONFLICT (user_code) DO NOTHING",
    )
    .bind(hash_secret(&device_code))
    .bind(&code.user_code)
    .bind(&code.device_code)
    .bind(interval)
    .bind(f64::from(expires_in))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    if inserted == 0 {
        return Err(unavailable("the user code is already in use, try again"));
    }
    Ok(Json(DeviceStart {
        device_code,
        user_code: code.user_code,
        verification_uri: code.verification_uri,
        expires_in,
        interval: code.interval,
    }))
}

/// The hook approves the GitHub user code in any case, without polling GitHub.
#[cfg(feature = "test-hooks")]
pub(crate) async fn approve_as(hub: &Hub, user_code: &str, human: &str) -> Result<bool, ApiError> {
    let approved = sqlx::query(
        "UPDATE device_authorizations SET human_id = $1, approved_at = now()
         WHERE upper(user_code) = upper($2) AND expires_at > now() AND approved_at IS NULL",
    )
    .bind(human)
    .bind(user_code)
    .execute(hub.pool())
    .await?
    .rows_affected();
    Ok(approved == 1)
}

#[derive(sqlx::FromRow)]
struct Authorization {
    human_id: Option<String>,
    approved: bool,
    github_device_code: String,
    interval_secs: i32,
}

#[expect(
    clippy::too_many_lines,
    reason = "keeps the row lock and every transaction outcome together"
)]
pub(super) async fn redeem(hub: &Hub, device_code: &str) -> Result<TokenResponse, ApiError> {
    let hash = hash_secret(device_code);
    let mut tx = hub.pool().begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '10s'")
        .execute(&mut *tx)
        .await?;
    let row: Option<Authorization> = sqlx::query_as(
        "SELECT human_id, approved_at IS NOT NULL AS approved, github_device_code, interval_secs
         FROM device_authorizations WHERE device_code_hash = $1 FOR UPDATE",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Err(ApiError::grant(
            "invalid_grant",
            "the device code is unknown or was already used",
        ));
    };
    // Check the clock after taking the row lock, including time spent waiting for another poll.
    let (expired, too_early): (bool, bool) = sqlx::query_as(
        "SELECT expires_at <= clock_timestamp(), next_poll_at > clock_timestamp()
         FROM device_authorizations WHERE device_code_hash = $1",
    )
    .bind(&hash)
    .fetch_one(&mut *tx)
    .await?;
    let outcome = if expired {
        Err(ApiError::grant(
            "expired_token",
            "the device code has expired",
        ))
    } else if let (true, Some(human)) = (row.approved, row.human_id) {
        Ok(human)
    } else {
        if too_early {
            return Err(ApiError::grant(
                "authorization_pending",
                "the login has not been approved yet",
            ));
        }
        let poll = hub.github().poll_device(&row.github_device_code).await;
        let interval = match &poll {
            Ok(DevicePoll::SlowDown { interval }) => {
                i32::try_from(*interval).map_err(|err| ApiError::Stored(Box::new(err)))?
            }
            _ => row.interval_secs,
        };
        sqlx::query(
            "UPDATE device_authorizations SET interval_secs = $2,
             next_poll_at = clock_timestamp() + make_interval(secs => $3)
             WHERE device_code_hash = $1",
        )
        .bind(&hash)
        .bind(interval)
        .bind(f64::from(interval))
        .execute(&mut *tx)
        .await?;
        let poll = match poll {
            Ok(poll) => poll,
            Err(error) => {
                tx.commit().await?;
                return Err(error.into());
            }
        };
        match poll {
            DevicePoll::Pending | DevicePoll::SlowDown { .. } => {
                tx.commit().await?;
                return Err(match poll {
                    DevicePoll::SlowDown { .. } => ApiError::grant("slow_down", "poll less often"),
                    _ => ApiError::grant(
                        "authorization_pending",
                        "the login has not been approved yet",
                    ),
                });
            }
            DevicePoll::Expired => Err(ApiError::grant(
                "expired_token",
                "the device code has expired",
            )),
            DevicePoll::Denied => Err(ApiError::grant(
                "access_denied",
                "the sign-in was declined on GitHub",
            )),
            DevicePoll::Approved(token) => {
                let account = match hub.github().user(&token).await {
                    Ok(account) => account,
                    Err(error) => {
                        tx.commit().await?;
                        return Err(error.into());
                    }
                };
                // The upsert commits separately on the pool and is idempotent.
                match upsert_human(hub, &account).await {
                    Ok((human, _)) => Ok(human),
                    Err(error) => {
                        tx.commit().await?;
                        return Err(error);
                    }
                }
            }
        }
    };
    sqlx::query("DELETE FROM device_authorizations WHERE device_code_hash = $1")
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    let tokens = match outcome {
        Ok(human) => Ok(issue_human_tokens(&mut tx, &human).await?),
        Err(err) => Err(err),
    };
    tx.commit().await?;
    tokens
}
