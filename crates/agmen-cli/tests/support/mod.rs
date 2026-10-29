//! Harness for the CLI's integration tests: a hub in this process on a
//! database of its own (ruling R26), connected to the dev EMQX, with a fresh
//! project; temp homes and git worktrees; and the `agmen` binary run with
//! the temp home, so nothing touches the user's keychain or real config
//! (ruling R17).
//!
//! Ids and projects are random per test, so tests run in parallel. Each
//! test deletes the retained messages left for its own project (the
//! `state/` topics its hub published and each identity's `presence/{id}`)
//! and lifts the EMQX ban on each identity it revoked, even when it fails.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the helpers"
)]

pub mod mcp;

use std::future::Future;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agmen_client::{IdentityFile, TokenKey, TokenStore};
use agmen_hub::{Config, Hub, Messaging, MqttConfig, Role, SeededHuman};
use agmen_proto::{IdentityId, Timestamp, Topics};
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use url::Url;

pub fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is unset: run deploy/dev/init.sh, start the dev stack, then \
             `set -a; . deploy/dev/.secrets/dev.env; set +a`"
        )
    })
}

/// `count` random characters from `alphabet`.
fn random(alphabet: &[u8], count: usize) -> String {
    let mut out = String::with_capacity(count);
    while out.len() < count {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        );
        let mut bits = hasher.finish();
        for _ in 0..8 {
            if out.len() == count {
                break;
            }
            let len = u64::try_from(alphabet.len()).unwrap();
            out.push(char::from(alphabet[usize::try_from(bits % len).unwrap()]));
            bits /= len;
        }
    }
    out
}

/// A fresh id in the hub's format.
pub fn new_id() -> String {
    random(b"abcdefghijklmnopqrstuvwxyz0123456789", 16)
}

/// A database of its own on the `AGMEN_IT_PG` server.
struct TestDb {
    name: String,
    url: String,
    admin: PgPool,
}

impl TestDb {
    async fn create() -> Self {
        let base = env("AGMEN_IT_PG");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&base)
            .await
            .expect("connect to AGMEN_IT_PG");
        let name = format!("agmen_it_cli_{}", new_id());
        // The name is a fixed prefix plus [a-z0-9], safe to splice into DDL.
        sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&admin)
            .await
            .expect("create test database");
        let mut url = Url::parse(&base).unwrap();
        url.set_path(&name);
        Self {
            name,
            url: url.into(),
            admin,
        }
    }

    async fn drop_db(self) {
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

/// Whether the test's hub ingests what clients publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingest {
    /// The hub reads `up/` and answers every publish.
    On,
    /// The hub serves HTTP only: nothing is ever acked.
    Off,
}

/// Everything a test gets from [`run`].
#[derive(Debug)]
pub struct Ctx {
    pub hub: Hub,
    pub github: agmen_hub::FakeGithub,
    pub db: PgPool,
    /// The hub's base URL.
    pub base: Url,
    pub topics: Topics,
    messaging: Option<Messaging>,
    mqtt: MqttConfig,
    server: JoinHandle<()>,
}

