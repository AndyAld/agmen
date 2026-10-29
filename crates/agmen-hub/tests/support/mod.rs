//! Shared harness for the hub's integration tests.
//!
//! Each test runs inside [`run`], which creates a fresh database on the
//! `AGMEN_IT_PG` server (ruling R26), starts a fake GitHub and an in-process
//! hub on ephemeral ports, and drops the database afterwards, even when the
//! test panics. Ids, tenants and projects are random per test, so tests run
//! in parallel and kick only the client ids they created.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the helpers"
)]

pub mod fake_paddle;
pub mod globs;
pub mod mqtt;

use std::collections::HashMap;
use std::future::Future;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agmen_hub::{
    Catalog, Config, DEFAULT_CLAIM_LEASE, EmqxConfig, GithubConfig, HostedConfig, Hub, HubMode,
    Messaging, MqttConfig, PaddleConfig, Secret,
};
use agmen_proto::Topics;
use jiff::Timestamp;
use reqwest::StatusCode;
use reqwest::redirect::Policy;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};
use tokio::task::JoinHandle;
use url::Url;

use agmen_hub::{FAKE_GITHUB_CLIENT_ID, FakeGithub};

pub const DEV_KEY_1: &str = "agmen-dev-1";
pub const DEV_KEY_2: &str = "agmen-dev-2";

/// The broker URL the test hub tells clients to use (`/v1/me`). Nothing
/// listens there: it only has to differ from the hub's own `AGMEN_MQTT_URL`.
pub const PUBLIC_MQTT_URL: &str = "wss://broker.agmen.test/mqtt";

pub fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is unset: run deploy/dev/init.sh, start the dev stack, then \
             `set -a; . deploy/dev/.secrets/dev.env; set +a`"
        )
    })
}

/// A fresh id in the hub's format: 16 of `[a-z0-9]`.
pub fn new_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut id = String::with_capacity(16);
    // RandomState is seeded randomly per instance; two draws give 16 chars.
    for _ in 0..2 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        );
        let mut bits = hasher.finish();
        for _ in 0..8 {
            id.push(char::from(ALPHABET[usize::try_from(bits % 36).unwrap()]));
            bits /= 36;
        }
    }
    id
}

/// A fresh ULID-shaped message id: 26 Crockford digits, the first 0 to 7.
pub fn ulid() -> String {
    const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut id = String::from("0");
    while id.len() < 26 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        );
        let mut bits = hasher.finish();
        for _ in 0..12 {
            if id.len() == 26 {
                break;
            }
            id.push(char::from(CROCKFORD[usize::try_from(bits % 32).unwrap()]));
            bits /= 32;
        }
    }
    id
}

/// A client envelope: `verb` with `data`, plus any `extra` top-level fields.
pub fn envelope(id: &str, verb: &str, data: Value, extra: Value) -> Vec<u8> {
    let mut env = json!({
        "v": 1, "id": id, "verb": verb, "data": data,
        "body": "hello", "sent_at": Timestamp::now().to_string(),
    });
    for (key, value) in extra.as_object().unwrap() {
        env[key] = value.clone();
    }
    serde_json::to_vec(&env).unwrap()
}

/// The broker topics of `project`.
pub fn topics(project: &Project) -> Topics {
    Topics::new(
        project.tenant.parse().unwrap(),
        project.project.parse().unwrap(),
    )
}

/// A database of its own on the `AGMEN_IT_PG` server.
#[derive(Debug)]
pub struct TestDb {
    pub name: String,
    pub url: String,
    admin: PgPool,
}

impl TestDb {
    /// An empty database: the hub's migrations have not run on it.
    pub async fn create() -> Self {
        let base = env("AGMEN_IT_PG");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&base)
            .await
            .expect("connect to AGMEN_IT_PG");
        let name = format!("agmen_it_{}", new_id());
        // The name is a fixed prefix plus [a-z0-9], so it is safe to splice
        // into DDL, which takes no bind parameters.
        sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&admin)
            .await
            .expect("create test database");
        let mut url = Url::parse(&base).expect("parse AGMEN_IT_PG");
        url.set_path(&name);
        Self {
            name,
            url: url.into(),
            admin,
        }
    }

    pub async fn drop_db(self) {
        sqlx::query(AssertSqlSafe(format!(
            "DROP DATABASE \"{}\" WITH (FORCE)",
            self.name
        )))
        .execute(&self.admin)
        .await
        .expect("drop test database");
        self.admin.close().await;
    }
}

