//! GitHub device sign-in and account lookup. Humans are keyed by numeric id.

use std::fmt;
use std::num::{NonZeroU64, ParseIntError};
use std::str::FromStr;

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::GithubConfig;
use crate::db::{is_unique_violation, new_id};
use crate::http::{ApiError, Hub, public_link};

/// Why a GitHub sign-in failed on GitHub's side.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GithubError {
    /// A configured GitHub URL does not make a valid endpoint.
    #[error("GitHub endpoint URL {0} is invalid")]
    BadUrl(String),
    /// A GitHub device request failed.
    #[error("requesting GitHub device sign-in failed")]
    Request(#[source] reqwest::Error),
    /// Reading a GitHub account failed.
    #[error("reading the GitHub account failed")]
    User(#[source] reqwest::Error),
    /// GitHub refused the request with this error code.
    #[error("GitHub refused device sign-in: {code}")]
    Refused {
        /// The error code returned by GitHub.
        code: String,
    },
}

/// A GitHub account's numeric id. Unlike the login, it never changes and
/// never passes to another account, so the hub binds people to it.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), std::num::ParseIntError> {
/// let id: agmen_hub::GithubId = "583231".parse()?;
/// assert_eq!(id.get(), 583_231);
/// assert!("0".parse::<agmen_hub::GithubId>().is_err());
/// # Ok(()) }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GithubId(NonZeroU64);

impl GithubId {
    /// The id as a number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl FromStr for GithubId {
    type Err = ParseIntError;

    /// A decimal id; zero is refused.
    fn from_str(text: &str) -> Result<Self, ParseIntError> {
        text.parse().map(Self)
    }
}

impl fmt::Display for GithubId {
    /// Decimal, the form `humans.provider_sub` stores.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A GitHub account as `GET /user` and `GET /users/{login}` describe it.
#[derive(Debug, Deserialize)]
pub(crate) struct GithubUser {
    pub(crate) id: GithubId,
    pub(crate) login: String,
}

/// A device authorization returned by GitHub.
#[derive(Debug, Deserialize)]
pub(crate) struct DeviceCode {
    pub(crate) device_code: String,
    pub(crate) user_code: String,
    pub(crate) verification_uri: Url,
    pub(crate) expires_in: u32,
    pub(crate) interval: u32,
}

#[derive(Debug)]
pub(crate) enum DevicePoll {
    Pending,
    SlowDown { interval: u32 },
    Expired,
    Denied,
    Approved(GithubToken),
}

pub(crate) struct GithubToken(String);

impl fmt::Debug for GithubToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GithubToken(<redacted>)")
    }
}