impl Ctx {
    /// Starts or stops the hub's messaging. Stopped, it reads nothing from
    /// the broker, so a client's publish stays in its outbox.
    pub async fn set_ingest(&mut self, ingest: Ingest) {
        // Stopped first: two connections with one client id take over
        // each other's session.
        self.messaging = None;
        self.messaging = match ingest {
            Ingest::On => Some(
                self.hub
                    .start_messaging(&self.mqtt)
                    .await
                    .expect("start messaging"),
            ),
            Ingest::Off => None,
        };
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Runs `body` against a fresh database and an in-process hub with a fresh
/// project, then deletes the project's retained state and drops the
/// database, even when `body` panics.
pub async fn run<F, Fut>(ingest: Ingest, body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    run_in_mode(agmen_hub::HubMode::SelfHost, ingest, body).await;
}

/// Runs the same isolated harness as [`run`] with hosted group creation enabled.
pub async fn run_hosted<F, Fut>(ingest: Ingest, body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    run_in_mode(agmen_hub::HubMode::Hosted, ingest, body).await;
}

async fn run_in_mode<F, Fut>(mode: agmen_hub::HubMode, ingest: Ingest, body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let db = TestDb::create().await;
    let github = agmen_hub::FakeGithub::start().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = Url::parse(&format!("http://{addr}")).unwrap();
    let (instance, share_group) = (new_id(), new_id());
    let signing_key = PathBuf::from(env("AGMEN_IT_JWT_DIR"))
        .join("agmen-dev-1.pem")
        .to_str()
        .unwrap()
        .to_owned();
    // Built through the hub's own variable parser, so a setting the hub adds
    // later takes its default here instead of breaking this harness.
    let mut config = Config::from_lookup(|name| {
        Some(match name {
            "AGMEN_DATABASE_URL" => db.url.clone(),
            "AGMEN_LISTEN" => addr.to_string(),
            "AGMEN_PUBLIC_URL" => base.to_string(),
            "AGMEN_GITHUB_CLIENT_ID" => agmen_hub::FAKE_GITHUB_CLIENT_ID.to_owned(),
            "AGMEN_GITHUB_URL" | "AGMEN_GITHUB_API_URL" => github.base().to_string(),
            "AGMEN_EMQX_API_URL" => env("AGMEN_IT_EMQX_API"),
            "AGMEN_EMQX_API_KEY" => env("AGMEN_IT_EMQX_API_KEY"),
            "AGMEN_EMQX_API_SECRET" => env("AGMEN_IT_EMQX_API_SECRET"),
            "AGMEN_MQTT_URL" => env("AGMEN_IT_MQTT"),
            "AGMEN_HUB_INSTANCE" => instance.clone(),
            "AGMEN_MQTT_SHARE_GROUP" => share_group.clone(),
            "AGMEN_JWT_SIGNING_KEY" => signing_key.clone(),
            _ => return None,
        })
    })
    .expect("build the hub config");
    config.mode = mode;
    let hub = Hub::connect(&config).await.expect("start the hub");
    let router = hub.router();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let topics = hub.seed_project().await.expect("seed a project");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db.url)
        .await
        .expect("connect to the test database");
    let mut ctx = Ctx {
        hub,
        github,
        db: pool.clone(),
        base,
        topics: topics.clone(),
        messaging: None,
        mqtt: config.mqtt.clone(),
        server,
    };
    ctx.set_ingest(ingest).await;
    // The body owns the hub's messaging, so the hub publishes nothing once
    // it has ended.
    let outcome = tokio::spawn(body(ctx)).await;
    pool.close().await;
    clean_up(&db.url, &topics).await;
    db.drop_db().await;
    if let Err(err) = outcome {
        std::panic::resume_unwind(err.into_panic());
    }
}

/// Deletes the retained `state/roster`, `state/head` and each identity's
/// `presence/{id}` in `topics`' project, and lifts the ban on each revoked
/// identity, as the test's database lists them.
async fn clean_up(db_url: &str, topics: &Topics) {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(db_url)
        .await
        .expect("connect to the test database");
    let identities: Vec<(String, bool)> =
        sqlx::query_as("SELECT id, revoked_at IS NOT NULL FROM identities WHERE project_id = $1")
            .bind(topics.project().as_str())
            .fetch_all(&pool)
            .await
            .unwrap();
    pool.close().await;
    let mut retained = vec![topics.state("roster"), topics.state_head()];
    for (id, revoked) in &identities {
        // A watch killed without DISCONNECT leaves its last will to the
        // broker; wait until it has gone, so the will comes before the
        // deletion.
        for suffix in ["watch", "mcp"] {
            disconnected(&format!("{id}-{suffix}")).await;
        }
        retained.push(topics.presence(&id.parse().unwrap()));
        if *revoked {
            emqx_delete(&["banned", "username", id]).await;
        }
    }
    for topic in &retained {
        emqx_delete(&["mqtt", "retainer", "message", topic]).await;
    }
}

/// The EMQX REST API URL for `segments` under `AGMEN_IT_EMQX_API`.
fn emqx_api(segments: &[&str]) -> Url {
    let mut url = Url::parse(&env("AGMEN_IT_EMQX_API")).unwrap();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend(segments);
    url
}

/// What EMQX lists for `client_id`: `None` when it lists nothing.
pub async fn emqx_client(client_id: &str) -> Option<serde_json::Value> {
    let resp = reqwest::Client::new()
        .get(emqx_api(&["clients", client_id]))
        .basic_auth(
            env("AGMEN_IT_EMQX_API_KEY"),
            Some(env("AGMEN_IT_EMQX_API_SECRET")),
        )
        .send()
        .await
        .unwrap();
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return None;
    }
    assert!(resp.status().is_success(), "{client_id}: {}", resp.status());
    Some(resp.json().await.unwrap())
}

