//! The hub's HTTP API (axum): shared state, routes, and how errors become
//! responses.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agmen_proto::{MAX_ENVELOPE_BYTES, TenantId};
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use jsonwebtoken::jwk::JwkSet;
use serde_json::json;
use sqlx::PgPool;
use url::Url;

use crate::auth::{self, Github, GithubError};
use crate::billing::{BillingLimiter, PaddleClient, PaddleError};
use crate::broker_tokens::{BrokerTokens, TokenError};
use crate::config::{Config, HostedConfig, HubMode};
use crate::emqx::{EmqxAdmin, EmqxError};
use crate::events::Slots;
use crate::group::{self, GroupError};
use crate::ingest::IngestState;
use crate::presence::PresenceState;
use crate::self_config::{self, SelfConfigError};
use crate::{db, events, health, human_post, identities, invites, items, lanes, me, replay, state};

/// How long a client has to finish sending one HTTP request head before the
/// hub drops the connection.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the hub waits for GitHub or EMQX to answer.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);

const ADOPTED_GROUP_MESSAGE: &str = "adopted the database's only tenant as this self-host hub's group; \
     set AGMEN_HUB_MODE=hosted to keep tenants separate; \
     hosted mode also requires AGMEN_PADDLE_API_KEY, AGMEN_PADDLE_WEBHOOK_SECRET, \
     AGMEN_PADDLE_PRICE_TEAM_BASE, AGMEN_PADDLE_PRICE_TEAM_SEAT, \
     AGMEN_PADDLE_PRICE_BUSINESS_10, AGMEN_PADDLE_PRICE_BUSINESS_25 and AGMEN_PADDLE_PRICE_BUSINESS_50";

