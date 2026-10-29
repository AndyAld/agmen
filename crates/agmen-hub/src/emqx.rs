//! The EMQX REST calls the hub makes (ADR-0002, "REST kick" and ruling
//! R36's username ban).

use agmen_proto::Timestamp;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::{EmqxConfig, Secret};

/// Why an EMQX REST call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EmqxError {
    /// The configured API URL cannot take path segments.
    #[error("EMQX API URL {0} cannot be a base URL")]
    BadApiUrl(String),
    /// The request did not complete.
    #[error("EMQX request to {call} failed")]
    Request {
        /// The call, such as `kick idn7k2q9abcd-mcp` or `ban username idn7k2q9abcd`.
        call: String,
        /// The transport error.
        #[source]
        source: reqwest::Error,
    },
    /// EMQX answered with a status the hub does not expect.
    #[error("EMQX answered {status} to {call}")]
    Status {
        /// The call, such as `kick idn7k2q9abcd-mcp` or `ban username idn7k2q9abcd`.
        call: String,
        /// The HTTP status.
        status: u16,
    },
}

/// A client for the EMQX REST API, authenticated with an API key.
#[derive(Debug, Clone)]
pub struct EmqxAdmin {
    http: reqwest::Client,
    api_url: Url,
    api_key: String,
    api_secret: Secret,
}

/// `POST /banned` body (EMQX's `emqx_mgmt_api_banned.ban` schema).
#[derive(Debug, Serialize)]
struct Ban<'a> {
    #[serde(rename = "as")]
    kind: &'static str,
    who: &'a str,
    by: &'static str,
    reason: &'static str,
    until: Timestamp,
}

/// The `code` of an EMQX error body.
#[derive(Debug, Deserialize)]
struct ErrorBody {
    code: String,
}

impl EmqxAdmin {
    /// A client for the API at `config.api_url` that sends requests
    /// through `http`, authenticated with `api_secret`.
    ///
    /// # Errors
    ///
    /// [`EmqxError::BadApiUrl`] when the URL cannot take path segments.
    pub fn new(
        config: &EmqxConfig,
        api_secret: Secret,
        http: reqwest::Client,
    ) -> Result<Self, EmqxError> {
        if config.api_url.cannot_be_a_base() {
            return Err(EmqxError::BadApiUrl(config.api_url.to_string()));
        }
        Ok(Self {
            http,
            api_url: config.api_url.clone(),
            api_key: config.api_key.clone(),
            api_secret,
        })
    }

    /// `segments` appended to the API URL. Pushing a segment percent-encodes
    /// it, so a client id or username can never reach another API path.
    fn endpoint(&self, segments: &[&str]) -> Result<Url, EmqxError> {
        let mut url = self.api_url.clone();
        url.path_segments_mut()
            .map_err(|()| EmqxError::BadApiUrl(self.api_url.to_string()))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    /// Disconnects `client_id` (`DELETE /clients/{client_id}`). The client
    /// gets DISCONNECT 0x98. A client that is not connected (404) counts as
    /// kicked.
    ///
    /// # Errors
    ///
    /// [`EmqxError::Request`] when EMQX cannot be reached, and
    /// [`EmqxError::Status`] for any answer but 204 or 404.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(emqx: agmen_hub::EmqxAdmin) -> Result<(), agmen_hub::EmqxError> {
    /// emqx.kick("idn7k2q9abcd-mcp").await?;
    /// # Ok(()) }
    /// ```
    pub async fn kick(&self, client_id: &str) -> Result<(), EmqxError> {
        let call = || format!("kick {client_id}");
        let response = self
            .http
            .delete(self.endpoint(&["clients", client_id])?)
            .basic_auth(&self.api_key, Some(self.api_secret.expose()))
            .send()
            .await
            .map_err(|source| EmqxError::Request {
                call: call(),
                source,
            })?;
        match response.status() {
            reqwest::StatusCode::NO_CONTENT | reqwest::StatusCode::NOT_FOUND => Ok(()),
            status => Err(EmqxError::Status {
                call: call(),
                status: status.as_u16(),
            }),
        }
    }

    /// Bans `username` until `until` (`POST /banned`, `as: username`), so
    /// EMQX refuses its CONNECT (CONNACK 0x8A) even with an unexpired token.
    /// A ban that already exists counts as success, so the call can be
    /// repeated.
    ///
    /// # Errors
    ///
    /// [`EmqxError::Request`] when EMQX cannot be reached, and
    /// [`EmqxError::Status`] for any answer but 200, or 400 with code
    /// `ALREADY_EXISTS`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(emqx: agmen_hub::EmqxAdmin) -> Result<(), Box<dyn std::error::Error>> {
    /// let until = agmen_proto::Timestamp::now() + std::time::Duration::from_secs(3900);
    /// emqx.ban_username("idn7k2q9abcd", until).await?;
    /// # Ok(()) }
    /// ```
    pub async fn ban_username(&self, username: &str, until: Timestamp) -> Result<(), EmqxError> {
        let call = || format!("ban username {username}");
        let response = self
            .http
            .post(self.endpoint(&["banned"])?)
            .basic_auth(&self.api_key, Some(self.api_secret.expose()))
            .json(&Ban {
                kind: "username",
                who: username,
                by: "agmen-hub",
                reason: "identity revoked",
                until,
            })
            .send()
            .await
            .map_err(|source| EmqxError::Request {
                call: call(),
                source,
            })?;
        let status = response.status();
        if status == reqwest::StatusCode::OK {
            return Ok(());
        }
        if status == reqwest::StatusCode::BAD_REQUEST {
            // A body that is not EMQX's error shape is just an unexpected 400.
            let body = response.json::<ErrorBody>().await.ok();
            if body.is_some_and(|body| body.code == "ALREADY_EXISTS") {
                return Ok(());
            }
        }
        Err(EmqxError::Status {
            call: call(),
            status: status.as_u16(),
        })
    }
}
