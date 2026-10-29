//! The hub's HTTP API as one identity sees it (protocol section 6): replay
//! pages and broker tokens, both with the identity's own hub token.

use std::fmt;
use std::time::Duration;

use agmen_proto::{IdentityId, ProjectId, StampedEnvelope, Stream};
use serde::Deserialize;
use serde_json::Value;
use url::{Host, Url};

use crate::body::{
    BodyReadError, MAX_HUB_RESPONSE_BYTES, MAX_PAGE_RESPONSE_BYTES, read_hub_body_with_limit,
};
use crate::error::ClientError;
use crate::token::TokenSource;

/// How long one request to the hub may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The most messages the hub puts in one replay page (protocol 5.3).
pub const PAGE_LIMIT: u32 = 500;

/// A hub URL would send credentials over an unsupported transport.
/// Only the host is retained, so errors cannot expose URL credentials.
#[derive(Debug, Clone, thiserror::Error)]
#[error("the hub at {host} requires a secure connection; use https")]
pub struct HubUrlError {
    host: String,
}

/// Allows HTTPS, or HTTP to localhost or a loopback IP address.
///
/// # Errors
///
/// Returns [`HubUrlError`] for any other scheme or non-loopback HTTP host.
pub fn check_hub_url(url: &Url) -> Result<(), HubUrlError> {
    let loopback = match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if url.scheme() == "https" || (url.scheme() == "http" && loopback) {
        Ok(())
    } else {
        Err(HubUrlError {
            host: url.host_str().unwrap_or("(no host)").to_owned(),
        })
    }
}

/// Validates hub URLs at saved-file boundaries using the shared rule.
pub(crate) fn deserialize_hub_url<'de, D>(deserializer: D) -> Result<Url, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let url = Url::deserialize(deserializer)?;
    check_hub_url(&url).map_err(serde::de::Error::custom)?;
    Ok(url)
}

/// One page of a stream, as `GET /v1/projects/{p}/streams/{s}` answers.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[non_exhaustive]
pub struct Page {
    /// The messages after the requested seq, in seq order.
    pub messages: Vec<StampedEnvelope>,
    /// The seq to ask after for the next page; `None` once this page
    /// reaches `head`.
    pub next_after: Option<u64>,
    /// The stream's last seq; 0 for a stream with no messages.
    pub head: u64,
    /// The highest seq the hub deleted from this stream; 0 when none.
    #[serde(default)]
    pub pruned_through: u64,
}

/// An identity's client for the hub's HTTP API. Cheap to clone.
///
/// It sends the identity's hub token only to the origin of the hub URL it
/// was made with, and follows no redirects, so the token never reaches
/// another host (ruling R42).
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use agmen_client::HubClient;
/// use agmen_proto::Stream;
///
/// let hub = HubClient::new(
///     "https://api.agmen.example".parse()?,
///     "projectp1".parse()?,
///     "idn7k2q9".parse()?,
///     "the identity's hub token",
/// )?;
/// let page = hub.page(&Stream::Feed, 0).await?;
/// println!("{} messages, head {}", page.messages.len(), page.head);
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct HubClient {
    http: reqwest::Client,
    base: Url,
    project: ProjectId,
    identity: IdentityId,
    token: String,
}

impl fmt::Debug for HubClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubClient")
            .field("base", &self.base.as_str())
            .field("project", &self.project)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

/// `GET /v1/identities/{id}/broker-token`'s answer.
#[derive(Debug, Deserialize)]
struct BrokerToken {
    token: String,
}

/// The hub's error body.
#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: String,
}

/// `GET /v1/projects/{p}/state`'s answer. Only `value` is used.
#[derive(Debug, Deserialize)]
struct StateAnswer {
    value: Value,
}