/// Waits up to 10 s until EMQX lists `client_id` as not connected (or not
/// at all), checking every 100 ms.
async fn disconnected(client_id: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match emqx_client(client_id).await {
            Some(client) if client["connected"] != false => {}
            _ => return,
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{client_id} is still connected"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Has EMQX drop `client_id`'s connection, as the hub's kick does.
pub async fn kick(client_id: &str) {
    let resp = reqwest::Client::new()
        .delete(emqx_api(&["clients", client_id]))
        .basic_auth(
            env("AGMEN_IT_EMQX_API_KEY"),
            Some(env("AGMEN_IT_EMQX_API_SECRET")),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NO_CONTENT,
        "kicking {client_id}"
    );
}

/// `DELETE` on the EMQX REST API at `segments`. Nothing there (404) is
/// fine.
async fn emqx_delete(segments: &[&str]) {
    let resp = reqwest::Client::new()
        .delete(emqx_api(segments))
        .basic_auth(
            env("AGMEN_IT_EMQX_API_KEY"),
            Some(env("AGMEN_IT_EMQX_API_SECRET")),
        )
        .send()
        .await
        .unwrap();
    assert!(
        matches!(resp.status().as_u16(), 200 | 204 | 404),
        "deleting {} answered {}",
        segments.join("/"),
        resp.status()
    );
}

/// A temp home for one person's `agmen`: its config, state and token store.
#[derive(Debug)]
pub struct Home {
    dir: TempDir,
}

impl Home {
    pub fn new() -> Self {
        Self::from_dir(tempfile::tempdir().unwrap())
    }

    /// A home under a short base, for Unix socket path limits.
    pub fn new_in(base: &Path) -> Self {
        Self::from_dir(
            tempfile::Builder::new()
                .prefix("agw-")
                .tempdir_in(base)
                .unwrap(),
        )
    }

    fn from_dir(dir: TempDir) -> Self {
        let home = Self { dir };
        #[cfg(unix)]
        {
            for dir in ["/usr/bin", "/bin"] {
                for agent in [
                    "claude",
                    "codex",
                    "opencode",
                    "cursor",
                    "cursor-agent",
                    "gemini",
                ] {
                    assert!(
                        !Path::new(dir).join(agent).exists(),
                        "an agent program is installed in /usr/bin or /bin; the setup tests cannot hide it"
                    );
                }
            }
            let git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join("git"))
                .find(|path| {
                    use std::os::unix::fs::PermissionsExt as _;
                    std::fs::metadata(path).is_ok_and(|metadata| {
                        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                    })
                })
                .expect("git is on the test PATH");
            let bin = home.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            std::os::unix::fs::symlink(std::fs::canonicalize(git).unwrap(), bin.join("git"))
                .unwrap();
        }
        home
    }

    /// A fake executable that records its arguments inside this temp home.
    #[cfg(unix)]
    pub fn fake_program(&self, name: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );
        let path = self.path().join("bin").join(name);
        let text = format!(
            "#!/bin/sh\nmkdir -p \"$HOME/calls\"\nprintf '%s\\n' \"$*\" >> \"$HOME/calls/{name}.log\"\n{script}\n"
        );
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The folder that holds each identity's state folder, as the CLI
    /// places it with this home.
    pub fn state_base(&self) -> PathBuf {
        let base = if cfg!(target_os = "macos") {
            self.path().join("Library/Application Support")
        } else {
            self.path().join("state")
        };
        base.join("agmen")
    }

    /// The token store `agmen` uses with this home.
    pub fn tokens(&self) -> TokenStore {
        TokenStore::Dir(self.path().join("tokens"))
    }

    /// Where `agmen login` keeps who is signed in, as the CLI places it.
    pub fn login_file(&self) -> PathBuf {
        let base = if cfg!(target_os = "macos") {
            self.path().join("Library/Application Support")
        } else {
            self.path().join("config")
        };
        base.join("agmen").join("login.json")
    }

    /// `agmen <args>` in `cwd` with this home, and nothing of the user's
    /// own config, keychain or git settings.
    pub fn agmen(&self, cwd: &Path, args: &[&str]) -> Command {
        // Cargo tells a package's tests only where that package's own
        // binaries are: agmen-cli's tests run its `agmen`, agmen-e2e's the
        // same command line built there as `agmen-e2e`.
        let bin = option_env!("CARGO_BIN_EXE_agmen")
            .or(option_env!("CARGO_BIN_EXE_agmen-e2e"))
            .expect("cargo builds this test with an agmen binary");
        let mut cmd = Command::new(bin);
        cmd.args(args).current_dir(cwd);
        self.apply_env(&mut cmd);
        cmd
    }

    /// Sets the variables that point a process at this home.
    pub fn apply_env(&self, cmd: &mut Command) {
        let home = self.path();
        cmd.env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env(TokenStore::DIR_VAR, home.join("tokens"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", home.join("bin").display()),
            )
            .env_remove("CODEX_HOME")
            .env_remove("AGMEN_HUB")
            .env_remove("AGMEN_LOG");
    }

    /// Runs `agmen <args>` in `cwd` off the async runtime and returns its
    /// output.
    pub async fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut cmd = self.agmen(cwd, args);
        tokio::task::spawn_blocking(move || cmd.output().unwrap())
            .await
            .unwrap()
    }

    /// Signs `human` in at the hub at `base` as `agmen login` leaves it:
    /// who is signed in, and their tokens in the token store.
    pub fn sign_in(&self, base: &Url, human: &SeededHuman) {
        let file = self.login_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let login = serde_json::json!({
            "hub_url": base,
            "human_id": human.id.as_str(),
            "handle": human.handle,
        });
        std::fs::write(&file, serde_json::to_vec(&login).unwrap()).unwrap();
        let id: IdentityId = human.id.as_str().parse().unwrap();
        let tokens = serde_json::json!({
            "access_token": human.access_token,
            "refresh_token": human.refresh_token,
            "expires_at": Timestamp::now() + jiff::SignedDuration::from_secs(3000),
        });
        self.tokens()
            .set(&TokenKey::new(base, &id).unwrap(), &tokens.to_string())
            .unwrap();
    }
}

/// A git repository with one commit, as a worktree to join.
#[derive(Debug)]
pub struct Worktree {
    dir: TempDir,
}

impl Worktree {
    pub fn new(home: &Home) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let wt = Self { dir };
        wt.git(home, &["init", "-q", "-b", "main"]);
        std::fs::write(wt.path().join("README.md"), "hello\n").unwrap();
        wt.git(home, &["add", "README.md"]);
        wt.git(home, &["commit", "-q", "-m", "first"]);
        wt
    }

    /// The worktree root, every link followed, as git names it.
    pub fn path(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    /// Runs `git <args>` here with `home`'s settings and returns stdout.
    pub fn git(&self, home: &Home, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(self.dir.path());
        home.apply_env(&mut cmd);
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// The identity file `agmen join` wrote.
    pub fn identity(&self) -> IdentityFile {
        let raw = std::fs::read(self.path().join(".agmen/identity.json")).unwrap();
        serde_json::from_slice(&raw).unwrap()
    }
}

/// stdout as text.
pub fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

/// stderr as text.
pub fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

/// Asserts that `out` exited with `code`, showing its output otherwise.
#[track_caller]
pub fn assert_exit(out: &Output, code: i32) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        stdout(out),
        stderr(out)
    );
}

