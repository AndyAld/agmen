//! `agmen host up|status|stop|logs` for a self-hosted hub on this machine.

mod docker;
mod files;

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use url::Url;

use self::docker::{Compose, check_docker};
use self::files::{EMQX_CONF, HostEnv, HostName, render_compose};
use crate::output::{Code, Fail, clean};
use crate::paths::{host_dir, host_dir_path};

const HEALTH_TRIES: u32 = 60;
const HEALTH_PAUSE: Duration = Duration::from_secs(1);

/// Run a self-hosted hub on this machine.
#[derive(Debug, Args)]
pub(crate) struct HostArgs {
    #[command(subcommand)]
    command: HostCommand,
}

#[derive(Debug, Subcommand)]
enum HostCommand {
    /// Write the stack, start it, and wait until the hub answers.
    Up(UpArgs),
    /// Show each service and whether the public URL answers.
    Status(NameArgs),
    /// Stop the stack. Docker volumes keep the data.
    Stop(NameArgs),
    /// Show logs from the stack.
    Logs(LogsArgs),
}

#[derive(Debug, Args)]
struct NameArgs {
    /// Directory name under ~/.agmen/host: 1 to 32 lowercase letters, digits, and hyphens.
    #[arg(
        long,
        default_value = "agmen",
        value_parser = clap::value_parser!(HostName)
    )]
    name: HostName,
}

impl NameArgs {
    fn host_name(&self) -> &HostName {
        &self.name
    }
}

#[derive(Debug, Args)]
struct UpArgs {
    #[command(flatten)]
    name: NameArgs,
    /// Public https URL of this hub.
    #[arg(long, value_name = "URL")]
    public_url: Option<String>,
    /// Local port published to the hub's port 8080.
    #[arg(long, default_value_t = 7080)]
    port: u16,
    /// Seconds a later reachability check may wait. This command polls health on its own.
    #[arg(long, default_value_t = 90)]
    wait: u64,
    /// Days of history the hub keeps, from 1 to 36500. Omit it to keep everything.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1i64..=36_500i64))]
    retention_days: Option<u32>,
}

#[derive(Debug, Args)]
struct LogsArgs {
    #[command(flatten)]
    name: NameArgs,
    /// Keep printing until interrupted.
    #[arg(long)]
    follow: bool,
    /// How many lines to show.
    #[arg(long, default_value_t = 200)]
    tail: u32,
    /// Which service to show. Defaults to every service.
    service: Option<Service>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Service {
    Hub,
    Emqx,
    Postgres,
    Ngrok,
}

impl Service {
    fn as_str(self) -> &'static str {
        match self {
            Self::Hub => "hub",
            Self::Emqx => "emqx",
            Self::Postgres => "postgres",
            Self::Ngrok => "ngrok",
        }
    }
}

/// Runs one `agmen host` command.
///
/// # Errors
///
/// Usage errors exit 2. A missing host for `status` or `stop` exits 3.
/// Docker and port failures exit 1.
pub(crate) async fn run(args: HostArgs) -> Result<()> {
    match args.command {
        HostCommand::Up(args) => up(args).await,
        HostCommand::Status(args) => status(args).await,
        HostCommand::Stop(args) => stop(args).await,
        HostCommand::Logs(args) => logs(args).await,
    }
}

async fn up(args: UpArgs) -> Result<()> {
    // `--wait` bounds the public reachability check added with tunnels.
    tracing::debug!(
        wait_secs = args.wait,
        "host up polls the local health URL on its own"
    );
    let public_url = require_public_url(args.public_url)?;
    check_docker().await?;
    ensure_port_free(args.name.host_name().as_str(), args.port).await?;
    let dir = host_dir(args.name.host_name().as_str())?;
    let mut env = HostEnv::load(&dir.join(".env"))?.unwrap_or_default();
    fill_env(
        &mut env,
        args.name.host_name(),
        &public_url,
        args.port,
        args.retention_days,
    )?;
    write_stack(&dir, args.name.host_name(), &env)?;
    let compose = Compose::new(dir);
    if let Err(err) = compose.up(&[]).await {
        print_hub_logs(&compose).await;
        return Err(err);
    }
    wait_healthy(args.port).await
}

fn require_public_url(text: Option<String>) -> Result<Url> {
    let Some(text) = text else {
        return Err(Fail::usage("pass --public-url <https url>").into());
    };
    let url = match Url::parse(&text) {
        Ok(url) => url,
        Err(_err) => {
            return Err(Fail::usage(
                "--public-url must be an https URL with a host and no query or fragment",
            )
            .into());
        }
    };
    let allow_http = matches!(std::env::var("AGMEN_HOST_ALLOW_HTTP").as_deref(), Ok("1"));
    let scheme_ok = url.scheme() == "https" || (allow_http && url.scheme() == "http");
    if !scheme_ok || !url.has_host() || url.query().is_some() || url.fragment().is_some() {
        return Err(Fail::usage(
            "--public-url must be an https URL with a host and no query or fragment",
        )
        .into());
    }
    Ok(url)
}

async fn ensure_port_free(name: &str, port: u16) -> Result<()> {
    let dir = host_dir_path(name)?;
    let compose_exists = dir.join("compose.yaml").is_file();
    let running = match Compose::new(dir).ps().await {
        Ok(states) => states
            .iter()
            .any(|state| state.service == "hub" && state.state == "running"),
        Err(err) if !compose_exists => {
            tracing::debug!(
                %err,
                "compose ps failed and compose.yaml is absent, so this is a first start"
            );
            false
        }
        Err(err) => {
            // The stack is there but its state is not readable: an older Compose
            // prints another shape, or `.env` is gone and `ps` stops on its
            // required variables. A rerun must not stop on that. `compose up`
            // changes nothing on a running stack and reports a busy port itself.
            eprintln!(
                "agmen: {}; skipping the port check",
                clean(&format!("{err:#}"))
            );
            return Ok(());
        }
    };
    if running {
        return Ok(());
    }
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(_listener) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            bail!("port {port} is in use: pass --port <free port>")
        }
        Err(err) => Err(err).context("checking whether the host port is free"),
    }
}

