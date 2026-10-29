//! The hub's device sign-in (RFC 8628), shared by the CLI and agmend:
//! [`device_start`] asks the hub for a code, the person confirms it in a
//! browser, and [`device_wait`] polls until the hub grants tokens.
//! [`signed_in_as`] then asks who signed in, and [`save_login`] keeps it.
//!
//! [`save_login`]: crate::save_login

use std::time::Duration;

use agmen_proto::IdentityId;
use serde::Deserialize;
use url::Url;

use crate::body::read_hub_body;
use crate::login::{HumanTokens, Login, LoginError, refusal, token_grant};

/// RFC 8628 section 3.4.
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// RFC 8628 section 3.5: a `slow_down` adds this to the interval.
const SLOW_DOWN: Duration = Duration::from_secs(5);
/// The most polls one sign-in makes, whatever the hub says about expiry.
const MAX_POLLS: u32 = 720;
/// The longest a sign-in waits, whatever `expires_in` the hub claims.
const MAX_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// `/v1/<segments>` under `hub`, keeping any path prefix it has.
fn hub_url(hub: &Url, segments: &[&str]) -> Url {
    let mut url = hub.clone();
    if let Ok(mut path) = url.path_segments_mut() {
        path.pop_if_empty().push("v1").extend(segments);
    }
    url
}

/// The hub's answer to a device sign-in start (RFC 8628 section 3.2). The
/// device code is private: it stays inside this crate's [`device_wait`] and
/// never shows in `Debug`.
#[derive(Clone, Deserialize)]
pub struct DeviceStart {
    /// The code the person types at `verification_uri`.
    pub user_code: String,
    /// The page where the person enters the code.
    pub verification_uri: Url,
    /// How many seconds the code lasts.
    pub expires_in: u64,
    /// How many seconds to wait between polls.
    pub interval: u64,
    device_code: String,
}

impl std::fmt::Debug for DeviceStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceStart")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri.as_str())
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

/// Starts a device sign-in at `hub` (`POST /v1/auth/device`).
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let http = reqwest::Client::new();
/// let hub = "https://hub.example".parse()?;
/// let start = agmen_client::device_start(&http, &hub).await?;
/// println!("enter {} at {}", start.user_code, start.verification_uri);
/// let tokens = agmen_client::device_wait(&http, &hub, &start).await?;
/// # let _ = tokens;
/// # Ok(()) }
/// ```
///
/// # Errors
///
/// [`LoginError::HubUrl`] when `hub` is not a safe URL for credentials,
/// [`LoginError::Http`] when it cannot be reached,
/// [`LoginError::Refused`] when it refuses, and [`LoginError::ReadAnswer`],
/// [`LoginError::HubBodyTooLarge`] or [`LoginError::BadAnswer`] when its
/// answer cannot be read.
pub async fn device_start(http: &reqwest::Client, hub: &Url) -> Result<DeviceStart, LoginError> {
    crate::check_hub_url(hub)?;
    let response = http
        .post(hub_url(hub, &["auth", "device"]))
        .send()
        .await
        .map_err(LoginError::Http)?;
    let status = response.status();
    let bytes = read_hub_body(response).await;
    if !status.is_success() {
        return Err(refusal(status.as_u16(), bytes));
    }
    let bytes = bytes.map_err(LoginError::from)?;
    serde_json::from_slice(&bytes).map_err(LoginError::BadAnswer)
}

/// Polls the token endpoint until the person confirms the code, and returns
/// the granted tokens. Waits `start.interval` seconds (at least 1) between
/// polls, 5 more after each `slow_down`; stops after 720 polls or when
/// `start.expires_in` has passed.
///
/// # Errors
///
/// [`LoginError::Declined`] when the person declined,
/// [`LoginError::Expired`] when the code ran out, and the error of the
/// token endpoint (as [`token_grant`] reports it) for any other failure.
pub async fn device_wait(
    http: &reqwest::Client,
    hub: &Url,
    start: &DeviceStart,
) -> Result<HumanTokens, LoginError> {
    // A hub that claims a huge window or interval cannot stretch the wait
    // past a day, or make a sleep or an instant overflow.
    let window = Duration::from_secs(start.expires_in).min(MAX_WAIT);
    let deadline = tokio::time::Instant::now() + window;
    let mut interval = Duration::from_secs(start.interval.max(1));
    for _ in 0..MAX_POLLS {
        tokio::time::sleep(interval.min(window)).await;
        if tokio::time::Instant::now() > deadline {
            break;
        }
        let form = [
            ("grant_type", DEVICE_CODE_GRANT),
            ("device_code", start.device_code.as_str()),
        ];
        let error = match token_grant(http, hub, &form).await {
            Ok(tokens) => return Ok(tokens),
            Err(error) => error,
        };
        let LoginError::Refused { status, code, .. } = &error else {
            return Err(error);
        };
        match (*status, code.as_str()) {
            (_, "authorization_pending") => {}
            (_, "slow_down") => interval = interval.saturating_add(SLOW_DOWN).min(MAX_WAIT),
            (_, "expired_token") => return Err(LoginError::Expired),
            (400, "access_denied") => return Err(LoginError::Declined),
            _ => return Err(error),
        }
    }
    Err(LoginError::Expired)
}

/// `GET /v1/me`: only the person's id and handle, whatever else it says.
#[derive(Deserialize)]
struct WhoAmI {
    id: IdentityId,
    handle: String,
}

/// Who `access_token` signed in as at `hub`, as the [`Login`] to save.
///
/// # Errors
///
/// The errors of [`device_start`], with [`LoginError::Refused`] when the hub
/// does not take the token.
pub async fn signed_in_as(
    http: &reqwest::Client,
    hub: &Url,
    access_token: &str,
) -> Result<Login, LoginError> {
    crate::check_hub_url(hub)?;
    let response = http
        .get(hub_url(hub, &["me"]))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(LoginError::Http)?;
    let status = response.status();
    let bytes = read_hub_body(response).await;
    if !status.is_success() {
        return Err(refusal(status.as_u16(), bytes));
    }
    let bytes = bytes.map_err(LoginError::from)?;
    let me: WhoAmI = serde_json::from_slice(&bytes).map_err(LoginError::BadAnswer)?;
    Ok(Login {
        hub_url: hub.clone(),
        human_id: me.id,
        handle: me.handle,
    })
}
