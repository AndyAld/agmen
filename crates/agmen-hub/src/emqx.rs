//! The EMQX REST calls the hub makes (ADR-0002, "REST kick").

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
    #[error("EMQX request for client {client_id} failed")]
    Request {
        /// The client id the call was about.
        client_id: String,
        /// The transport error.
        #[source]
        source: reqwest::Error,
    },
    /// EMQX answered with a status the hub does not expect.
    #[error("EMQX answered {status} for client {client_id}")]
    Status {
        /// The client id the call was about.
        client_id: String,
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

impl EmqxAdmin {
    /// A client for the API at `config.api_url` that sends requests
    /// through `http`.
    ///
    /// # Errors
    ///
    /// [`EmqxError::BadApiUrl`] when the URL cannot take path segments.
    pub fn new(config: &EmqxConfig, http: reqwest::Client) -> Result<Self, EmqxError> {
        if config.api_url.cannot_be_a_base() {
            return Err(EmqxError::BadApiUrl(config.api_url.to_string()));
        }
        Ok(Self {
            http,
            api_url: config.api_url.clone(),
            api_key: config.api_key.clone(),
            api_secret: config.api_secret.clone(),
        })
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
        let mut url = self.api_url.clone();
        // Pushing a segment percent-encodes it, so a client id can never
        // reach another API path.
        url.path_segments_mut()
            .map_err(|()| EmqxError::BadApiUrl(self.api_url.to_string()))?
            .pop_if_empty()
            .push("clients")
            .push(client_id);
        let response = self
            .http
            .delete(url)
            .basic_auth(&self.api_key, Some(self.api_secret.expose()))
            .send()
            .await
            .map_err(|source| EmqxError::Request {
                client_id: client_id.to_owned(),
                source,
            })?;
        match response.status() {
            reqwest::StatusCode::NO_CONTENT | reqwest::StatusCode::NOT_FOUND => Ok(()),
            status => Err(EmqxError::Status {
                client_id: client_id.to_owned(),
                status: status.as_u16(),
            }),
        }
    }
}