/// Everything a test gets from [`run`].
#[derive(Debug)]
pub struct Ctx {
    pub db_url: String,
    pub pool: PgPool,
    pub github: FakeGithub,
    pub hub: TestHub,
    pub http: reqwest::Client,
}

/// Runs `body` against a fresh database, a fake GitHub and a hub signing
/// with dev key 1, then drops the database. A panic in `body` fails the
/// test after the cleanup.
pub async fn run<F, Fut>(body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    run_with_lease(DEFAULT_CLAIM_LEASE, body).await;
}

/// [`run`] with a hub whose claims last `lease` without a HEARTBEAT.
pub async fn run_with_lease<F, Fut>(lease: Duration, body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    run_configured(HubMode::SelfHost, lease, body).await;
}

/// [`run`] with a hub in the requested mode.
pub async fn run_in_mode<F, Fut>(mode: HubMode, body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    run_configured(mode, DEFAULT_CLAIM_LEASE, body).await;
}

async fn run_configured<F, Fut>(mode: HubMode, lease: Duration, body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let db = TestDb::create().await;
    let github = FakeGithub::start().await.unwrap();
    let hub =
        TestHub::start_with_mode_and_lease(&db.url, &github, DEV_KEY_1, &[], mode, lease).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db.url)
        .await
        .expect("connect to the test database");
    let ctx = Ctx {
        db_url: db.url.clone(),
        pool: pool.clone(),
        github,
        hub,
        http: http_client(),
    };
    let outcome = tokio::spawn(body(ctx)).await;
    pool.close().await;
    db.drop_db().await;
    if let Err(err) = outcome {
        std::panic::resume_unwind(err.into_panic());
    }
}

/// [`run`] against a hosted hub whose Paddle settings point at a [`fake_paddle::FakePaddle`].
/// The fake stays up until `body` returns.
pub async fn run_hosted<F, Fut>(body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    run_hosted_with_paddle(|ctx, _paddle| body(ctx)).await;
}

/// [`run_hosted`] with access to the fake Paddle signer.
pub async fn run_hosted_with_paddle<F, Fut>(body: F)
where
    F: FnOnce(Ctx, Arc<fake_paddle::FakePaddle>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let db = TestDb::create().await;
    let github = FakeGithub::start().await.unwrap();
    let paddle = Arc::new(fake_paddle::FakePaddle::start().await.unwrap());
    let hub = TestHub::start_hosted(&db.url, &github, &paddle, DEV_KEY_1, &[]).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db.url)
        .await
        .expect("connect to the test database");
    let ctx = Ctx {
        db_url: db.url.clone(),
        pool: pool.clone(),
        github,
        hub,
        http: http_client(),
    };
    let outcome = tokio::spawn(body(ctx, Arc::clone(&paddle))).await;
    pool.close().await;
    db.drop_db().await;
    drop(paddle);
    if let Err(err) = outcome {
        std::panic::resume_unwind(err.into_panic());
    }
}

/// A client that exposes redirects to the test instead of following them.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .unwrap()
}

fn jwt_key(name: &str) -> PathBuf {
    PathBuf::from(env("AGMEN_IT_JWT_DIR")).join(format!("{name}.pem"))
}

/// An in-process hub on an ephemeral port.
#[derive(Debug)]
pub struct TestHub {
    pub base: Url,
    /// The hub itself, for calls tests make without HTTP.
    pub hub: Hub,
    /// Its broker settings: an instance and share group of its own, so
    /// parallel tests never take each other's `up/` messages.
    pub mqtt: MqttConfig,
    server: JoinHandle<()>,
}

impl Drop for TestHub {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn hosted_config(paddle: &fake_paddle::FakePaddle) -> HostedConfig {
    HostedConfig {
        paddle: PaddleConfig {
            api_url: paddle.base().clone(),
            api_key: Secret::new(paddle.api_key()),
            webhook_secret: Secret::new(paddle.webhook_secret()),
        },
        catalog: Catalog {
            team_base: "pri_team_base".to_owned(),
            team_seat: "pri_team_seat".to_owned(),
            business_10: "pri_business_10".to_owned(),
            business_25: "pri_business_25".to_owned(),
            business_50: "pri_business_50".to_owned(),
        },
    }
}

impl TestHub {
    /// Starts a hub on `db_url` that signs broker tokens with `signing` and
    /// also publishes `extra` in its JWKS.
    pub async fn start(db_url: &str, github: &FakeGithub, signing: &str, extra: &[&str]) -> Self {
        Self::start_configured(db_url, github, signing, extra, |_| {}).await
    }