impl HubClient {
    /// A client for `identity` in `project` at the hub at `base` (its
    /// `/v1` routes live under it), sending `token`.
    ///
    /// # Errors
    ///
    /// [`ClientError::HubUrl`] when the URL is unsafe, or
    /// [`ClientError::Http`] when the HTTP client cannot be built.
    pub fn new(
        base: Url,
        project: ProjectId,
        identity: IdentityId,
        token: impl Into<String>,
    ) -> Result<Self, ClientError> {
        check_hub_url(&base)?;
        let http = reqwest::Client::builder()
            .https_only(base.scheme() == "https")
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("agmen-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(ClientError::Http)?;
        Ok(Self {
            http,
            base,
            project,
            identity,
            token: token.into(),
        })
    }

    /// The identity this client acts as.
    #[must_use]
    pub const fn identity(&self) -> &IdentityId {
        &self.identity
    }

    /// Up to 500 messages of `stream` after seq `after`, in seq order.
    ///
    /// # Errors
    ///
    /// [`ClientError::Http`] when the hub cannot be reached, and
    /// [`ClientError::Hub`] when it refuses (403 `forbidden_stream` for a
    /// stream this identity may not read).
    /// [`ClientError::HubBodyTooLarge`] when its body exceeds
    /// [`MAX_PAGE_RESPONSE_BYTES`], or
    /// [`ClientError::BadAnswer`] when a success body is not valid JSON.
    pub async fn page(&self, stream: &Stream, after: u64) -> Result<Page, ClientError> {
        let stream = stream.to_string();
        let mut url = self.url(&["projects", self.project.as_str(), "streams", &stream]);
        url.query_pairs_mut()
            .append_pair("after", &after.to_string())
            .append_pair("limit", &PAGE_LIMIT.to_string());
        self.get(url, MAX_PAGE_RESPONSE_BYTES).await
    }

    /// The project's full roster: the `value` of
    /// `GET /v1/projects/{p}/state?key=roster`, an array of entries ordered
    /// by handle. The retained `state/roster` topic is only a small head.
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use agmen_client::HubClient;
    ///
    /// let hub = HubClient::new(
    ///     "https://api.agmen.example".parse()?,
    ///     "projectp1".parse()?,
    ///     "idn7k2q9".parse()?,
    ///     "the identity's hub token",
    /// )?;
    /// let roster = hub.roster().await?;
    /// println!("{roster}");
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// [`ClientError::Http`] when the hub cannot be reached, and
    /// [`ClientError::Hub`] when it refuses (403 `not_a_member` for a
    /// caller that is not a member of the project).
    /// [`ClientError::HubBodyTooLarge`] when its body exceeds
    /// [`MAX_PAGE_RESPONSE_BYTES`], or
    /// [`ClientError::BadAnswer`] when a success body is not valid JSON
    /// or has no `value`.
    pub async fn roster(&self) -> Result<Value, ClientError> {
        let mut url = self.url(&["projects", self.project.as_str(), "state"]);
        url.query_pairs_mut().append_pair("key", "roster");
        let answer: StateAnswer = self.get(url, MAX_PAGE_RESPONSE_BYTES).await?;
        Ok(answer.value)
    }

    /// A fresh broker token for this identity (1 hour).
    ///
    /// # Errors
    ///
    /// [`ClientError::Http`] when the hub cannot be reached, and
    /// [`ClientError::Hub`] when it refuses (403 `identity_revoked` once the
    /// identity is revoked).
    /// [`ClientError::HubBodyTooLarge`] when its body exceeds 16 MiB, or
    /// [`ClientError::BadAnswer`] when a success body is not valid JSON.
    pub async fn broker_token(&self) -> Result<String, ClientError> {
        let url = self.url(&["identities", self.identity.as_str(), "broker-token"]);
        let answer: BrokerToken = self.get(url, MAX_HUB_RESPONSE_BYTES).await?;
        Ok(answer.token)
    }

    /// `/v1/<segments>` under the base URL, keeping any path prefix it has.
    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().push("v1").extend(segments);
        }
        url
    }

    async fn get<T: for<'de> Deserialize<'de>>(
        &self,
        url: Url,
        limit: usize,
    ) -> Result<T, ClientError> {
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(ClientError::Http)?;
        let status = response.status();
        let bytes = read_hub_body_with_limit(response, limit).await;
        if status.is_success() {
            let bytes = bytes.map_err(ClientError::from)?;
            return serde_json::from_slice(&bytes).map_err(ClientError::BadAnswer);
        }
        // The body names the error; a body that does not parse leaves only
        // the status.
        let code = match bytes {
            Ok(bytes) => serde_json::from_slice::<ErrorBody>(&bytes)
                .map_or_else(|_| String::new(), |body| body.error),
            // Preserve status-only errors when the body cannot be read.
            Err(BodyReadError::Http(_)) => String::new(),
            Err(error) => return Err(error.into()),
        };
        Err(ClientError::Hub {
            status: status.as_u16(),
            code,
        })
    }
}

