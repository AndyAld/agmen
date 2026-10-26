//! Who is calling: hub bearer tokens, their scopes, and the token endpoint.
//!
//! A human signs in through the RFC 8628 device flow (`device`), whose
//! approve page signs them in with GitHub (`oauth`), and gets an access
//! token (1 hour) and a refresh token (30 days, replaced on every use).
//! Minting an identity gives it an identity token that acts only as that
//! identity. Tokens are random 32-byte values; the database keeps only
//! their SHA-256.

mod device;
mod oauth;

pub use oauth::GithubError;
pub(crate) use oauth::{GithubOAuth, is_human_handle};

use axum::extract::{Form, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Postgres, Transaction};

use crate::db::{hash_secret, new_secret};
use crate::http::{ApiError, Hub};
use crate::identities::{HumanId, Identity};

/// Lifetime of a human access token, in seconds.
const ACCESS_TTL_SECS: i64 = 3600;
/// Lifetime of a human refresh token, in seconds (30 days).
const REFRESH_TTL_SECS: i64 = 30 * 24 * 3600;

/// RFC 8628 section 3.4.
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

pub(crate) fn routes() -> Router<Hub> {
    Router::new()
        .route("/v1/auth/device", post(device::start).get(device::form))
        .route("/v1/auth/device/approve", post(device::approve))
        .route("/v1/auth/token", post(token))
        .route("/v1/auth/oauth/github/start", get(oauth::start))
        .route("/v1/auth/oauth/github/callback", get(oauth::callback))
}

/// The authenticated caller of a request.
#[derive(Debug, Clone)]
pub(crate) enum Caller {
    /// A human's access token: everything that human may do.
    Human(HumanId),
    /// An identity token: that identity only.
    Identity(Identity),
}

impl Caller {
    /// The human, or 403 for an identity token.
    pub(crate) fn human(self) -> Result<HumanId, ApiError> {
        match self {
            Self::Human(human) => Ok(human),
            Self::Identity(_) => Err(ApiError::forbidden(
                "human_token_required",
                "an identity token cannot call this route",
            )),
        }
    }

    /// The identity, or 403 for a human token.
    pub(crate) fn identity(self) -> Result<Identity, ApiError> {
        match self {
            Self::Identity(identity) => Ok(identity),
            Self::Human(_) => Err(ApiError::forbidden(
                "identity_token_required",
                "this route takes the identity's own token",
            )),
        }
    }
}

impl FromRequestParts<Hub> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, hub: &Hub) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| {
                let (scheme, token) = value.split_once(' ')?;
                scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
            })
            .filter(|token| !token.is_empty())
            .ok_or_else(ApiError::unauthorized)?;
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT human_id, identity_id FROM tokens
             WHERE hash = $1 AND kind IN ('access', 'identity')
               AND (expires_at IS NULL OR expires_at > now())",
        )
        .bind(hash_secret(token))
        .fetch_optional(hub.pool())
        .await?;
        match row {
            None => Err(ApiError::unauthorized()),
            Some((human, None)) => Ok(Self::Human(HumanId::from_stored(human))),
            Some((_, Some(identity))) => Identity::load(hub.pool(), &identity)
                .await?
                .map(Self::Identity)
                .ok_or_else(ApiError::unauthorized),
        }
    }
}

/// A successful token endpoint answer (RFC 6749 section 5.1).
#[derive(Debug, Serialize)]
pub(crate) struct TokenResponse {
    access_token: String,
    token_type: &'static str,
    expires_in: i64,
    refresh_token: String,
}

impl IntoResponse for TokenResponse {
    fn into_response(self) -> Response {
        let mut response = Json(self).into_response();
        // RFC 6749 section 5.1: token responses must not be cached.
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

/// Stores a new access and refresh token for `human` and returns them. Also
/// deletes the human's expired tokens, so they do not pile up.
pub(crate) async fn issue_human_tokens(
    tx: &mut Transaction<'_, Postgres>,
    human: &str,
) -> Result<TokenResponse, ApiError> {
    sqlx::query("DELETE FROM tokens WHERE human_id = $1 AND expires_at <= now()")
        .bind(human)
        .execute(&mut **tx)
        .await?;
    let access_token = new_secret()?;
    let refresh_token = new_secret()?;
    for (secret, kind, ttl) in [
        (&access_token, "access", ACCESS_TTL_SECS),
        (&refresh_token, "refresh", REFRESH_TTL_SECS),
    ] {
        sqlx::query(
            "INSERT INTO tokens (hash, kind, human_id, expires_at)
             VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
        )
        .bind(hash_secret(secret))
        .bind(kind)
        .bind(human)
        .bind(ttl)
        .execute(&mut **tx)
        .await?;
    }
    Ok(TokenResponse {
        access_token,
        token_type: "Bearer",
        expires_in: ACCESS_TTL_SECS,
        refresh_token,
    })
}

/// Stores a new identity token for `identity`, owned by `human`.
pub(crate) async fn issue_identity_token(
    conn: &mut PgConnection,
    human: &str,
    identity: &str,
) -> Result<String, ApiError> {
    let token = new_secret()?;
    sqlx::query(
        "INSERT INTO tokens (hash, kind, human_id, identity_id) VALUES ($1, 'identity', $2, $3)",
    )
    .bind(hash_secret(&token))
    .bind(human)
    .bind(identity)
    .execute(conn)
    .await?;
    Ok(token)
}

#[derive(Debug, Deserialize)]
struct TokenForm {
    grant_type: String,
    device_code: Option<String>,
    refresh_token: Option<String>,
}

/// `POST /v1/auth/token`: the device code grant (RFC 8628) and the refresh
/// token grant (RFC 6749 section 6).
async fn token(
    State(hub): State<Hub>,
    Form(form): Form<TokenForm>,
) -> Result<TokenResponse, ApiError> {
    let missing = |field: &str| ApiError::grant("invalid_request", format!("{field} is required"));
    match form.grant_type.as_str() {
        DEVICE_CODE_GRANT => {
            let code = form.device_code.ok_or_else(|| missing("device_code"))?;
            device::redeem(&hub, &code).await
        }
        "refresh_token" => {
            let refresh = form.refresh_token.ok_or_else(|| missing("refresh_token"))?;
            refresh_tokens(&hub, &refresh).await
        }
        other => Err(ApiError::grant(
            "unsupported_grant_type",
            format!("grant_type {other:?} is not supported"),
        )),
    }
}

/// Spends a refresh token: it is deleted, and a new pair replaces it.
async fn refresh_tokens(hub: &Hub, refresh: &str) -> Result<TokenResponse, ApiError> {
    let mut tx = hub.pool().begin().await?;
    let human: Option<String> = sqlx::query_scalar(
        "DELETE FROM tokens WHERE hash = $1 AND kind = 'refresh' AND expires_at > now()
         RETURNING human_id",
    )
    .bind(hash_secret(refresh))
    .fetch_optional(&mut *tx)
    .await?;
    let human = human.ok_or_else(|| {
        ApiError::grant(
            "invalid_grant",
            "the refresh token is unknown, used or expired",
        )
    })?;
    let tokens = issue_human_tokens(&mut tx, &human).await?;
    tx.commit().await?;
    Ok(tokens)
}
