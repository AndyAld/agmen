//! The RFC 8628 device flow: `agmen login` asks for a code, the person
//! signs in with GitHub in a browser and approves, and the CLI's poll then
//! gets tokens.

use axum::Json;
use axum::extract::{Form, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde::{Deserialize, Serialize};

use super::{TokenResponse, issue_human_tokens};
use crate::db::{hash_secret, new_secret};
use crate::http::{ApiError, Hub};

/// How long a device login stays open, in seconds.
const EXPIRES_IN_SECS: i64 = 600;
/// The poll interval the CLI is told to keep, in seconds.
const INTERVAL_SECS: u32 = 5;
/// Consonants only, so a code never spells a word; 16 of them, so a random
/// byte masked to 4 bits picks one with no bias. 8 of them give 32 bits.
const USER_CODE_ALPHABET: &[u8; 16] = b"BCDFGHJKLMNPQRST";
const USER_CODE_LEN: usize = 8;
/// Fresh user codes tried before giving up on a collision.
const USER_CODE_ATTEMPTS: usize = 3;

/// A user code as people type it (`BCDF-GHJK`, any case, dash optional),
/// reduced to the stored form, or `None` if it cannot be one.
pub(super) fn normalize_user_code(input: &str) -> Option<String> {
    let code: String = input
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == USER_CODE_LEN && code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b)))
        .then_some(code)
}

/// The stored form shown with a dash in the middle: `BCDF-GHJK`.
pub(super) fn display_user_code(code: &str) -> String {
    let (head, tail) = code.split_at(code.len().min(USER_CODE_LEN / 2));
    format!("{head}-{tail}")
}

fn new_user_code() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; USER_CODE_LEN];
    getrandom::fill(&mut bytes)?;
    Ok(bytes
        .iter()
        .map(|b| char::from(USER_CODE_ALPHABET[usize::from(b & 15)]))
        .collect())
}

/// RFC 8628 section 3.2.
#[derive(Debug, Serialize)]
pub(super) struct DeviceStart {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: i64,
    interval: u32,
}

/// `POST /v1/auth/device`: opens a device login.
pub(super) async fn start(State(hub): State<Hub>) -> Result<Json<DeviceStart>, ApiError> {
    sqlx::query("DELETE FROM device_authorizations WHERE expires_at <= now()")
        .execute(hub.pool())
        .await?;
    let device_code = new_secret()?;
    let mut user_code = None;
    for _ in 0..USER_CODE_ATTEMPTS {
        let candidate = new_user_code()?;
        let inserted = sqlx::query(
            "INSERT INTO device_authorizations (device_code_hash, user_code, expires_at)
             VALUES ($1, $2, now() + make_interval(secs => $3))
             ON CONFLICT (user_code) DO NOTHING",
        )
        .bind(hash_secret(&device_code))
        .bind(&candidate)
        .bind(EXPIRES_IN_SECS)
        .execute(hub.pool())
        .await?
        .rows_affected();
        if inserted == 1 {
            user_code = Some(candidate);
            break;
        }
    }
    let user_code = user_code.ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "temporarily_unavailable",
            "no free user code, try again",
        )
    })?;
    let shown = display_user_code(&user_code);
    let mut complete = hub.public_link("v1/auth/oauth/github/start");
    complete.query_pairs_mut().append_pair("user_code", &shown);
    Ok(Json(DeviceStart {
        device_code,
        user_code: shown,
        verification_uri: hub.public_link("v1/auth/device").into(),
        verification_uri_complete: complete.into(),
        expires_in: EXPIRES_IN_SECS,
        interval: INTERVAL_SECS,
    }))
}

/// `GET /v1/auth/device`: the page where a person types the code their
/// terminal shows.
pub(super) async fn form(State(hub): State<Hub>) -> Html<String> {
    let action = hub.public_link("v1/auth/oauth/github/start");
    page(
        "Sign in to agmen",
        &format!(
            "<p>Enter the code your terminal shows.</p>\
             <form method=\"get\" action=\"{}\">\
             <input name=\"user_code\" autocomplete=\"off\" autofocus> \
             <button>Continue with GitHub</button></form>",
            escape(action.as_str())
        ),
    )
}

#[derive(Debug, Deserialize)]
pub(super) struct ApproveForm {
    approve: String,
}