/// Why the hub could not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HubError {
    /// Loading supplied secrets or creating the self-host secrets failed.
    #[error("loading the hub secrets failed")]
    SelfConfig(#[source] SelfConfigError),
    /// Initializing the self-host group failed.
    #[error("initializing the self-host group failed")]
    Group(#[source] GroupError),
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
    /// The Paddle settings are unusable.
    #[error("the Paddle settings are unusable")]
    Paddle(#[source] PaddleError),
}

/// The running hub's shared state. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    mode: HubMode,
    broker_up: AtomicBool,
    pool: PgPool,
    tokens: BrokerTokens,
    emqx: EmqxAdmin,
    github: Github,
    paddle: Option<PaddleClient>,
    hosted: Option<HostedConfig>,
    public_url: Url,
    public_mqtt_url: Url,
    mqtt_ws_upstream: Option<Url>,
    ingest: IngestState,
    presence: PresenceState,
    events: Slots,
    claim_lease: Duration,
    retention_days: Option<u32>,
    billing_calls: Mutex<BillingLimiter>,
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
    /// agmen_hub::serve(
    ///     listener,
    ///     hub.router(),
    ///     agmen_hub::HEADER_READ_TIMEOUT,
    ///     std::future::pending(),
    /// )
    /// .await;
    /// # Ok(()) }
    /// ```
    pub async fn connect(config: &Config) -> Result<Self, HubError> {
        if config.github.client_id == crate::DEFAULT_GITHUB_CLIENT_ID {
            tracing::warn!(
                "the default GitHub client id is a placeholder; configure a device-flow OAuth app before sign-in"
            );
        }
        let loaded = self_config::load(&config.secrets, &config.emqx.api_key)
            .map_err(HubError::SelfConfig)?;
        let extra = config
            .jwt_extra_keys
            .iter()
            .map(|path| self_config::load_signing_key(path))
            .collect::<Result<Vec<_>, _>>()
            .map_err(HubError::SelfConfig)?;
        let tokens = BrokerTokens::new(loaded.signing, extra).map_err(HubError::Keys)?;
        let http = upstream_client().map_err(HubError::HttpClient)?;
        let emqx = EmqxAdmin::new(&config.emqx, loaded.emqx_api_secret, http.clone())
            .map_err(HubError::Emqx)?;
        let paddle = config
            .hosted
            .as_ref()
            .map(|hosted| PaddleClient::new(&hosted.paddle, http.clone()))
            .transpose()
            .map_err(HubError::Paddle)?;
        let github = Github::new(&config.github, http).map_err(HubError::Github)?;
        let pool = db::connect(config.database_url.expose())
            .await
            .map_err(HubError::Database)?;
        if config.mode == HubMode::SelfHost {
            let group = group::ensure_group(&pool, &config.group_name)
                .await
                .map_err(HubError::Group)?;
            if let Some(name) = &group.adopted {
                tracing::info!(
                    tenant = %group.tenant,
                    name = %name,
                    "{ADOPTED_GROUP_MESSAGE}"
                );
            }
            if let Some(token) = group::mint_claim_token(&pool)
                .await
                .map_err(HubError::Group)?
            {
                group::log_claim_token(&token);
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                mode: config.mode,
                broker_up: AtomicBool::new(false),
                pool,
                tokens,
                emqx,
                github,
                paddle,
                hosted: config.hosted.clone(),
                public_url: config.public_url.clone(),
                public_mqtt_url: config.public_mqtt_url.clone(),
                mqtt_ws_upstream: config.mqtt_ws_upstream.clone(),
                ingest: IngestState::default(),
                presence: PresenceState::default(),
                events: Slots::default(),
                claim_lease: config.claim_lease,
                retention_days: config.retention_days,
                billing_calls: Mutex::default(),
            }),
        })
    }

    /// The hub's HTTP routes: the `/v1` API plus the cluster probes
    /// `GET /livez` and `GET /healthz`, which sit outside `/v1`.
    pub fn router(&self) -> Router {
        let router = if self.inner.mode == HubMode::SelfHost {
            Router::new().route("/v1/claim", post(group::claim))
        } else {
            Router::new()
        };
        let router = match &self.inner.mqtt_ws_upstream {
            Some(upstream) => router.merge(crate::mqtt_proxy::router(
                upstream.clone(),
                crate::mqtt_proxy::MAX_PROXIED_CONNECTIONS,
            )),
            None => router,
        };
        router
            .merge(health::routes())
            .route("/i", get(crate::invite_page::page))
            .route("/v1/jwks", get(jwks))
            .merge(auth::routes())
            .route("/v1/me", get(me::me))
            .route("/v1/projects", post(crate::projects::create))
            .route("/v1/groups", post(crate::groups::create))
            .route(
                "/v1/paddle/webhook",
                post(crate::billing::webhook::receive).layer(DefaultBodyLimit::max(
                    crate::billing::webhook::MAX_BODY_BYTES,
                )),
            )
            .route("/v1/tenants/{tenant}/members", get(crate::tenants::list))
            .route(
                "/v1/tenants/{tenant}/members/{handle}",
                delete(crate::tenants::remove),
            )
            .route(
                "/v1/tenants/{tenant}/members/{handle}/role",
                axum::routing::put(crate::tenants::change_role),
            )
            .route(
                "/v1/tenants/{tenant}/transfer",
                post(crate::tenants::transfer),
            )
            .route(
                "/v1/tenants/{tenant}/seats",
                get(crate::groups::seats).put(crate::groups::set_seats),
            )
            .route(
                "/v1/tenants/{tenant}/plan",
                post(crate::groups::set_plan_route),
            )
            .route("/v1/tenants/{tenant}/portal", post(crate::groups::portal))
            .route("/v1/identities", post(identities::mint))
            .route("/v1/identities/{id}", delete(identities::revoke))
            .route(
                "/v1/identities/{id}/broker-token",
                get(identities::broker_token),
            )
            .route("/v1/invites", post(invites::create))
            .route("/v1/invites/{code}/accept", post(invites::accept))
            .route("/v1/projects/{p}/streams/{s}", get(replay::stream))
            .route(
                "/v1/projects/{project}/lanes",
                get(lanes::list).put(lanes::replace),
            )
            .route("/v1/projects/{project}/check-path", post(lanes::check_path))
            .route("/v1/projects/{project}/items", get(items::list))
            // The body is one envelope: past its limit, refuse before
            // reading the rest (axum's default would buffer 2 MB).
            .route(
                "/v1/projects/{project}/messages",
                post(human_post::post).layer(DefaultBodyLimit::max(MAX_ENVELOPE_BYTES)),
            )
            .route("/v1/projects/{project}/events", get(events::events))
            .route("/v1/projects/{project}/state", get(state::get))
            .layer(axum::middleware::map_response(refuse_framing))
            .with_state(self.clone())
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.inner.pool
    }

    pub(crate) fn mode(&self) -> HubMode {
        self.inner.mode
    }

    pub(crate) fn hosted(&self) -> Option<&HostedConfig> {
        self.inner.hosted.as_ref()
    }

    pub(crate) fn retention_days(&self) -> Option<u32> {
        self.inner.retention_days
    }

    pub(crate) fn set_broker_up(&self, up: bool) {
        self.inner.broker_up.store(up, Ordering::Relaxed);
    }

    pub(crate) fn broker_up(&self) -> bool {
        self.inner.broker_up.load(Ordering::Relaxed)
    }

    pub(crate) fn tokens(&self) -> &BrokerTokens {
        &self.inner.tokens
    }

    pub(crate) fn paddle(&self) -> Option<&PaddleClient> {
        self.inner.paddle.as_ref()
    }

    /// Takes one Paddle call from `tenant`'s bucket, or 429 `rate_limited`.
    pub(crate) fn take_billing_call(&self, tenant: &TenantId) -> Result<(), ApiError> {
        self.inner
            .billing_calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(tenant, Instant::now())
            .map_err(ApiError::rate_limited)
    }

    pub(crate) fn emqx(&self) -> &EmqxAdmin {
        &self.inner.emqx
    }

    pub(crate) fn github(&self) -> &Github {
        &self.inner.github
    }

    pub(crate) fn ingest_state(&self) -> &IngestState {
        &self.inner.ingest
    }

    pub(crate) fn presence(&self) -> &PresenceState {
        &self.inner.presence
    }

    /// The places for open events streams.
    pub(crate) fn event_slots(&self) -> &Slots {
        &self.inner.events
    }

    /// How long a granted claim lasts without a HEARTBEAT.
    pub(crate) fn claim_lease(&self) -> Duration {
        self.inner.claim_lease
    }

    /// The broker address clients connect to (`AGMEN_PUBLIC_MQTT_URL`).
    pub(crate) fn public_mqtt_url(&self) -> &Url {
        &self.inner.public_mqtt_url
    }

    /// `path` (such as `v1/auth/device`) under the public URL, keeping any
    /// path prefix the public URL has.
    pub(crate) fn public_link(&self, path: &str) -> Url {
        public_link(&self.inner.public_url, path)
    }
}