    /// [`TestHub::start`] with claims that last `lease`.
    pub async fn start_with_lease(
        db_url: &str,
        github: &FakeGithub,
        signing: &str,
        extra: &[&str],
        lease: Duration,
    ) -> Self {
        Self::start_configured(db_url, github, signing, extra, |config| {
            config.claim_lease = lease;
        })
        .await
    }

    /// [`TestHub::start`] in the requested hub mode.
    ///
    /// Does not fill Paddle settings. [`TestHub::start_hosted`] does.
    pub async fn start_in_mode(
        db_url: &str,
        github: &FakeGithub,
        signing: &str,
        extra: &[&str],
        mode: HubMode,
    ) -> Self {
        Self::start_configured(db_url, github, signing, extra, |config| {
            config.mode = mode;
        })
        .await
    }

    /// [`TestHub::start`] in hosted mode, with Paddle settings taken from `paddle`.
    pub async fn start_hosted(
        db_url: &str,
        github: &FakeGithub,
        paddle: &fake_paddle::FakePaddle,
        signing: &str,
        extra: &[&str],
    ) -> Self {
        let hosted = hosted_config(paddle);
        Self::start_configured(db_url, github, signing, extra, move |config| {
            config.mode = HubMode::Hosted;
            config.hosted = Some(hosted);
        })
        .await
    }

    /// Starts a hub and then applies `edit` to its config.
    pub async fn start_configured(
        db_url: &str,
        github: &FakeGithub,
        signing: &str,
        extra: &[&str],
        edit: impl FnOnce(&mut Config),
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let base = Url::parse(&format!("http://{addr}")).unwrap();
        let (host, port) = mqtt::mqtt_addr();
        let mut config = Config {
            mode: HubMode::SelfHost,
            // `start_hosted` fills Paddle settings. This default stays None,
            // including `start_in_mode(Hosted)`, whose tests never read them.
            hosted: None,
            retention_days: None,
            group_name: "agmen".to_owned(),
            database_url: Secret::new(db_url),
            listen: addr,
            public_url: base.clone(),
            github: GithubConfig {
                client_id: FAKE_GITHUB_CLIENT_ID.to_owned(),
                url: github.base().clone(),
                api_url: github.base().clone(),
            },
            emqx: EmqxConfig {
                api_url: Url::parse(&env("AGMEN_IT_EMQX_API")).unwrap(),
                api_key: env("AGMEN_IT_EMQX_API_KEY"),
            },
            mqtt: MqttConfig {
                host,
                port,
                instance: new_id(),
                share_group: new_id(),
            },
            public_mqtt_url: Url::parse(PUBLIC_MQTT_URL).unwrap(),
            mqtt_ws_upstream: std::env::var("AGMEN_IT_MQTT_WS")
                .ok()
                .map(|value| Url::parse(&value).unwrap()),
            secrets: agmen_hub::HubSecrets::Given {
                jwt_signing_key: jwt_key(signing),
                emqx_api_secret: Secret::new(env("AGMEN_IT_EMQX_API_SECRET")),
            },
            jwt_extra_keys: extra.iter().map(|name| jwt_key(name)).collect(),
            claim_lease: DEFAULT_CLAIM_LEASE,
        };
        edit(&mut config);
        let hub = Hub::connect(&config).await.expect("start the hub");
        let router = hub.router();
        let server = tokio::spawn(async move {
            agmen_hub::serve(
                listener,
                router,
                agmen_hub::HEADER_READ_TIMEOUT,
                std::future::pending(),
            )
            .await;
        });
        Self {
            base,
            hub,
            mqtt: config.mqtt,
            server,
        }
    }

    async fn start_with_mode_and_lease(
        db_url: &str,
        github: &FakeGithub,
        signing: &str,
        extra: &[&str],
        mode: HubMode,
        lease: Duration,
    ) -> Self {
        Self::start_configured(db_url, github, signing, extra, |config| {
            config.mode = mode;
            config.claim_lease = lease;
        })
        .await
    }

    /// Connects this hub to the dev EMQX and starts ingest and the
    /// sweeper. Dropping the result stops them.
    pub async fn start_messaging(&self) -> Messaging {
        self.hub
            .start_messaging(&self.mqtt)
            .await
            .expect("start messaging")
    }

