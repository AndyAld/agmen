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

use agmen_client::{
    BodyReadError, LoginError, MAX_HUB_RESPONSE_BYTES, MAX_PAGE_RESPONSE_BYTES, Page,
    read_hub_body, read_hub_body_with_limit,
};
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

/// Keeps reqwest's decode error text and source after a bounded read.
#[derive(Debug, thiserror::Error)]
#[error("error decoding response body for url ({url})")]
pub(crate) struct DecodeError {
    url: Url,
    #[source]
    source: serde_json::Error,
}

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
    /// The hub answered with invalid JSON.
    #[error("the hub could not be reached")]
    BadAnswer(#[source] DecodeError),
    /// The hub's response body exceeded the shared cap.
    #[error(transparent)]
    Body(BodyReadError),
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
            | HubError::BadAnswer(_)
            | HubError::Body(_)
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
        agmen_client::check_hub_url(&login.hub_url).map_err(LoginError::from)?;
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
        let response = self
            .send(
                Method::GET,
                project,
                &["streams", "feed"],
                &[("after", &after), ("limit", &limit)],
                |r| r.timeout(REQUEST_TIMEOUT),
            )
            .await?;
        json_with_limit(response, MAX_PAGE_RESPONSE_BYTES).await
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
            .get(
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
        self.get(project, &["state"], &[("key", key)]).await
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
        let url = response.url().clone();
        let bytes = read_hub_body(response).await.map_err(|error| match error {
            BodyReadError::Http(error) => HubError::Http(error),
            error => HubError::Body(error),
        })?;
        match serde_json::from_slice(&bytes)
            .map_err(|source| HubError::BadAnswer(DecodeError { url, source }))?
        {
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
    json_with_limit(response, MAX_HUB_RESPONSE_BYTES).await
}

/// A JSON answer bounded by the endpoint's response limit.
async fn json_with_limit<T: DeserializeOwned>(
    response: reqwest::Response,
    limit: usize,
) -> Result<T, HubError> {
    if response.status().is_success() {
        let url = response.url().clone();
        let bytes =
            read_hub_body_with_limit(response, limit)
                .await
                .map_err(|error| match error {
                    BodyReadError::Http(error) => HubError::Http(error),
                    error => HubError::Body(error),
                })?;
        serde_json::from_slice(&bytes)
            .map_err(|source| HubError::BadAnswer(DecodeError { url, source }))
    } else {
        Err(refusal_with_limit(response, limit).await)
    }
}

/// A failed answer as [`HubError::Refused`].
async fn refusal(response: reqwest::Response) -> HubError {
    refusal_with_limit(response, MAX_HUB_RESPONSE_BYTES).await
}

/// A failed answer bounded by the endpoint's response limit.
async fn refusal_with_limit(response: reqwest::Response, limit: usize) -> HubError {
    let status = response.status().as_u16();
    // A body that does not parse leaves only the status.
    let code = match read_hub_body_with_limit(response, limit).await {
        Ok(bytes) => serde_json::from_slice::<ErrorBody>(&bytes)
            .map_or_else(|_| String::new(), |body| body.error),
        Err(BodyReadError::Http(_)) => String::new(),
        Err(error) => return HubError::Body(error),
    };
    HubError::Refused { status, code }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn oversized_success_and_refusal_bodies_are_bounded() {
        let body = serde_json::json!({"error":"x".repeat(16 * 1024 * 1024)}).to_string();
        let response = axum::http::Response::builder()
            .status(200)
            .body(body.clone())
            .unwrap();
        assert!(matches!(
            json::<serde_json::Value>(response.into()).await,
            Err(HubError::Body(BodyReadError::TooLarge {
                limit: MAX_HUB_RESPONSE_BYTES
            }))
        ));
        let response = axum::http::Response::builder()
            .status(403)
            .body(body)
            .unwrap();
        assert!(matches!(
            refusal(response.into()).await,
            HubError::Body(BodyReadError::TooLarge {
                limit: MAX_HUB_RESPONSE_BYTES
            })
        ));
    }

    #[tokio::test]
    async fn hub_url_policy_daemon() {
        use agmen_client::{Login, TokenStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("login.json");
        for base in ["http://hub.example", "ftp://localhost"] {
            let login = Login {
                hub_url: base.parse().unwrap(),
                human_id: "hmn7k2q9".parse().unwrap(),
                handle: "andy".into(),
            };
            std::fs::write(&path, serde_json::to_vec(&login).unwrap()).unwrap();
            let http = Hub::http().unwrap();
            let hub = Hub::new(
                http.clone(),
                std::sync::Arc::new(Credentials::new(
                    path.clone(),
                    TokenStore::Dir(dir.path().join("tokens")),
                    http,
                )),
            );
            let error = hub.head(&"project1".parse().unwrap()).await.unwrap_err();
            assert!(chain(&error).contains("use https"), "{error:?}");
        }
    }

    #[tokio::test]
    async fn replay_page_body_boundaries() {
        use agmen_client::{HumanTokens, Login, TokenStore, store_tokens};

        let cap = 500 * 80 * 1024 + 1024 * 1024;
        for (status, size) in [(200, cap - 1), (200, cap + 1), (403, cap + 1)] {
            let mut body = br#"{"messages":[],"next_after":null,"head":0}"#.to_vec();
            body.resize(size, b' ');
            let app = axum::Router::new().route(
                "/v1/projects/projectp1/streams/feed",
                axum::routing::get(move || {
                    let body = body.clone();
                    async move { (axum::http::StatusCode::from_u16(status).unwrap(), body) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let login = Login {
                hub_url: format!("http://{}", listener.local_addr().unwrap())
                    .parse()
                    .unwrap(),
                human_id: "hmn7k2q9".parse().unwrap(),
                handle: "andy".to_owned(),
            };
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let dir = tempfile::tempdir().unwrap();
            let login_file = dir.path().join("login.json");
            std::fs::write(&login_file, serde_json::to_vec(&login).unwrap()).unwrap();
            let store = TokenStore::Dir(dir.path().join("tokens"));
            store_tokens(
                &store,
                &login,
                &HumanTokens::new("access".into(), "refresh".into(), 3600),
            )
            .await
            .unwrap();
            let http = Hub::http().unwrap();
            let hub = Hub::new(
                http.clone(),
                std::sync::Arc::new(Credentials::new(login_file, store, http)),
            );
            let result = hub.page(&"projectp1".parse().unwrap(), 0, 500).await;
            task.abort();
            if size < cap {
                let page = result.unwrap();
                assert!(page.messages.is_empty());
                assert_eq!(page.next_after, None);
                assert_eq!(page.head, 0);
            } else {
                assert!(matches!(result, Err(HubError::Body(_))), "{result:?}");
                assert!(result.unwrap_err().to_string().contains(&cap.to_string()));
            }
        }
    }

    #[tokio::test]
    async fn normal_hub_error_text_is_unchanged() {
        for (status, body) in [
            (200, ""),
            (200, "not json"),
            (403, r#"{"error":"forbidden"}"#),
            (403, ""),
            (403, "not json"),
        ] {
            let app = axum::Router::new().fallback(move || async move {
                (axum::http::StatusCode::from_u16(status).unwrap(), body)
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let http = Hub::http().unwrap();
            let expected = if status == 200 {
                let error = http
                    .get(&url)
                    .send()
                    .await
                    .unwrap()
                    .json::<serde_json::Value>()
                    .await
                    .unwrap_err();
                chain(&HubError::Http(error))
            } else if body.starts_with('{') {
                "the hub answered 403 forbidden".to_owned()
            } else {
                "the hub answered 403 ".to_owned()
            };
            let error = json::<serde_json::Value>(http.get(&url).send().await.unwrap())
                .await
                .unwrap_err();
            task.abort();
            assert_eq!(chain(&error), expected);
        }
    }

    #[tokio::test]
    async fn oversized_post_replies_are_refused() {
        use agmen_client::{HumanTokens, Login, TokenStore, store_tokens};
        use axum::body::{Body, Bytes};

        for status in [200, 409, 422, 429, 503, 403] {
            let app = axum::Router::new().fallback(move || async move {
                axum::http::Response::builder()
                    .status(status)
                    .header("content-length", "16777217")
                    .body(Body::from_stream(tokio_stream::pending::<
                        Result<Bytes, std::convert::Infallible>,
                    >()))
                    .unwrap()
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let login = Login {
                hub_url: format!("http://{}", listener.local_addr().unwrap())
                    .parse()
                    .unwrap(),
                human_id: "hmn7k2q9".parse().unwrap(),
                handle: "andy".to_owned(),
            };
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let dir = tempfile::tempdir().unwrap();
            let login_file = dir.path().join("login.json");
            std::fs::write(&login_file, serde_json::to_vec(&login).unwrap()).unwrap();
            let store = TokenStore::Dir(dir.path().join("tokens"));
            store_tokens(
                &store,
                &login,
                &HumanTokens::new("access".into(), "refresh".into(), 3600),
            )
            .await
            .unwrap();
            let http = Hub::http().unwrap();
            let hub = Hub::new(
                http.clone(),
                std::sync::Arc::new(Credentials::new(login_file, store, http)),
            );
            let envelope = serde_json::from_value(serde_json::json!({
                "v": 1, "id": "01JBX6Q4T7W3C2M8Y9K5P00001", "verb": "FYI",
                "data": {}, "body": "hello", "sent_at": "2026-10-25T16:04:11Z"
            }))
            .unwrap();
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                hub.post(&"projectp1".parse().unwrap(), &envelope),
            )
            .await;
            task.abort();
            let error = result
                .expect("refuse before reading the post body")
                .unwrap_err();
            assert!(
                error.to_string().contains("16777216 byte limit"),
                "{status}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn oversized_hub_bodies_are_refused() {
        for status in [200, 403] {
            for chunked in [false, true] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                let task = tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    socket.read_exact(&mut [0; 1]).await.unwrap();
                    let framing = if chunked {
                        "Transfer-Encoding: chunked"
                    } else {
                        "Content-Length: 16777217"
                    };
                    socket
                        .write_all(
                            format!("HTTP/1.1 {status} Test\r\n{framing}\r\n\r\n").as_bytes(),
                        )
                        .await
                        .unwrap();
                    if chunked {
                        let chunk = format!("10000\r\n{}\r\n", " ".repeat(65536));
                        for _ in 0..257 {
                            if socket.write_all(chunk.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                });
                let result = tokio::time::timeout(Duration::from_secs(3), async {
                    let response = Hub::http().unwrap().get(url).send().await.unwrap();
                    json::<serde_json::Value>(response).await
                })
                .await;
                task.abort();
                let error = result
                    .expect("refuse before the stalled body ends")
                    .unwrap_err();
                assert!(error.to_string().contains("16777216 byte limit"), "{error}");
            }
        }
    }
}