#[derive(Debug)]
pub(crate) struct Github {
    client_id: String,
    http: reqwest::Client,
    device_url: Url,
    token_url: Url,
    user_url: Url,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DeviceAnswer {
    Error { error: String },
    Code(DeviceCode),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PollAnswer {
    Error {
        error: String,
        interval: Option<u32>,
    },
    Token {
        access_token: String,
    },
}

const MAX_GITHUB_RESPONSE_BYTES: usize = 64 * 1024;

// Bound upstream bodies before parsing them, including chunked responses without Content-Length.
async fn read_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
    error: fn(reqwest::Error) -> GithubError,
) -> Result<T, GithubError> {
    let mut body = Vec::new();
    for _ in 0..=MAX_GITHUB_RESPONSE_BYTES {
        let Some(chunk) = response.chunk().await.map_err(error)? else {
            let buffered = reqwest::Response::from(axum::http::Response::new(body));
            return buffered.json().await.map_err(error);
        };
        if chunk.len() > MAX_GITHUB_RESPONSE_BYTES - body.len() {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Err(GithubError::Refused {
        code: "response_too_large".into(),
    })
}

impl Github {
    pub(crate) fn new(config: &GithubConfig, http: reqwest::Client) -> Result<Self, GithubError> {
        for base in [&config.url, &config.api_url] {
            if base.cannot_be_a_base() || !matches!(base.scheme(), "http" | "https") {
                return Err(GithubError::BadUrl(base.to_string()));
            }
        }
        Ok(Self {
            client_id: config.client_id.clone(),
            http,
            device_url: public_link(&config.url, "login/device/code"),
            token_url: public_link(&config.url, "login/oauth/access_token"),
            user_url: public_link(&config.api_url, "user"),
        })
    }

    pub(crate) async fn start_device(&self) -> Result<DeviceCode, GithubError> {
        let response = self
            .http
            .post(self.device_url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[("client_id", self.client_id.as_str())])
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(GithubError::Request)?;
        let answer = read_json(response, GithubError::Request).await?;
        match answer {
            DeviceAnswer::Code(code) if (1..=600).contains(&code.interval) => Ok(code),
            DeviceAnswer::Code(_) => Err(GithubError::Refused {
                code: "invalid_interval".into(),
            }),
            DeviceAnswer::Error { error } => Err(GithubError::Refused { code: error }),
        }
    }

    pub(crate) async fn poll_device(&self, device_code: &str) -> Result<DevicePoll, GithubError> {
        let response = self
            .http
            .post(self.token_url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("device_code", device_code),
                ("grant_type", super::DEVICE_CODE_GRANT),
            ])
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(GithubError::Request)?;
        let answer = read_json(response, GithubError::Request).await?;
        match answer {
            PollAnswer::Token { access_token } => {
                Ok(DevicePoll::Approved(GithubToken(access_token)))
            }
            PollAnswer::Error { error, interval } => match error.as_str() {
                "authorization_pending" => Ok(DevicePoll::Pending),
                "slow_down" => match interval.filter(|secs| (1..=600).contains(secs)) {
                    Some(interval) => Ok(DevicePoll::SlowDown { interval }),
                    None => Err(GithubError::Refused {
                        code: "invalid_interval".into(),
                    }),
                },
                "expired_token" => Ok(DevicePoll::Expired),
                "access_denied" => Ok(DevicePoll::Denied),
                _ => Err(GithubError::Refused { code: error }),
            },
        }
    }

    pub(crate) async fn user(&self, token: &GithubToken) -> Result<GithubUser, GithubError> {
        let response = self
            .http
            .get(self.user_url.clone())
            .bearer_auth(&token.0)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(GithubError::User)?;
        read_json(response, GithubError::User).await
    }
}

/// The account that holds `login` now (`GET {api_url}/users/{login}`), or
/// `None` when GitHub knows no such login. `login` must already pass
/// [`is_human_handle`], so it is a single path segment.
pub(crate) async fn account_by_login(
    http: &reqwest::Client,
    api_url: &Url,
    login: &str,
) -> Result<Option<GithubUser>, GithubError> {
    debug_assert!(is_human_handle(&login.to_ascii_lowercase()), "{login:?}");
    let response = http
        .get(public_link(api_url, &format!("users/{login}")))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(GithubError::User)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    response
        .error_for_status()
        .map_err(GithubError::User)?
        .json()
        .await
        .map(Some)
        .map_err(GithubError::User)
}

/// The human for a GitHub account, made at first sign-in with the login,
/// lowercased, as handle. Also claims any first-admin rows that `agmen-hub
/// bootstrap` left for the account id. Returns the id and handle.
pub(super) async fn upsert_human(
    hub: &Hub,
    account: &GithubUser,
) -> Result<(String, String), ApiError> {
    let handle = account.login.to_ascii_lowercase();
    if !is_human_handle(&handle) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_handle",
            format!(
                "the GitHub login {:?} cannot be an agmen handle",
                account.login
            ),
        ));
    }
    let github_id = account.id.to_string();
    let row: Result<(String, String), sqlx::Error> = sqlx::query_as(
        "INSERT INTO humans (id, provider, provider_sub, handle) VALUES ($1, 'github', $2, $3)
         ON CONFLICT (provider, provider_sub) DO UPDATE SET provider_sub = EXCLUDED.provider_sub
         RETURNING id, handle",
    )
    .bind(new_id()?)
    .bind(&github_id)
    .bind(&handle)
    .fetch_one(hub.pool())
    .await;
    let (id, stored_handle) = match row {
        Ok(row) => row,
        // The account is new and another account already has the handle.
        Err(err) if is_unique_violation(&err) => {
            return Err(ApiError::conflict(
                "handle_taken",
                format!("the handle {handle:?} belongs to another account"),
            ));
        }
        Err(err) => return Err(err.into()),
    };
    claim_bootstrap_admin(hub, &id, &github_id).await?;
    Ok((id, stored_handle))
}

