//! The hub's HTTP routes agmend calls as the person (protocol section 6):
//! state, replay, events and the human post. Every call sends the person's
//! access token, and only to the origin of the hub in their login file;
//! the client follows no redirects (ruling R42), as agmen-client's does.
//!
//! agmen-client's `HubClient` is one identity's client with a fixed token,
//! a fixed page size and a 10 s total timeout, which would cut every
//! events stream; this client takes the token per call from
//! [`Credentials`] and bounds the events stream by a read timeout instead.

use std::collections::BTreeMap;
use std::time::Duration;

use agmen_client::{LoginError, Page};
use agmen_local::console::RosterEntry;
use agmen_local::{ApiError, ErrorCode};
use agmen_proto::{ClientEnvelope, DeliveryReply, IdentityId, NackReason, ProjectId, Seq, Stream};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use crate::login::Credentials;

/// How long one request (not an events stream) may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for a TCP connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest silence on a connection: three of the hub's 15 s
/// keep-alives, so a dead events stream is noticed within 45 s.
const READ_TIMEOUT: Duration = Duration::from_secs(45);

/// Why a hub call failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HubError {
    #[error("invalid hub response: {0}")]
    Invalid(&'static str),
    /// The person's sign-in could not be had.
    #[error(transparent)]
    Login(#[from] LoginError),
    /// The hub could not be reached, or its answer could not be read.
    #[error("the hub could not be reached")]
    Http(#[source] reqwest::Error),
    /// The hub refused with an error body.
    #[error("the hub answered {status} {code}")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// The hub's error code; empty when the body named none.
        code: String,
    },
    /// The hub did not record a posted message.
    #[error("the hub refused the message: {reason}")]
    Nack {
        /// Why.
        reason: NackReason,
        /// When the hub says to retry.
        retry_after_ms: Option<u64>,
    },
}

impl HubError {
    /// Whether retrying soon will not help: nobody is signed in, the hub
    /// refused the sign-in, or it refused this person the project (401,
    /// 403, 404). Everything else (unreachable, 5xx, a broken answer) is
    /// the hub being away.
    pub(crate) const fn is_final(&self) -> bool {
        matches!(
            self,
            Self::Login(LoginError::NotSignedIn | LoginError::Refused { .. })
                | Self::Refused {
                    status: 401 | 403 | 404,
                    ..
                }
        )
    }
}

impl From<HubError> for ApiError {
    fn from(err: HubError) -> Self {
        let (code, message) = match &err {
            HubError::Login(LoginError::NotSignedIn) => (
                ErrorCode::Unauthorized,
                "not signed in: run agmen login --hub <url>".to_owned(),
            ),
            // The hub refused the refresh grant: the sign-in is over.
            HubError::Login(LoginError::Refused { .. }) => (
                ErrorCode::Unauthorized,
                format!("{}: run agmen login", chain(&err)),
            ),
            // The renewal could not reach the hub, or the store or the
            // login file failed: nothing says the person signed out, so a
            // console treats it as the hub being away and retries.
            HubError::Login(_) => (ErrorCode::Hub, chain(&err)),
            HubError::Refused { status: 401, .. } => (
                ErrorCode::Unauthorized,
                format!("{err}: the sign-in has ended; run agmen login"),
            ),
            HubError::Refused { status: 403, .. } => (ErrorCode::Unauthorized, err.to_string()),
            HubError::Refused { status: 404, .. } => (ErrorCode::NotFound, err.to_string()),
            HubError::Nack {
                reason,
                retry_after_ms: Some(ms),
            } => (
                ErrorCode::Hub,
                format!("the hub refused the message: {reason} (retry after {ms} ms)"),
            ),
            HubError::Invalid(_)
            | HubError::Refused { .. }
            | HubError::Http(_)
            | HubError::Nack { .. } => (ErrorCode::Hub, chain(&err)),
        };
        Self { code, message }
    }
}

/// `err` and its sources, joined by `: `.
fn chain(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut source = err.source();
    // Error chains are short; the bound only guards a cyclic one.
    for _ in 0..16 {
        let Some(next) = source else { break };
        text.push_str(": ");
        text.push_str(&next.to_string());
        source = next.source();
    }
    text
}

/// The hub's error body.
#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: String,
}

/// `GET state?key=`'s answer.
#[derive(Debug, Deserialize)]
struct StateAnswer<T> {
    value: T,
}