/// A person with `role` in the test's project, signed in in a fresh home,
/// and a worktree they joined as `<handle>/<name>`.
#[derive(Debug)]
pub struct Member {
    pub human: SeededHuman,
    pub home: Home,
    pub wt: Worktree,
    pub identity: IdentityFile,
}

impl Ctx {
    /// A person with `role`, signed in, who has joined a fresh worktree as
    /// `<handle>/<name>` with `kind` (`agent` or `coordinator`).
    pub async fn member(&self, role: Role, name: &str, kind: &str) -> Member {
        let human = self.hub.seed_human(&self.topics, role).await.unwrap();
        let home = Home::new();
        home.sign_in(&self.base, &human);
        let wt = Worktree::new(&home);
        let project = self.topics.project().as_str();
        let out = home
            .run(
                &wt.path(),
                &[
                    "join",
                    "--project",
                    project,
                    "--handle",
                    name,
                    "--kind",
                    kind,
                ],
            )
            .await;
        assert_exit(&out, 0);
        let identity = wt.identity();
        Member {
            human,
            home,
            wt,
            identity,
        }
    }
}

impl Member {
    /// Runs `agmen <args>` in this member's worktree.
    pub async fn run(&self, args: &[&str]) -> Output {
        self.home.run(&self.wt.path(), args).await
    }
}

/// How long a test waits for something the hub does in the background.
pub const WAIT: Duration = Duration::from_secs(20);