/// Serves `router` on `listener` until `shutdown` resolves, giving every
/// connection `header_read_timeout` to transmit a complete request head and
/// draining in-flight requests on shutdown.
///
/// # Examples
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let config = agmen_hub::Config::from_env()?;
/// let hub = agmen_hub::Hub::connect(&config).await?;
/// let listener = tokio::net::TcpListener::bind(config.listen).await?;
/// agmen_hub::serve(
///     listener,
///     hub.router(),
///     agmen_hub::HEADER_READ_TIMEOUT,
///     std::future::pending(),
/// )
/// .await;
/// # Ok(()) }
/// ```
pub async fn serve(
    mut listener: tokio::net::TcpListener,
    router: Router,
    header_read_timeout: Duration,
    shutdown: impl Future<Output = ()> + Send,
) {
    let mut http = hyper::server::conn::http1::Builder::new();
    http.timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(header_read_timeout);
    let (signal_tx, signal_rx) = tokio::sync::watch::channel(());
    let mut connections = tokio::task::JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);

    loop {
        tokio::select! {
            (io, _remote_addr) = axum::serve::Listener::accept(&mut listener) => {
                let http = http.clone();
                let service = hyper_util::service::TowerToHyperService::new(router.clone());
                let signal_tx = signal_tx.clone();
                connections.spawn(async move {
                    let mut conn = std::pin::pin!(
                        http.serve_connection(hyper_util::rt::TokioIo::new(io), service)
                            .with_upgrades()
                    );
                    tokio::select! {
                        result = conn.as_mut() => {
                            if let Err(err) = result {
                                tracing::trace!(error = %err, "failed to serve connection");
                            }
                            return;
                        }
                        () = signal_tx.closed() => {
                            conn.as_mut().graceful_shutdown();
                        }
                    }
                    if let Err(err) = conn.await {
                        tracing::trace!(error = %err, "failed to serve connection");
                    }
                });
            }
            Some(joined) = connections.join_next(), if !connections.is_empty() => {
                if let Err(err) = joined {
                    tracing::error!(error = %err, "connection task failed");
                }
            }
            () = &mut shutdown => {
                break;
            }
        }
    }

    drop(listener);
    drop(signal_rx);
    while let Some(joined) = connections.join_next().await {
        if let Err(err) = joined {
            tracing::error!(error = %err, "connection task failed");
        }
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
pub(super) async fn refuse_framing(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static("frame-ancestors 'none'"));
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
        retry_after: Option<Duration>,
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
    #[error("Paddle call failed")]
    Paddle(#[from] PaddleError),
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
            retry_after: None,
        }
    }

    /// 429 `rate_limited` with `Retry-After` of `wait`, rounded up to seconds.
    pub(crate) fn rate_limited(wait: Duration) -> Self {
        let secs = retry_after_secs(wait);
        Self::Client {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limited",
            description: "too many billing changes; try again later".to_owned(),
            retry_after: Some(Duration::from_secs(secs)),
        }
    }

    /// Whether this is a 429 `rate_limited` answer.
    pub(crate) fn is_rate_limited(&self) -> bool {
        matches!(
            self,
            Self::Client {
                code: "rate_limited",
                ..
            }
        )
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
        // Match a reference so the log can print this error before the fields move.
        let (status, code, description, retry_after) = match &self {
            Self::Client {
                status,
                code,
                description,
                retry_after,
            } => (*status, *code, description.clone(), *retry_after),
            Self::Paddle(PaddleError::BadUrl { .. }) => (
                StatusCode::BAD_GATEWAY,
                "paddle_bad_url",
                "Paddle returned a URL the hub will not send".to_owned(),
                None,
            ),
            Self::Emqx(_) | Self::Github(_) | Self::Paddle(_) => {
                tracing::error!(error = %error_chain(&self), "upstream call failed");
                (
                    StatusCode::BAD_GATEWAY,
                    "upstream_unavailable",
                    self.to_string(),
                    None,
                )
            }
            Self::Database(_) | Self::Random(_) | Self::Stored(_) | Self::Token(_) => {
                tracing::error!(error = %error_chain(&self), "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "server_error",
                    "internal error".to_owned(),
                    None,
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
        if status == StatusCode::TOO_MANY_REQUESTS
            && let Some(wait) = retry_after
            && let Ok(value) = HeaderValue::from_str(&wait.as_secs().max(1).to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

/// `wait` in whole seconds, rounded up, at least 1.
fn retry_after_secs(wait: Duration) -> u64 {
    u64::try_from(wait.as_millis().div_ceil(1000))
        .unwrap_or(u64::MAX)
        .max(1)
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
#[path = "http_tests.rs"]
mod tests;