fn fill_env(
    env: &mut HostEnv,
    name: &HostName,
    public_url: &Url,
    port: u16,
    retention_days: Option<u32>,
) -> Result<()> {
    env.ensure_secret("AGMEN_POSTGRES_PASSWORD")?;
    env.ensure_secret("AGMEN_EMQX_NODE_COOKIE")?;
    env.ensure_secret("AGMEN_EMQX_DASHBOARD_PASSWORD")?;
    env.set("AGMEN_PUBLIC_URL", public_url.as_str())?;
    env.set("AGMEN_HOST_PORT", &port.to_string())?;
    if env.get("AGMEN_GROUP_NAME").is_none() {
        env.set("AGMEN_GROUP_NAME", name.as_str())?;
    }
    env.set("AGMEN_TUNNEL", "url")?;
    if let Some(days) = retention_days {
        env.set("AGMEN_RETENTION_DAYS", &days.to_string())?;
    }
    Ok(())
}

fn write_stack(dir: &std::path::Path, name: &HostName, env: &HostEnv) -> Result<()> {
    let compose = dir.join("compose.yaml");
    std::fs::write(&compose, render_compose(name))
        .with_context(|| format!("writing {}", compose.display()))?;
    let conf = dir.join("emqx.conf");
    std::fs::write(&conf, EMQX_CONF).with_context(|| format!("writing {}", conf.display()))?;
    env.write(&dir.join(".env"))
}

async fn print_hub_logs(compose: &Compose) {
    match compose.logs_text("hub").await {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(20);
            for line in &lines[start..] {
                // Log text comes from the container, so it is cleaned like any other
                // hub-supplied string (ruling R63).
                eprintln!("{}", clean(line));
            }
        }
        Err(err) => {
            tracing::debug!(%err, "reading hub logs after a failed start");
        }
    }
}

async fn wait_healthy(port: u16) -> Result<()> {
    let client = probe_client(Duration::from_secs(2))?;
    let url = format!("http://127.0.0.1:{port}/healthz");
    for attempt in 1..=HEALTH_TRIES {
        match client.get(&url).send().await {
            Ok(response) if response.status() == reqwest::StatusCode::OK => return Ok(()),
            Ok(response) => {
                tracing::debug!(status = %response.status(), attempt, "hub health");
            }
            Err(err) => tracing::debug!(%err, attempt, "hub health"),
        }
        if attempt < HEALTH_TRIES {
            tokio::time::sleep(HEALTH_PAUSE).await;
        }
    }
    bail!("the hub did not answer {url}")
}

async fn status(args: NameArgs) -> Result<()> {
    let dir = host_dir_path(args.name.as_str())?;
    if !dir.is_dir() {
        return Err(missing_host(args.name.as_str()).into());
    }
    let compose = Compose::new(dir.clone());
    for state in compose.ps().await? {
        if state.health.is_empty() {
            println!("{} {}", state.service, state.state);
        } else {
            println!("{} {} {}", state.service, state.state, state.health);
        }
    }
    let env = HostEnv::load(&dir.join(".env"))?.unwrap_or_default();
    let public_url = env.get("AGMEN_PUBLIC_URL");
    let tunnel = env.get("AGMEN_TUNNEL").unwrap_or("unset");
    match public_url {
        Some(url) => println!("public url: {url}"),
        None => println!("public url: unset"),
    }
    println!("tunnel: {tunnel}");
    let Some(url) = public_url else {
        println!("livez not reachable: public url is unset");
        return Ok(());
    };
    let livez = format!("{}/livez", url.trim_end_matches('/'));
    match probe_livez(&livez).await {
        Ok(()) => println!("{livez} reachable"),
        Err(err) => println!("{livez} not reachable: {}", one_line(&format!("{err:#}"))),
    }
    Ok(())
}

async fn stop(args: NameArgs) -> Result<()> {
    let dir = host_dir_path(args.name.as_str())?;
    if !dir.is_dir() {
        return Err(missing_host(args.name.as_str()).into());
    }
    Compose::new(dir).stop().await?;
    println!("stopped; the data stays in Docker volumes; agmen host up starts it again");
    Ok(())
}

async fn logs(args: LogsArgs) -> Result<()> {
    let dir = host_dir_path(args.name.host_name().as_str())?;
    Compose::new(dir)
        .logs(args.service.map(Service::as_str), args.follow, args.tail)
        .await
}

fn missing_host(name: &str) -> Fail {
    Fail::new(
        Code::NotLoggedIn,
        format!("no host named {name}: run agmen host up"),
    )
}

fn probe_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(timeout)
        .build()
        .context("building the HTTP client")
}

async fn probe_livez(url: &str) -> Result<()> {
    let client = probe_client(Duration::from_secs(5))?;
    let response = client.get(url).send().await.context("request failed")?;
    if response.status() == reqwest::StatusCode::OK {
        Ok(())
    } else {
        bail!("status {}", response.status())
    }
}

fn one_line(text: &str) -> String {
    let flat = text.replace(['\n', '\r'], " ");
    let end = flat.floor_char_boundary(200.min(flat.len()));
    flat[..end].to_owned()
}