    pub fn url(&self, path: &str) -> Url {
        self.base.join(path).unwrap()
    }
}

/// A tenant and project, made directly in the database (`agmen-hub
/// bootstrap` makes them outside tests).
#[derive(Debug, Clone)]
pub struct Project {
    pub tenant: String,
    pub project: String,
}

/// Names carry the id, since tenant names are unique.
pub async fn seed_project(pool: &PgPool) -> Project {
    let (tenant, project) = (new_id(), new_id());
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'tenant ' || $1)")
        .bind(&tenant)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO projects (id, tenant_id, name) VALUES ($1, $2, 'project ' || $1)")
        .bind(&project)
        .bind(&tenant)
        .execute(pool)
        .await
        .unwrap();
    Project { tenant, project }
}

/// A person who has signed in with GitHub account `sub` before.
pub async fn seed_human(pool: &PgPool, sub: u64, handle: &str) -> String {
    let id = new_id();
    sqlx::query(
        "INSERT INTO humans (id, provider, provider_sub, handle) VALUES ($1, 'github', $2, $3)",
    )
    .bind(&id)
    .bind(sub.to_string())
    .bind(handle)
    .execute(pool)
    .await
    .unwrap();
    id
}

pub async fn seed_member(pool: &PgPool, human: &str, project: &Project, role: &str) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO memberships (human_id, project_id, role) VALUES ($1, $2, $3)")
        .bind(human)
        .bind(&project.project)
        .bind(role)
        .execute(&mut *tx)
        .await
        .unwrap();
    agmen_hub::seed_tenant_member(
        &mut tx,
        &project.tenant,
        human,
        if role == "admin" {
            agmen_hub::TenantRole::Admin
        } else {
            agmen_hub::TenantRole::Member
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// What `POST /v1/auth/device` answered.
#[derive(Debug)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub body: Value,
}

pub async fn start_device_login(ctx: &Ctx) -> DeviceStart {
    let resp = ctx
        .http
        .post(ctx.hub.url("/v1/auth/device"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let field = |name: &str| body[name].as_str().unwrap().to_owned();
    DeviceStart {
        device_code: field("device_code"),
        user_code: field("user_code"),
        verification_uri: field("verification_uri"),
        body: body.clone(),
    }
}

/// Polls the token endpoint once with the device code.
pub async fn poll_device_token(ctx: &Ctx, device_code: &str) -> (StatusCode, Value) {
    let resp = ctx
        .http
        .post(ctx.hub.url("/v1/auth/token"))
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
        ])
        .send()
        .await
        .unwrap();
    (resp.status(), resp.json().await.unwrap())
}

/// Signs in through the device flow as GitHub account `(sub, login)` and
/// returns the human's access token.
pub async fn login(ctx: &Ctx, sub: u64, login: &str) -> String {
    let start = start_device_login(ctx).await;
    ctx.github.approve(&start.user_code, sub, login);
    let (status, body) = poll_device_token(ctx, &start.device_code).await;
    assert_eq!(status, StatusCode::OK, "token poll answered {body}");
    body["access_token"].as_str().unwrap().to_owned()
}

/// `POST /v1/identities` with `token`.
pub async fn mint(ctx: &Ctx, token: &str, body: &Value) -> (StatusCode, Value) {
    let resp = ctx
        .http
        .post(ctx.hub.url("/v1/identities"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// A minted identity: its id and its own hub token.
#[derive(Debug, Clone)]
pub struct Minted {
    pub id: String,
    pub token: String,
    pub body: Value,
}

pub async fn mint_ok(ctx: &Ctx, human_token: &str, handle: &str, project: &Project) -> Minted {
    mint_kind(ctx, human_token, handle, project, "agent").await
}

/// Mints an identity of `kind` in `project` with `human_token`.
pub async fn mint_kind(
    ctx: &Ctx,
    human_token: &str,
    handle: &str,
    project: &Project,
    kind: &str,
) -> Minted {
    let (status, body) = mint(
        ctx,
        human_token,
        &json!({ "handle": handle, "project": project.project, "kind": kind }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "mint answered {body}");
    Minted {
        id: body["id"].as_str().unwrap().to_owned(),
        token: body["token"].as_str().unwrap().to_owned(),
        body,
    }
}

/// `GET /v1/identities/{id}/broker-token` on `hub` with `token`.
pub async fn broker_token(
    http: &reqwest::Client,
    hub: &TestHub,
    id: &str,
    token: &str,
) -> (StatusCode, Value) {
    let resp = http
        .get(hub.url(&format!("/v1/identities/{id}/broker-token")))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// A human with a project membership and a signed-in token.
pub async fn member_with_token(
    ctx: &Ctx,
    project: &Project,
    sub: u64,
    handle: &str,
    role: &str,
) -> String {
    let human = seed_human(&ctx.pool, sub, handle).await;
    seed_member(&ctx.pool, &human, project, role).await;
    login(ctx, sub, handle).await
}

/// Lifts EMQX's ban on `username`, so a test leaves no ban behind. A ban
/// that is already gone (404) is fine.
pub async fn unban_username(http: &reqwest::Client, username: &str) {
    let mut url = Url::parse(&env("AGMEN_IT_EMQX_API")).unwrap();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend(["banned", "username", username]);
    let resp = http
        .delete(url)
        .basic_auth(
            env("AGMEN_IT_EMQX_API_KEY"),
            Some(env("AGMEN_IT_EMQX_API_SECRET")),
        )
        .send()
        .await
        .unwrap();
    assert!(
        matches!(
            resp.status(),
            StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
        ),
        "unban {username} answered {}",
        resp.status()
    );
}

/// How long [`clearing`] waits after a panic for late publishes: the last
/// wills of the dropped clients, and what the hub sends for them.
const LATE_WAIT: Duration = Duration::from_secs(2);

/// The EMQX REST API URL for `segments` under `AGMEN_IT_EMQX_API`.
pub fn emqx_api(segments: &[&str]) -> Url {
    let mut url = Url::parse(&env("AGMEN_IT_EMQX_API")).unwrap();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend(segments);
    url
}

/// Waits until EMQX no longer lists `client_id` as connected.
pub async fn gone_from_broker(http: &reqwest::Client, client_id: &str) {
    let deadline = tokio::time::Instant::now() + mqtt::ACK_WAIT;
    loop {
        let resp = http
            .get(emqx_api(&["clients", client_id]))
            .basic_auth(
                env("AGMEN_IT_EMQX_API_KEY"),
                Some(env("AGMEN_IT_EMQX_API_SECRET")),
            )
            .send()
            .await
            .unwrap();
        if resp.status() == StatusCode::NOT_FOUND {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{client_id} is still connected"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Deletes the retained messages on `retained` through the EMQX REST API.
/// One that is already gone (404) is fine.
async fn clear_retained(http: &reqwest::Client, retained: &[String]) {
    for topic in retained {
        let resp = http
            .delete(emqx_api(&["mqtt", "retainer", "message", topic]))
            .basic_auth(
                env("AGMEN_IT_EMQX_API_KEY"),
                Some(env("AGMEN_IT_EMQX_API_SECRET")),
            )
            .send()
            .await
            .unwrap();
        assert!(
            matches!(
                resp.status(),
                StatusCode::NO_CONTENT | StatusCode::OK | StatusCode::NOT_FOUND
            ),
            "clearing {topic} answered {}",
            resp.status()
        );
    }
}

/// Runs `body`, then deletes the retained messages on `retained`, also when
/// `body` panics, so the shared dev broker keeps none of them. List only
/// topics under projects the test created. A panic drops the body's clients
/// without DISCONNECT, so it first waits [`LATE_WAIT`] for their last wills.
pub async fn clearing<F, Fut>(ctx: Ctx, retained: Vec<String>, body: F)
where
    F: FnOnce(Arc<Ctx>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let ctx = Arc::new(ctx);
    let outcome = tokio::spawn(body(Arc::clone(&ctx))).await;
    if outcome.is_err() {
        tokio::time::sleep(LATE_WAIT).await;
    }
    clear_retained(&ctx.http, &retained).await;
    if let Err(err) = outcome {
        std::panic::resume_unwind(err.into_panic());
    }
}

/// Counts rows, for assertions on what the hub stored.
pub async fn count(pool: &PgPool, sql: &'static str, bind: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(bind)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The JWKS the hub serves, by kid.
pub async fn hub_jwks(http: &reqwest::Client, hub: &TestHub) -> HashMap<String, Value> {
    let jwks: Value = http
        .get(hub.url("/v1/jwks"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    jwks["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| (key["kid"].as_str().unwrap().to_owned(), key.clone()))
        .collect()
}
