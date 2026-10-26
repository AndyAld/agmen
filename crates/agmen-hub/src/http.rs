//! The hub's HTTP API (axum): shared state, routes, and how errors become
//! responses.

use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use jsonwebtoken::jwk::JwkSet;
use serde_json::json;
use sqlx::PgPool;
use url::Url;

use crate::auth::{self, GithubError, GithubOAuth};
use crate::broker_tokens::{BrokerTokens, SigningKey, TokenError};
use crate::config::Config;
use crate::emqx::{EmqxAdmin, EmqxError};
use crate::ingest::IngestState;
use crate::{db, health, identities, invites};

/// How long the hub waits for GitHub or EMQX to answer.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);

/// Why the hub could not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HubError {
    /// Connecting to Postgres or running the migrations failed.
    #[error("connecting to the database failed")]
    Database(#[source] sqlx::Error),
    /// A broker token signing key could not be loaded.
    #[error("loading the broker token keys failed")]
    Keys(#[source] TokenError),
    /// The HTTP client for GitHub and EMQX could not be built.
    #[error("building the HTTP client failed")]
    HttpClient(#[source] reqwest::Error),
    /// The EMQX API settings are unusable.
    #[error("the EMQX API settings are unusable")]
    Emqx(#[source] EmqxError),
    /// The GitHub settings are unusable.
    #[error("the GitHub settings are unusable")]
    Github(#[source] GithubError),
}

/// The running hub's shared state. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    pool: PgPool,
    tokens: BrokerTokens,
    emqx: EmqxAdmin,
    github: GithubOAuth,
    public_url: Url,
    ingest: IngestState,
}

impl Hub {
    /// Connects to the database (running the migrations), loads the
    /// signing keys and builds the GitHub and EMQX clients.
    ///
    /// # Errors
    ///
    /// [`HubError`] naming the step that failed.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let config = agmen_hub::Config::from_env()?;
    /// let hub = agmen_hub::Hub::connect(&config).await?;
    /// let listener = tokio::net::TcpListener::bind(config.listen).await?;
    /// axum::serve(listener, hub.router()).await?;
    /// # Ok(()) }
    /// ```
    pub async fn connect(config: &Config) -> Result<Self, HubError> {
        let signing = SigningKey::from_pem_file(&config.jwt_signing_key).map_err(HubError::Keys)?;
        let extra = config
            .jwt_extra_keys
            .iter()
            .map(SigningKey::from_pem_file)
            .collect::<Result<Vec<_>, _>>()
            .map_err(HubError::Keys)?;
        let tokens = BrokerTokens::new(signing, extra).map_err(HubError::Keys)?;
        let http = upstream_client().map_err(HubError::HttpClient)?;
        let emqx = EmqxAdmin::new(&config.emqx, http.clone()).map_err(HubError::Emqx)?;
        let github =
            GithubOAuth::new(&config.github, &config.public_url, http).map_err(HubError::Github)?;
        let pool = db::connect(config.database_url.expose())
            .await
            .map_err(HubError::Database)?;
        Ok(Self {
            inner: Arc::new(Inner {
                pool,
                tokens,
                emqx,
                github,
                public_url: config.public_url.clone(),
                ingest: IngestState::default(),
            }),
        })
    }

    /// The `/v1` routes.
    pub fn router(&self) -> Router {
        Router::new()
            .merge(health::routes())
            .route("/v1/jwks", get(jwks))
            .merge(auth::routes())
            .route("/v1/identities", post(identities::mint))
            .route("/v1/identities/{id}", delete(identities::revoke))
            .route(
                "/v1/identities/{id}/broker-token",
                get(identities::broker_token),
            )
            .route("/v1/invites", post(invites::create))
            .route("/v1/invites/{code}/accept", post(invites::accept))
            .layer(axum::middleware::map_response(refuse_framing))
            .with_state(self.clone())
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.inner.pool
    }

    pub(crate) fn tokens(&self) -> &BrokerTokens {
        &self.inner.tokens
    }

    pub(crate) fn emqx(&self) -> &EmqxAdmin {
        &self.inner.emqx
    }

    pub(crate) fn github(&self) -> &GithubOAuth {
        &self.inner.github
    }

    pub(crate) fn ingest_state(&self) -> &IngestState {
        &self.inner.ingest
    }

    /// `path` (such as `v1/auth/device`) under the public URL, keeping any
    /// path prefix the public URL has.
    pub(crate) fn public_link(&self, path: &str) -> Url {
        public_link(&self.inner.public_url, path)
    }
}

/// The HTTP client for GitHub and EMQX. No redirects: OAuth token exchanges
/// must not follow them, and EMQX never sends them. GitHub's API refuses
/// requests without a User-Agent.
pub(crate) fn upstream_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(UPSTREAM_TIMEOUT)
        .user_agent(concat!("agmen-hub/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// `path` appended to `base`'s path.
pub(crate) fn public_link(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty().extend(path.split('/'));
    }
    url
}

/// Forbids every browser from showing a hub response inside a frame, so no
/// other site can overlay the approve button and trick a click (ruling
/// R38). Set on every response, so a page added later cannot miss it.
async fn refuse_framing(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    response
}

/// `GET /v1/jwks`: the public keys EMQX verifies broker tokens with.
async fn jwks(axum::extract::State(hub): axum::extract::State<Hub>) -> Json<JwkSet> {
    Json(hub.tokens().jwks())
}

/// An error a handler answers with. Client errors carry an RFC 6749 style
/// `{error, error_description}` body; upstream and internal errors are
/// logged and answered without detail.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
    /// The caller's request is refused.
    #[error("{description}")]
    Client {
        status: StatusCode,
        code: &'static str,
        description: String,
    },
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("the operating system's random source failed")]
    Random(#[from] getrandom::Error),
    #[error("a stored value is invalid")]
    Stored(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("minting a broker token failed")]
    Token(#[from] TokenError),
    #[error("EMQX call failed")]
    Emqx(#[from] EmqxError),
    #[error("GitHub call failed")]
    Github(#[from] GithubError),
}

impl ApiError {
    pub(crate) fn new(
        status: StatusCode,
        code: &'static str,
        description: impl Into<String>,
    ) -> Self {
        Self::Client {
            status,
            code,
            description: description.into(),
        }
    }

    pub(crate) fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "invalid_token",
            "a valid bearer token is required",
        )
    }

    pub(crate) fn forbidden(code: &'static str, description: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, description)
    }

    pub(crate) fn not_found(description: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", description)
    }

    pub(crate) fn conflict(code: &'static str, description: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, description)
    }

    /// An OAuth token endpoint error (RFC 6749 section 5.2, RFC 8628
    /// section 3.5): always 400.
    pub(crate) fn grant(code: &'static str, description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, description)
    }

    /// A stored value that the schema should have refused.
    pub(crate) fn stored(err: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Stored(Box::new(err))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, description) = match self {
            Self::Client {
                status,
                code,
                description,
            } => (status, code, description),
            Self::Emqx(_) | Self::Github(_) => {
                tracing::error!(error = %error_chain(&self), "upstream call failed");
                (
                    StatusCode::BAD_GATEWAY,
                    "upstream_unavailable",
                    self.to_string(),
                )
            }
            Self::Database(_) | Self::Random(_) | Self::Stored(_) | Self::Token(_) => {
                tracing::error!(error = %error_chain(&self), "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    "internal error".to_owned(),
                )
            }
        };
        let mut response = (
            status,
            Json(json!({ "error": code, "error_description": description })),
        )
            .into_response();
        if status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

/// `err` and up to 16 of its sources, joined with ": ".
pub(crate) fn error_chain(err: &dyn std::error::Error) -> String {
    std::iter::successors(Some(err), |e| e.source())
        .take(17)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_link_keeps_a_path_prefix() {
        let root = Url::parse("https://api.example.com").unwrap();
        assert_eq!(
            public_link(&root, "v1/auth/device").as_str(),
            "https://api.example.com/v1/auth/device"
        );
        let prefixed = Url::parse("https://example.com/hub/").unwrap();
        assert_eq!(
            public_link(&prefixed, "v1/jwks").as_str(),
            "https://example.com/hub/v1/jwks"
        );
    }
}
