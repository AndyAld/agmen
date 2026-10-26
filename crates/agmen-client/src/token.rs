//! Broker tokens: where a session gets them, when each one expires, and
//! when the session swaps it for a fresh one.

use std::future::Future;
use std::time::Duration;

use agmen_proto::Timestamp;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jiff::SignedDuration;
use serde::Deserialize;

use crate::error::ClientError;

/// How long before a token expires the session reconnects with a fresh
/// one (the hub's broker token docs say 5 minutes). A token that lives
/// less than twice this is renewed halfway through its life instead.
const REFRESH_LEAD: Duration = Duration::from_secs(300);

/// Where a session gets broker tokens: [`HubClient`](crate::HubClient)
/// fetches them from the hub. A session asks for one before each connect
/// and again before the current one expires.
pub trait TokenSource: Send + Sync + 'static {
    /// A fresh broker token.
    ///
    /// # Errors
    ///
    /// [`ClientError`] when no token can be had. [`ClientError::Hub`] with
    /// a 401 or 403 status is final: the session stops.
    fn token(&self) -> impl Future<Output = Result<String, ClientError>> + Send;
}

/// The claim this module reads from a broker token.
#[derive(Debug, Deserialize)]
struct Claims {
    exp: i64,
}

/// When broker token `token` expires: its `exp` claim, read without
/// checking the signature (the broker checks that).
pub(crate) fn expiry(token: &str) -> Result<Timestamp, ClientError> {
    let bad = |why: &str| ClientError::Token(why.to_owned());
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| bad("the broker token is not a JWT"))?;
    let json = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| bad("the broker token's claims are not base64url"))?;
    let claims: Claims = serde_json::from_slice(&json)
        .map_err(|_| bad("the broker token has no numeric exp claim"))?;
    Timestamp::from_second(claims.exp).map_err(|_| bad("the broker token's exp is out of range"))
}

/// When to replace a token that expires at `exp`, as seen at `now`: 5
/// minutes before `exp`, or halfway there for a token that lives less
/// than 10 minutes.
pub(crate) fn refresh_at(now: Timestamp, exp: Timestamp) -> Timestamp {
    let life = exp.duration_since(now);
    if life <= SignedDuration::ZERO {
        return now;
    }
    let lead = SignedDuration::try_from(REFRESH_LEAD).unwrap_or(SignedDuration::MAX);
    let lead = lead.min(life / 2);
    exp.checked_sub(lead).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: &str) -> String {
        format!(
            "eyJhbGciOiJFUzI1NiJ9.{}.c2ln",
            URL_SAFE_NO_PAD.encode(claims)
        )
    }

    #[test]
    fn reads_exp() {
        assert_eq!(
            expiry(&jwt(r#"{"sub":"idn7k2q9","exp":1793000000}"#)).unwrap(),
            Timestamp::from_second(1_793_000_000).unwrap()
        );
        for bad in [
            "not-a-jwt".to_owned(),
            "a.%%%.c".to_owned(),
            jwt(r#"{"sub":"idn7k2q9"}"#),
            jwt(r#"{"exp":"soon"}"#),
            jwt(r#"{"exp":99999999999999}"#),
        ] {
            assert!(expiry(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn refreshes_before_expiry() {
        let now = Timestamp::from_second(1_000_000).unwrap();
        let at = |secs: i64| Timestamp::from_second(1_000_000 + secs).unwrap();
        // A 1 hour token is renewed 5 minutes early.
        assert_eq!(refresh_at(now, at(3600)), at(3300));
        // A 12 s token halfway.
        assert_eq!(refresh_at(now, at(12)), at(6));
        // An expired one at once.
        assert_eq!(refresh_at(now, at(-5)), now);
    }
}