impl TokenSource for HubClient {
    async fn token(&self) -> Result<String, ClientError> {
        self.broker_token().await
    }
}

#[cfg(test)]
mod url_tests {
    use super::*;

    #[test]
    fn hub_url_policy_errors_expose_only_host() {
        for hub in [
            "http://user:password@hub.example:8090/private?secret=yes#fragment",
            "ftp://user:password@hub.example/private",
        ] {
            let error = check_hub_url(&hub.parse().unwrap()).unwrap_err();
            assert_eq!(
                error.to_string(),
                "the hub at hub.example requires a secure connection; use https"
            );
            assert_eq!(
                format!("{error:?}"),
                "HubUrlError { host: \"hub.example\" }"
            );
        }
        assert!(check_hub_url(&"file:///private".parse().unwrap()).is_err());
    }

    #[test]
    fn hub_url_policy_client() {
        for (base, allowed) in [
            ("http://hub.example", false),
            ("http://192.168.1.1", false),
            ("http://localhost.evil.example", false),
            ("ftp://localhost", false),
            ("http://localhost", true),
            ("http://127.0.0.1", true),
            ("http://127.42.3.4", true),
            ("http://[::1]", true),
            ("https://hub.example", true),
        ] {
            let result = HubClient::new(
                base.parse().unwrap(),
                "project1".parse().unwrap(),
                "idn7k2q9".parse().unwrap(),
                "token",
            );
            assert_eq!(result.is_ok(), allowed, "{base}: {result:?}");
            if let Err(error) = result {
                assert!(error.to_string().contains("use https"), "{error}");
            }
        }
    }
}

#[cfg(test)]
mod roster_tests {
    use axum::extract::Query;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use serde_json::json;

    use super::*;

    async fn state(
        headers: HeaderMap,
        Query(query): Query<Vec<(String, String)>>,
    ) -> impl IntoResponse {
        assert_eq!(query, [("key".to_owned(), "roster".to_owned())]);
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some("Bearer ok")
        {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(json!({"error": "not_a_member"})),
            )
                .into_response();
        }
        axum::Json(json!({
            "key": "roster",
            "as_of_seq": 1,
            "value": [{
                "identity_id": "idn7k2q9",
                "handle": "andy/sol",
                "kind": "agent",
                "status": "online"
            }],
        }))
        .into_response()
    }

    async fn fake_hub() -> String {
        let app = axum::Router::new().route("/v1/projects/{p}/state", get(state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    fn client(base: &str, token: &str) -> HubClient {
        HubClient::new(
            base.parse().unwrap(),
            "project1".parse().unwrap(),
            "idn7k2q9".parse().unwrap(),
            token,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn roster_returns_the_value() {
        let base = fake_hub().await;
        let roster = client(&base, "ok").roster().await.unwrap();
        assert_eq!(
            roster,
            json!([{
                "identity_id": "idn7k2q9",
                "handle": "andy/sol",
                "kind": "agent",
                "status": "online"
            }])
        );
        let refused = client(&base, "no").roster().await.unwrap_err();
        assert!(
            matches!(
                refused,
                ClientError::Hub { status: 403, ref code } if code == "not_a_member"
            ),
            "{refused:?}"
        );
    }
}
