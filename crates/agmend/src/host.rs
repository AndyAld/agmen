//! Optional daemon-owned hub hosting. Configuration contains paths, never credentials.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::sync::Notify;
use tokio::time::Instant;

const WINDOW: Duration = Duration::from_secs(600);
const MAX_RESTARTS: usize = 5;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ENV: u64 = 1024 * 1024;

/// Paths and public settings saved by `agmend host init`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Hub executable. Omission selects the helper beside a bundled agmend.
    pub hub_bin: Option<PathBuf>,
    /// First shell environment file.
    pub env_file: PathBuf,
    /// Additional environment files, applied in order.
    #[serde(default)]
    pub extra_env_files: Vec<PathBuf>,
    /// Public HTTPS origin used by the hub.
    pub public_url: String,
    /// Configure Tailscale's HTTPS 443 listener once at supervisor startup.
    #[serde(default)]
    pub tailscale_serve: bool,
}

impl HostConfig {
    /// Reads a regular private configuration file. Group and world access is refused.
    ///
    /// # Errors
    /// Returns an error for unsafe permissions, invalid TOML or invalid paths and URL.
    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path).context("opening host.toml")?;
        let metadata = file.metadata().context("reading host.toml permissions")?;
        ensure!(
            metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
            "host.toml must be a regular private file with mode 0600"
        );
        ensure!(metadata.len() <= 64 * 1024, "host.toml exceeds 64 KiB");
        let mut text = String::new();
        file.take(64 * 1024 + 1)
            .read_to_string(&mut text)
            .context("reading host.toml")?;
        // Do not attach the parser error: it can reproduce configuration contents.
        let config: Self =
            toml::from_str(&text).map_err(|_| anyhow::anyhow!("invalid host.toml"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.env_file.is_absolute()
                && self.extra_env_files.iter().all(|path| path.is_absolute()),
            "host env file paths must be absolute"
        );
        ensure!(
            self.extra_env_files.len() < 16,
            "at most 16 host env files are supported"
        );
        if let Some(path) = &self.hub_bin {
            ensure!(path.is_absolute(), "hub executable path must be absolute");
        }
        let url = url::Url::parse(&self.public_url).context("invalid host public URL")?;
        agmen_client::check_hub_url(&url).context("invalid host public URL")?;
        Ok(())
    }

    /// Writes the configuration atomically with mode 0600.
    ///
    /// # Errors
    /// Refuses existing files unless `force` is true and returns filesystem errors.
    pub fn write(&self, state: &Path, force: bool) -> Result<PathBuf> {
        self.validate()?;
        fs::create_dir_all(state).context("creating host state directory")?;
        let path = state.join("host.toml");
        let mut temporary =
            tempfile::NamedTempFile::new_in(state).context("creating host config")?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        let text = toml::to_string(self).context("encoding host config")?;
        temporary
            .write_all(text.as_bytes())
            .context("writing host config")?;
        temporary
            .as_file()
            .sync_all()
            .context("syncing host config")?;
        if force {
            temporary.persist(&path).context("replacing host.toml")?;
        } else {
            temporary.persist_noclobber(&path).context(
                "host.toml already exists or cannot be written; use --force to replace it",
            )?;
        }
        Ok(path)
    }
}

/// Finds the bundled helper without relying on the daemon service's PATH.
///
/// # Errors
/// Requires `--hub-bin` outside an application's Contents/MacOS directory.
pub fn bundled_hub(exe: &Path) -> Result<PathBuf> {
    let parent = exe.parent().context("agmend executable has no directory")?;
    ensure!(
        parent.file_name().is_some_and(|name| name == "MacOS")
            && parent
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "Contents")
            && parent
                .parent()
                .and_then(Path::parent)
                .and_then(Path::extension)
                .is_some_and(|extension| extension == "app"),
        "--hub-bin is required when agmend is outside an app bundle"
    );
    Ok(parent.join("agmen-hub"))
}

/// Loads shell env files just as hub.sh does, without changing agmend's environment.
/// Errors deliberately omit shell output, which may contain credentials.
async fn environment(config: &HostConfig) -> Result<BTreeMap<String, String>> {
    let mut files = vec![config.env_file.clone()];
    files.extend(config.extra_env_files.clone());
    let mut env = read_environment(&files).await?;
    normalize_environment(config, &mut env)?;
    Ok(env)
}

/// Env files are shell code holding credentials, so they get host.toml's rule.
fn check_private_env(path: &Path) -> Result<()> {
    let metadata =
        fs::metadata(path).with_context(|| format!("reading host env file {}", path.display()))?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "host env file {} must be a regular private file with mode 0600",
        path.display()
    );
    Ok(())
}

