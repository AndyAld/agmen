//! Device sign-in: the hub starts and polls GitHub, then issues hub tokens.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use sqlx::{Postgres, Transaction};
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
    // Refuse before calling GitHub when the table is already full. This read is
    // only a shortcut: the check under the lock below decides.
    let open: i64 =
        sqlx::query_scalar("SELECT count(*) FROM device_authorizations WHERE expires_at > now()")
            .fetch_one(hub.pool())
            .await?;
    if open >= MAX_OPEN_DEVICE_LOGINS {
        return Err(unavailable("too many open device logins, try again later"));
    }
    // No pool connection is held while GitHub answers.
    let code = hub.github().start_device().await?;
    let expires_in = code.expires_in.min(MAX_EXPIRES_IN_SECS);
    let interval = i32::try_from(code.interval).map_err(|err| ApiError::Stored(Box::new(err)))?;
    let device_code = new_secret()?;
    let mut tx = hub.pool().begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '10s'")
        .execute(&mut *tx)
        .await?;
    // Serialize inserts so concurrent requests cannot exceed the open-login cap.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(DEVICE_START_LOCK)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM device_authorizations WHERE expires_at <= now()")
        .execute(&mut *tx)
        .await?;
    // shortcut: one global cap of 1000 outstanding codes per hub and no per-client
    // limit, so one client can fill it; add per-client limits when sign-up is public
    // or abuse shows up.
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM device_authorizations")
        .fetch_one(&mut *tx)
        .await?;
    if open >= MAX_OPEN_DEVICE_LOGINS {
        tx.commit().await?;
        return Err(unavailable("too many open device logins, try again later"));
    }
    // shortcut: GitHub's device code is stored in plaintext because the hub must send it
    // back to GitHub. Anyone who can read the database within 15 minutes of a sign-in could
    // redeem that user's GitHub token with the public client id. The hub requests no scope,
    // so the token reads identity only; encrypt the column when the hub is multi-tenant
    // hosted or a broader scope is requested.
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

/// What the first short transaction of a poll decided.
enum Claim {
    /// The test hook had approved the row: tokens are issued and the row is gone.
    Issued(TokenResponse),
    /// This poll owns the next GitHub call for the row.
    Poll { github_device_code: String },
}

fn pending() -> ApiError {
    ApiError::grant(
        "authorization_pending",
        "the login has not been approved yet",
    )
}

fn expired() -> ApiError {
    ApiError::grant("expired_token", "the device code has expired")
}

/// Deletes the row and issues tokens in one transaction. The delete is what
/// makes a device code single-use: of two polls that both saw GitHub approve,
/// only one deletes the row.
async fn issue_once(
    tx: &mut Transaction<'_, Postgres>,
    hash: &[u8],
    human: &str,
) -> Result<TokenResponse, ApiError> {
    let deleted = sqlx::query("DELETE FROM device_authorizations WHERE device_code_hash = $1")
        .bind(hash)
        .execute(&mut **tx)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::grant(
            "invalid_grant",
            "the device code is unknown or was already used",
        ));
    }
    issue_human_tokens(tx, human).await
}

async fn delete_row(hub: &Hub, hash: &[u8]) -> Result<(), ApiError> {
    sqlx::query("DELETE FROM device_authorizations WHERE device_code_hash = $1")
        .bind(hash)
        .execute(hub.pool())
        .await?;
    Ok(())
}

/// First short transaction: lock the row, check it, and move `next_poll_at`
/// forward so a concurrent poll answers pending while this one asks GitHub.
async fn claim(hub: &Hub, hash: &[u8]) -> Result<Claim, ApiError> {
    let mut tx = hub.pool().begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '10s'")
        .execute(&mut *tx)
        .await?;
    let row: Option<Authorization> = sqlx::query_as(
        "SELECT human_id, approved_at IS NOT NULL AS approved, github_device_code, interval_secs
         FROM device_authorizations WHERE device_code_hash = $1 FOR UPDATE",
    )
    .bind(hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Err(ApiError::grant(
            "invalid_grant",
            "the device code is unknown or was already used",
        ));
    };
    // Check the clock after taking the row lock, including time spent waiting for another poll.
    let (is_expired, too_early): (bool, bool) = sqlx::query_as(
        "SELECT expires_at <= clock_timestamp(), next_poll_at > clock_timestamp()
         FROM device_authorizations WHERE device_code_hash = $1",
    )
    .bind(hash)
    .fetch_one(&mut *tx)
    .await?;
    if is_expired {
        sqlx::query("DELETE FROM device_authorizations WHERE device_code_hash = $1")
            .bind(hash)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Err(expired());
    }
    if let (true, Some(human)) = (row.approved, &row.human_id) {
        let tokens = issue_once(&mut tx, hash, human).await?;
        tx.commit().await?;
        return Ok(Claim::Issued(tokens));
    }
    if too_early {
        return Err(pending());
    }
    // A failed GitHub call keeps this interval, so a broken GitHub is not retried early.
    sqlx::query(
        "UPDATE device_authorizations
         SET next_poll_at = clock_timestamp() + make_interval(secs => $2)
         WHERE device_code_hash = $1",
    )
    .bind(hash)
    .bind(f64::from(row.interval_secs))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Claim::Poll {
        github_device_code: row.github_device_code,
    })
}

/// Polls GitHub for the row's login. No pool connection is held while GitHub
/// answers; the row's `next_poll_at` keeps other polls from asking too.
pub(super) async fn redeem(hub: &Hub, device_code: &str) -> Result<TokenResponse, ApiError> {
    let hash = hash_secret(device_code);
    let github_device_code = match claim(hub, &hash).await? {
        Claim::Issued(tokens) => return Ok(tokens),
        Claim::Poll { github_device_code } => github_device_code,
    };
    match hub.github().poll_device(&github_device_code).await? {
        DevicePoll::Pending => Err(pending()),
        DevicePoll::SlowDown { interval } => {
            let interval = i32::try_from(interval).map_err(ApiError::stored)?;
            sqlx::query(
                "UPDATE device_authorizations SET interval_secs = $2,
                 next_poll_at = clock_timestamp() + make_interval(secs => $3)
                 WHERE device_code_hash = $1",
            )
            .bind(&hash)
            .bind(interval)
            .bind(f64::from(interval))
            .execute(hub.pool())
            .await?;
            Err(ApiError::grant("slow_down", "poll less often"))
        }
        DevicePoll::Expired => {
            delete_row(hub, &hash).await?;
            Err(expired())
        }
        DevicePoll::Denied => {
            delete_row(hub, &hash).await?;
            Err(ApiError::grant(
                "access_denied",
                "the sign-in was declined on GitHub",
            ))
        }
        DevicePoll::Approved(token) => {
            let account = hub.github().user(&token).await?;
            // The upsert commits on its own connection and is idempotent, so
            // it is safe to repeat if the second transaction below fails.
            let (human, _) = upsert_human(hub, &account).await?;
            let mut tx = hub.pool().begin().await?;
            let tokens = issue_once(&mut tx, &hash, &human).await?;
            tx.commit().await?;
            Ok(tokens)
        }
    }
}