/// Makes `human` an admin of every project whose unclaimed first admin is
/// GitHub account `github_id`, and marks those rows claimed. Matching the id
/// and never the login keeps a later holder of a renamed login out (ruling
/// R39). One transaction claims rows with their project and tenant memberships.
async fn claim_bootstrap_admin(hub: &Hub, human: &str, github_id: &str) -> Result<(), ApiError> {
    use sqlx::Acquire as _;

    use crate::db::TenantRole;
    use crate::tenants::join_tenant;

    let mut tx = hub.pool().begin().await?;
    let claimed: Vec<(String, String)> = sqlx::query_as(
        "WITH claimed AS (
             UPDATE bootstrap_admins SET claimed_by = $1, claimed_at = now()
             WHERE github_id = $2 AND claimed_by IS NULL
             RETURNING project_id
         ), joined AS (
             INSERT INTO memberships (human_id, project_id, role)
             SELECT $1, project_id, 'admin' FROM claimed
             ON CONFLICT (human_id, project_id) DO UPDATE SET role = 'admin'
             RETURNING project_id
         )
         SELECT p.id, p.tenant_id FROM joined j JOIN projects p ON p.id = j.project_id
         ORDER BY p.tenant_id, p.id",
    )
    .bind(human)
    .bind(github_id)
    .fetch_all(&mut *tx)
    .await?;
    let tenants: std::collections::BTreeSet<_> = claimed.iter().map(|(_, tenant)| tenant).collect();
    for tenant in tenants {
        let owner: Option<String> = sqlx::query_scalar(
            "SELECT human_id FROM tenant_members WHERE tenant_id = $1 AND role = 'owner' FOR UPDATE",
        )
        .bind(tenant)
        .fetch_optional(&mut *tx)
        .await?;
        if owner.is_some() {
            join_tenant(&mut tx, tenant, human, TenantRole::Admin).await?;
        } else {
            // An absent owner row cannot be locked. A savepoint lets a losing
            // concurrent claim retry once as admin without losing memberships.
            let mut attempt = tx.begin().await?;
            let result = async {
                join_tenant(&mut attempt, tenant, human, TenantRole::Owner).await?;
                // Joining preserves existing roles. Only a bootstrap claim may
                // promote an existing member to this tenant's vacant ownership.
                sqlx::query(
                    "UPDATE tenant_members SET role = 'owner'
                     WHERE tenant_id = $1 AND human_id = $2 AND role <> 'owner'",
                )
                .bind(tenant)
                .bind(human)
                .execute(&mut *attempt)
                .await?;
                Ok::<(), sqlx::Error>(())
            }
            .await;
            match result {
                Ok(()) => attempt.commit().await?,
                Err(err) if is_unique_violation(&err) => {
                    attempt.rollback().await?;
                    join_tenant(&mut tx, tenant, human, TenantRole::Admin).await?;
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
    tx.commit().await?;
    if !claimed.is_empty() {
        tracing::info!(
            human,
            github_id,
            projects = claimed.len(),
            "claimed first admin"
        );
    }
    Ok(())
}

/// `humans.handle`'s rule: a GitHub login, lowercased (`[a-z0-9-]`, at most
/// 39 characters, not starting with `-`).
pub(crate) fn is_human_handle(handle: &str) -> bool {
    (1..=39).contains(&handle.len())
        && !handle.starts_with('-')
        && handle
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