/// Whether the hub child receives a variable. agmend's own environment and
/// unrelated env file entries (Compose passwords, integration-test names)
/// stay out of the hub.
fn passed_to_hub(name: &str) -> bool {
    (name.starts_with("AGMEN_") && !name.starts_with("AGMEN_IT_"))
        || name.starts_with("PG")
        || name.starts_with("LC_")
        || [
            "HOME",
            "PATH",
            "TMPDIR",
            "USER",
            "LOGNAME",
            "LANG",
            "TZ",
            "RUST_LOG",
            "RUST_BACKTRACE",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
        ]
        .contains(&name)
}

async fn read_environment(files: &[PathBuf]) -> Result<BTreeMap<String, String>> {
    for path in files {
        check_private_env(path)?;
    }
    let mut command = Command::new("/bin/bash");
    command.args(["-c", "set +x; set +v; set -a; for path do if ! source \"$path\" >/dev/null 2>&1; then exit 1; fi; done; /usr/bin/env -0", "agmend-host-env"])
        .args(files)
        .stdin(Stdio::null()).stderr(Stdio::null()).stdout(Stdio::piped()).kill_on_drop(true);
    let bytes = capture(&mut command, MAX_ENV)
        .await
        .context("reading host env file")?;
    let mut env = BTreeMap::new();
    for entry in bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = std::str::from_utf8(entry).context("host env file contains non-UTF-8 data")?;
        if let Some((name, value)) = entry.split_once('=') {
            env.insert(name.to_owned(), value.to_owned());
        }
    }
    Ok(env)
}