/// `POST /v1/auth/device/approve`: the confirm page's button. The form
/// carries a one-use secret from the OAuth callback, so a link alone can
/// never approve a login.
pub(super) async fn approve(
    State(hub): State<Hub>,
    Form(form): Form<ApproveForm>,
) -> Result<Response, ApiError> {
    let approved = sqlx::query(
        "UPDATE device_authorizations SET approved_at = now(), approval_hash = NULL
         WHERE approval_hash = $1 AND expires_at > now() AND approved_at IS NULL",
    )
    .bind(hash_secret(&form.approve))
    .execute(hub.pool())
    .await?
    .rows_affected();
    if approved == 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "this sign-in has expired or was already approved; run agmen login again",
        ));
    }
    Ok(no_store(page(
        "Signed in",
        "<p>You can close this tab and return to your terminal.</p>",
    )))
}

/// Approves the open device login whose user code is `user_code` (as
/// people type it) for `human`, as the confirm page's button does after a
/// GitHub sign-in. Returns whether a login was approved: `false` for an
/// unknown, expired or already approved code.
#[cfg(feature = "test-hooks")]
pub(crate) async fn approve_as(hub: &Hub, user_code: &str, human: &str) -> Result<bool, ApiError> {
    let Some(code) = normalize_user_code(user_code) else {
        return Ok(false);
    };
    let approved = sqlx::query(
        "UPDATE device_authorizations
         SET human_id = $1, approved_at = now(), oauth_state_hash = NULL, approval_hash = NULL
         WHERE user_code = $2 AND expires_at > now() AND approved_at IS NULL",
    )
    .bind(human)
    .bind(&code)
    .execute(hub.pool())
    .await?
    .rows_affected();
    Ok(approved == 1)
}

/// The device code grant: tokens once the login is approved, otherwise the
/// RFC 8628 section 3.5 error. A device code is redeemed once.
pub(super) async fn redeem(hub: &Hub, device_code: &str) -> Result<TokenResponse, ApiError> {
    let hash = hash_secret(device_code);
    let mut tx = hub.pool().begin().await?;
    let row: Option<(Option<String>, bool, bool)> = sqlx::query_as(
        "SELECT human_id, approved_at IS NOT NULL, expires_at <= now()
         FROM device_authorizations WHERE device_code_hash = $1 FOR UPDATE",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((human, approved, expired)) = row else {
        return Err(ApiError::grant(
            "invalid_grant",
            "the device code is unknown or was already used",
        ));
    };
    if expired {
        sqlx::query("DELETE FROM device_authorizations WHERE device_code_hash = $1")
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Err(ApiError::grant(
            "expired_token",
            "the device code has expired",
        ));
    }
    let (true, Some(human)) = (approved, human) else {
        return Err(ApiError::grant(
            "authorization_pending",
            "the login has not been approved yet",
        ));
    };
    sqlx::query("DELETE FROM device_authorizations WHERE device_code_hash = $1")
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    let tokens = issue_human_tokens(&mut tx, &human).await?;
    tx.commit().await?;
    Ok(tokens)
}

/// A minimal HTML page. `body` must already be escaped.
pub(super) fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title}</title></head>\
         <body style=\"font-family: system-ui, sans-serif; max-width: 32rem; margin: 3rem auto; padding: 0 1rem\">\
         <h1>{title}</h1>{body}</body></html>",
        title = escape(title)
    ))
}

/// `page` with `Cache-Control: no-store`, for the confirm and approve pages:
/// the confirm page carries a one-use approve secret, and neither belongs
/// in a shared or back-button cache (ruling R38).
pub(super) fn no_store(page: Html<String>) -> Response {
    let mut response = page.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Escapes text for HTML content and quoted attribute values.
pub(super) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_codes_normalize_and_display() {
        let code = new_user_code().unwrap();
        assert_eq!(normalize_user_code(&code).as_deref(), Some(code.as_str()));
        let shown = display_user_code(&code);
        assert_eq!(shown.len(), 9);
        assert_eq!(
            normalize_user_code(&shown.to_lowercase()).as_deref(),
            Some(code.as_str())
        );
        for bad in ["", "BCDF-GHJ", "BCDF-GHJKL", "AEIO-UBCD", "bcdf ghj1"] {
            assert_eq!(normalize_user_code(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn escape_covers_markup_and_quotes() {
        assert_eq!(
            escape("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