/// The `head` state's value; only the field agmend reads.
#[derive(Debug, Deserialize)]
struct Head {
    feed_seq: u64,
}

/// The person's client for the hub.
#[derive(Debug)]
pub(crate) struct Hub {
    http: reqwest::Client,
    credentials: std::sync::Arc<Credentials>,
}

impl Hub {
    pub(crate) const fn new(
        http: reqwest::Client,
        credentials: std::sync::Arc<Credentials>,
    ) -> Self {
        Self { http, credentials }
    }

    /// An HTTP client for the hub: no redirects, bounded connect and read.
    ///
    /// # Errors
    ///
    /// When reqwest cannot build one (no TLS backend).
    pub(crate) fn http() -> Result<reqwest::Client, reqwest::Error> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .user_agent(concat!("agmend/", env!("CARGO_PKG_VERSION")))
            .build()
    }

    pub(crate) fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// Sends `method` to `/v1/projects/{project}/<segments>?<query>` with
    /// the person's token, after `build` adds the rest.
    async fn send(
        &self,
        method: Method,
        project: &ProjectId,
        segments: &[&str],
        query: &[(&str, &str)],
        build: impl FnOnce(RequestBuilder) -> RequestBuilder,
    ) -> Result<reqwest::Response, HubError> {
        let (login, token) = self.credentials.access().await?;
        let mut url = url(&login.hub_url, project, segments);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let request = build(self.http.request(method, url).bearer_auth(token));
        let response = request.send().await.map_err(HubError::Http)?;
        if response.status() == StatusCode::UNAUTHORIZED {
            self.credentials.forget().await;
        }
        Ok(response)
    }

    async fn get<T: DeserializeOwned>(
        &self,
        project: &ProjectId,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> Result<T, HubError> {
        let response = self
            .send(Method::GET, project, segments, query, |r| {
                r.timeout(REQUEST_TIMEOUT)
            })
            .await?;
        json(response).await
    }

    async fn get_bounded<T: DeserializeOwned>(
        &self,
        project: &ProjectId,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> Result<T, HubError> {
        let response = self
            .send(Method::GET, project, segments, query, |r| {
                r.timeout(REQUEST_TIMEOUT)
            })
            .await?;
        if !response.status().is_success() {
            return Err(refusal(response).await);
        }
        body_json(response).await
    }

    /// The feed's head seq (`state?key=head`).
    pub(crate) async fn head(&self, project: &ProjectId) -> Result<u64, HubError> {
        let answer: StateAnswer<Head> = self.get(project, &["state"], &[("key", "head")]).await?;
        Ok(answer.value.feed_seq)
    }

    /// The roster (`state?key=roster`).
    pub(crate) async fn roster(&self, project: &ProjectId) -> Result<Vec<RosterEntry>, HubError> {
        let answer: StateAnswer<Vec<RosterEntry>> =
            self.get(project, &["state"], &[("key", "roster")]).await?;
        Ok(answer.value)
    }

    /// Up to `limit` (at most 500) feed messages after seq `after`.
    pub(crate) async fn page(
        &self,
        project: &ProjectId,
        after: u64,
        limit: u32,
    ) -> Result<Page, HubError> {
        let (after, limit) = (after.to_string(), limit.to_string());
        self.get(
            project,
            &["streams", "feed"],
            &[("after", &after), ("limit", &limit)],
        )
        .await
    }

    pub(crate) async fn inbox(
        &self,
        project: &ProjectId,
        identity: &IdentityId,
        after: u64,
        limit: u32,
    ) -> Result<Page, HubError> {
        self.stream_page(project, &Stream::Inbox(identity.clone()), after, limit)
            .await
    }

    pub(crate) async fn stream_page(
        &self,
        project: &ProjectId,
        stream: &Stream,
        after: u64,
        limit: u32,
    ) -> Result<Page, HubError> {
        let page: Page = self
            .get_bounded(
                project,
                &["streams", &stream.to_string()],
                &[("after", &after.to_string()), ("limit", &limit.to_string())],
            )
            .await?;
        if page.messages.len() > limit as usize
            || page
                .messages
                .iter()
                .any(|m| &m.stream != stream || m.seq.0 <= after)
        {
            return Err(HubError::Invalid("replay bounds or stream"));
        }
        Ok(page)
    }

    pub(crate) async fn state(
        &self,
        project: &ProjectId,
        key: &str,
    ) -> Result<serde_json::Value, HubError> {
        self.get_bounded(project, &["state"], &[("key", key)]).await
    }

    /// Posts `envelope` as the person; the stream and seq it was recorded
    /// at.
    pub(crate) async fn post(
        &self,
        project: &ProjectId,
        envelope: &ClientEnvelope,
    ) -> Result<(Stream, Seq), HubError> {
        let response = self
            .send(Method::POST, project, &["messages"], &[], |r| {
                r.json(envelope).timeout(REQUEST_TIMEOUT)
            })
            .await?;
        // human_post.rs: an ACK is 200 and a NACK 409, 422, 429 or 503,
        // each with the reply as the body.
        let status = response.status().as_u16();
        if !matches!(status, 200 | 409 | 422 | 429 | 503) {
            return Err(refusal(response).await);
        }
        match response.json().await.map_err(HubError::Http)? {
            DeliveryReply::Ack { stream, seq, .. } => Ok((stream, seq)),
            DeliveryReply::Nack {
                reason,
                retry_after_ms,
                ..
            } => Err(HubError::Nack {
                reason,
                retry_after_ms,
            }),
        }
    }

    /// Opens the events stream, resuming each stream in `after` past its
    /// seq; `last_id` is the last event id seen, if any.
    pub(crate) async fn events(
        &self,
        project: &ProjectId,
        after: &BTreeMap<Stream, u64>,
        last_id: Option<&str>,
    ) -> Result<reqwest::Response, HubError> {
        let after = after
            .iter()
            .map(|(stream, seq)| format!("{stream}:{seq}"))
            .collect::<Vec<_>>()
            .join(",");
        let query: &[(&str, &str)] = if after.is_empty() {
            &[]
        } else {
            &[("after", after.as_str())]
        };
        let response = self
            .send(Method::GET, project, &["events"], query, |mut r| {
                if let Some(id) = last_id {
                    r = r.header("Last-Event-ID", id);
                }
                r.header("Accept", "text/event-stream")
            })
            .await?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(refusal(response).await)
        }
    }
}