async fn capture(command: &mut Command, maximum: u64) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("starting command")?;
    let stdout = child
        .stdout
        .take()
        .context("opening host env file reader output")?;
    use tokio::io::AsyncReadExt as _;
    let mut bytes = Vec::new();
    tokio::time::timeout(COMMAND_TIMEOUT, async {
        stdout
            .take(maximum + 1)
            .read_to_end(&mut bytes)
            .await
            .context("reading command output")?;
        ensure!(
            bytes.len() as u64 <= maximum,
            "command output exceeds size limit"
        );
        let status = child.wait().await.context("waiting for command")?;
        ensure!(status.success(), "command failed with {status}");
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("command timed out")??;
    Ok(bytes)
}

fn normalize_environment(config: &HostConfig, env: &mut BTreeMap<String, String>) -> Result<()> {
    for (name, fallback) in [
        ("AGMEN_DATABASE_URL", "AGMEN_IT_PG"),
        ("AGMEN_MQTT_URL", "AGMEN_IT_MQTT"),
        ("AGMEN_EMQX_API_URL", "AGMEN_IT_EMQX_API"),
        ("AGMEN_EMQX_API_KEY", "AGMEN_IT_EMQX_API_KEY"),
        ("AGMEN_EMQX_API_SECRET", "AGMEN_IT_EMQX_API_SECRET"),
    ] {
        if env.get(name).is_none_or(String::is_empty)
            && let Some(value) = env.get(fallback).cloned()
        {
            env.insert(name.into(), value);
        }
    }
    if env
        .get("AGMEN_JWT_SIGNING_KEY")
        .is_none_or(String::is_empty)
        && let Some(dir) = env.get("AGMEN_IT_JWT_DIR")
    {
        env.insert(
            "AGMEN_JWT_SIGNING_KEY".into(),
            format!("{dir}/agmen-dev-1.pem"),
        );
    }
    env.entry("AGMEN_LISTEN".into())
        .or_insert_with(|| "127.0.0.1:8080".into());
    env.insert("AGMEN_PUBLIC_URL".into(), config.public_url.clone());
    if !env.contains_key("AGMEN_PUBLIC_MQTT_URL") {
        let mut mqtt = url::Url::parse(&config.public_url)?;
        mqtt.set_scheme("wss")
            .map_err(|()| anyhow::anyhow!("invalid public MQTT scheme"))?;
        mqtt.set_port(Some(8443))
            .map_err(|()| anyhow::anyhow!("invalid public MQTT port"))?;
        mqtt.set_path("/mqtt");
        env.insert("AGMEN_PUBLIC_MQTT_URL".into(), mqtt.to_string());
    }
    Ok(())
}

/// Returns the public URL from env files for host init, without displaying their values.
///
/// # Errors
/// Returns a missing URL or env file error.
pub async fn public_url_from_files(files: &[PathBuf]) -> Result<String> {
    ensure!(!files.is_empty(), "--env-file is required");
    let mut env = read_environment(files).await?;
    let url = env.remove("AGMEN_PUBLIC_URL").unwrap_or_default();
    ensure!(
        !url.is_empty(),
        "--public-url is required when env files do not set AGMEN_PUBLIC_URL"
    );
    Ok(url)
}

#[derive(Debug)]
pub(crate) struct Host {
    config_path: PathBuf,
    status: Mutex<HostStatus>,
    restart: Notify,
    stop: Notify,
    log: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum HostState {
    Running,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
struct HostStatus {
    configured: bool,
    state: HostState,
    last_error: Option<String>,
    public_url: Option<String>,
}

impl Host {
    pub(crate) fn new(state: &Path, log: PathBuf) -> Arc<Self> {
        let path = state.join("host.toml");
        let status = if !path.exists() {
            HostStatus {
                configured: false,
                state: HostState::Stopped,
                last_error: None,
                public_url: None,
            }
        } else {
            match HostConfig::load(&path) {
                Ok(config) => HostStatus {
                    configured: true,
                    state: HostState::Stopped,
                    last_error: None,
                    public_url: Some(config.public_url),
                },
                Err(error) => HostStatus {
                    configured: true,
                    state: HostState::Failed,
                    last_error: Some(format!("{error:#}")),
                    public_url: None,
                },
            }
        };
        Arc::new(Self {
            config_path: path,
            status: Mutex::new(status),
            restart: Notify::new(),
            stop: Notify::new(),
            log,
        })
    }

    pub(crate) fn status(&self) -> Value {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !status.configured && self.config_path.exists() {
            status.configured = true;
            self.restart.notify_one();
        }
        json!(&*status)
    }

    fn set(&self, state: HostState, error: Option<String>) {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        status.state = state;
        status.last_error = error;
    }

    pub(crate) fn restart(&self) -> Result<Value> {
        ensure!(
            self.config_path.exists(),
            "host mode has no configuration; run agmend host init first"
        );
        self.set(HostState::Stopped, None);
        self.restart.notify_one();
        Ok(self.status())
    }

    pub(crate) fn stop(&self) {
        self.stop.notify_one();
    }

    pub(crate) async fn supervise(self: Arc<Self>) {
        // A missing configuration is rechecked by host_status, and Restart reloads it.
        // Each active run budgets automatic retries; this actor lasts until shutdown.
        loop {
            if self.config_path.exists() {
                let prepare = async {
                    let config = HostConfig::load(&self.config_path)?;
                    {
                        let mut status = self
                            .status
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        status.configured = true;
                        status.public_url = Some(config.public_url.clone());
                    }
                    let env = environment(&config).await?;
                    Ok::<_, anyhow::Error>((config, env))
                };
                let prepared = tokio::select! {
                    () = self.stop.notified() => { self.set(HostState::Stopped, None); return; }
                    result = prepare => result,
                };
                let result = match prepared {
                    Ok((config, env)) => self.run(&config, &env, &mut SystemRunner).await,
                    Err(error) => Err(error),
                };
                match result {
                    Ok(true) => return,
                    Ok(false) => continue,
                    Err(error) => self.set(HostState::Failed, Some(format!("{error:#}"))),
                }
            }
            tokio::select! {
                () = self.stop.notified() => { self.set(HostState::Stopped, None); return; }
                () = self.restart.notified() => {}
            }
        }
    }

    async fn run(
        &self,
        config: &HostConfig,
        env: &BTreeMap<String, String>,
        runner: &mut impl CommandRunner,
    ) -> Result<bool> {
        let listen = env
            .get("AGMEN_LISTEN")
            .context("missing AGMEN_LISTEN")?
            .parse::<std::net::SocketAddr>()
            .context("invalid AGMEN_LISTEN")?;
        let target = format!("http://{listen}");
        tokio::select! {
            () = self.stop.notified() => { self.set(HostState::Stopped, None); return Ok(true); }
            result = setup_tailscale(config.tailscale_serve, &target, runner) => result?,
        }
        let hub = match &config.hub_bin {
            Some(path) => path.clone(),
            None => bundled_hub(&std::env::current_exe().context("locating bundled hub")?)?,
        };
        if let Some(parent) = self.log.parent() {
            fs::create_dir_all(parent).context("creating hub log directory")?;
        }
        let mut restarts = VecDeque::with_capacity(MAX_RESTARTS);
        // This actor lives until daemon shutdown. Every automatic restart is budgeted.
        loop {
            let output = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&self.log)
                .context("opening hub.log")?;
            output
                .set_permissions(fs::Permissions::from_mode(0o600))
                .context("securing hub.log")?;
            let offset = output.metadata()?.len();
            let child = Command::new(&hub)
                .arg("serve")
                .env_clear()
                .envs(env.iter().filter(|(name, _)| passed_to_hub(name)))
                .stdin(Stdio::null())
                .stdout(output.try_clone()?)
                .stderr(output)
                .kill_on_drop(true)
                .spawn();
            let failure = match child {
                Err(error) => format!("starting agmen-hub: {error}"),
                Ok(mut child) => {
                    self.set(HostState::Running, None);
                    tokio::select! {
                        () = self.stop.notified() => {
                            stop_child(&mut child).await?;
                            self.set(HostState::Stopped, None);
                            return Ok(true);
                        }
                        () = self.restart.notified() => {
                            stop_child(&mut child).await?;
                            return Ok(false);
                        }
                        status = child.wait() => {
                            let status = status.context("waiting for agmen-hub")?;
                            let detail = self.error_tail(offset, env);
                            format!("agmen-hub exited with {status}{}", if detail.is_empty() { String::new() } else { format!(": {detail}") })
                        }
                    }
                }
            };
            let now = Instant::now();
            restarts.retain(|when| now.duration_since(*when) < WINDOW);
            self.set(HostState::Failed, Some(failure.clone()));
            if restarts.len() == MAX_RESTARTS {
                self.set(
                    HostState::Failed,
                    Some(format!("{failure}; stopped after 5 restarts in 10 minutes")),
                );
                tokio::select! {
                    () = self.stop.notified() => { self.set(HostState::Stopped, None); return Ok(true); }
                    () = self.restart.notified() => return Ok(false),
                }
            }
            let backoff = Duration::from_secs(1 << restarts.len());
            tokio::select! {
                () = self.stop.notified() => { self.set(HostState::Stopped, None); return Ok(true); }
                () = self.restart.notified() => return Ok(false),
                () = tokio::time::sleep(backoff) => { restarts.push_back(Instant::now()); }
            }
        }
    }

    fn error_tail(&self, offset: u64, env: &BTreeMap<String, String>) -> String {
        let read = || -> Result<String> {
            let mut file = File::open(&self.log)?;
            let end = file.metadata()?.len();
            file.seek(SeekFrom::Start(offset.max(end.saturating_sub(8192))))?;
            let mut bytes = Vec::new();
            file.take(8192).read_to_end(&mut bytes)?;
            Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
        };
        let mut text =
            read().unwrap_or_else(|error| format!("reading hub error output failed: {error:#}"));
        for (name, value) in env {
            if !value.is_empty()
                && ["KEY", "SECRET", "TOKEN", "PASSWORD", "DATABASE_URL"]
                    .iter()
                    .any(|part| name.contains(part))
            {
                text = text.replace(value, "[redacted]");
            }
        }
        text
    }
}

async fn stop_child(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_none() {
        child.start_kill().context("stopping agmen-hub")?;
    }
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .context("stopping agmen-hub timed out")??;
    Ok(())
}

trait CommandRunner {
    async fn output(&mut self, args: &[&str]) -> Result<Value>;
}

struct SystemRunner;

impl CommandRunner for SystemRunner {
    async fn output(&mut self, args: &[&str]) -> Result<Value> {
        let program = tailscale_program();
        let bytes = capture(Command::new(program).args(args), MAX_ENV)
            .await
            .context("running tailscale")?;
        if args == ["serve", "status", "--json"] {
            serde_json::from_slice(&bytes).context("invalid tailscale serve status JSON")
        } else {
            Ok(Value::Null)
        }
    }
}

fn tailscale_program() -> PathBuf {
    // launchd does not inherit an interactive shell's PATH.
    [
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
    .unwrap_or_else(|| PathBuf::from("tailscale"))
}

async fn setup_tailscale(
    enabled: bool,
    target: &str,
    runner: &mut impl CommandRunner,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    let status = runner.output(&["serve", "status", "--json"]).await?;
    if !already_serving(&status, target) {
        runner
            .output(&["serve", "--bg", "--https=443", target])
            .await?;
    }
    Ok(())
}

fn already_serving(status: &Value, target: &str) -> bool {
    web_serves(status, target)
        || status
            .get("Foreground")
            .and_then(Value::as_object)
            .is_some_and(|foreground| foreground.values().any(|config| web_serves(config, target)))
}

fn web_serves(config: &Value, target: &str) -> bool {
    config
        .get("Web")
        .and_then(Value::as_object)
        .is_some_and(|web| {
            web.iter().any(|(host, routes)| {
                host.ends_with(":443")
                    && routes
                        .get("Handlers")
                        .and_then(|handlers| handlers.get("/"))
                        .and_then(|handler| handler.get("Proxy"))
                        .and_then(Value::as_str)
                        == Some(target)
            })
        })
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