/// `/v1/projects/{project}/<segments>` under `base`, keeping any path
/// prefix it has.
fn url(base: &Url, project: &ProjectId, segments: &[&str]) -> Url {
    let mut url = base.clone();
    if let Ok(mut path) = url.path_segments_mut() {
        path.pop_if_empty()
            .extend(["v1", "projects", project.as_str()])
            .extend(segments);
    }
    url
}

/// A success's JSON body, or the refusal.
async fn json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, HubError> {
    if response.status().is_success() {
        response.json().await.map_err(HubError::Http)
    } else {
        Err(refusal(response).await)
    }
}

async fn body_json<T: DeserializeOwned>(mut response: reqwest::Response) -> Result<T, HubError> {
    const MAX_BODY: usize = 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    for _ in 0..=MAX_BODY {
        let Some(chunk) = response.chunk().await.map_err(HubError::Http)? else {
            return serde_json::from_slice(&bytes).map_err(|_| HubError::Invalid("JSON"));
        };
        if chunk.len() > MAX_BODY.saturating_sub(bytes.len()) {
            return Err(HubError::Invalid("body exceeds 16 MiB"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Err(HubError::Invalid("too many body chunks"))
}

/// A failed answer as [`HubError::Refused`].
async fn refusal(response: reqwest::Response) -> HubError {
    let status = response.status().as_u16();
    // A body that does not parse leaves only the status.
    let code = body_json::<ErrorBody>(response)
        .await
        .map_or_else(|_| String::new(), |body| body.error);
    HubError::Refused { status, code }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn oversized_success_and_refusal_bodies_are_bounded() {
        let body = serde_json::json!({"error":"x".repeat(16 * 1024 * 1024)}).to_string();
        let response = axum::http::Response::builder()
            .status(200)
            .body(body.clone())
            .unwrap();
        assert!(matches!(
            body_json::<serde_json::Value>(response.into()).await,
            Err(HubError::Invalid("body exceeds 16 MiB"))
        ));
        let response = axum::http::Response::builder()
            .status(403)
            .body(body)
            .unwrap();
        assert!(
            matches!(refusal(response.into()).await,HubError::Refused { status:403,code } if code.is_empty())
        );
    }
}
