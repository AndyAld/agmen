//! `agmen host up|status|stop|logs` for a self-hosted hub on this machine.

mod docker;
mod files;
mod onboard;
mod tunnel;

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use url::Url;

use self::docker::{Compose, check_docker};
use self::files::{EMQX_CONF, HostEnv, HostName, render_compose};
use self::tunnel::{Tunnel, TunnelKind};
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
    /// Tunnel to use when no public URL is supplied.
    #[arg(long, value_enum)]
    tunnel: Option<TunnelKind>,
    /// Local port published to the hub's port 8080, from 1 to 65535.
    #[arg(long, default_value_t = 7080, value_parser = parse_host_port)]
    port: u16,
    /// Seconds to wait for the public URL to answer after the stack starts.
    #[arg(long, default_value_t = 90)]
    wait: u64,
    /// Days of history the hub keeps, from 1 to 36500. Omit it to keep everything.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1i64..=36_500i64))]
    retention_days: Option<u32>,
    /// Name of the first project; defaults to the worktree folder's name.
    #[arg(long, value_name = "NAME")]
    project: Option<String>,
    /// Name after your handle and a slash, as in agmen join --handle.
    #[arg(long, value_name = "NAME")]
    handle: Option<String>,
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
/// Usage errors exit 2. A missing host for `status`, `logs`, or `stop` exits 3.
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
    let public_url = args.public_url.map(parse_public_url).transpose()?;
    check_docker().await?;
    let path = host_dir_path(args.name.host_name().as_str())?;
    let mut env = HostEnv::load(&path.join(".env"))?.unwrap_or_default();
    let tunnel = tunnel::choose(public_url, args.tunnel, &env).await?;
    let mut public_url = match &tunnel {
        Tunnel::Url(url) => url.clone(),
        Tunnel::Tailscale => tunnel::prepare_tailscale(args.port).await?,
        // Compose requires a URL even for the initial, ngrok-only start.
        // The hub starts only after the agent supplies its public URL.
        Tunnel::Ngrok => Url::parse("https://localhost").context("parsing the startup URL")?,
    };
    if tunnel == Tunnel::Ngrok {
        let token = tunnel::ngrok_token(&env).await?;
        env.set("AGMEN_NGROK_AUTHTOKEN", &token)?;
    }
    match env.get("AGMEN_TUNNEL") {
        Some("ngrok") if tunnel != Tunnel::Ngrok => {
            let compose = Compose::new(path.clone());
            compose.stop_ngrok().await?;
            compose.remove_ngrok().await?;
            env.remove("AGMEN_NGROK_AUTHTOKEN");
        }
        Some("tailscale") if tunnel != Tunnel::Tailscale => {
            funnel_off_stored_port(&env)
                .await
                .context("turning off the old Tailscale Funnel (or run tailscale funnel reset and set AGMEN_TUNNEL in .env)")?;
        }
        _ => {}
    }
    env.set("AGMEN_TUNNEL", tunnel.name())?;
    env.set(
        "COMPOSE_PROFILES",
        if tunnel == Tunnel::Ngrok { "ngrok" } else { "" },
    )?;
    ensure_port_free(args.name.host_name().as_str(), args.port).await?;
    let dir = host_dir(args.name.host_name().as_str())?;
    fill_env(
        &mut env,
        args.name.host_name(),
        &public_url,
        args.port,
        args.retention_days,
    )?;
    write_stack(&dir, args.name.host_name(), &env)?;
    let compose = Compose::new(dir.clone());
    if tunnel == Tunnel::Ngrok {
        compose.up(&["ngrok"]).await?;
        public_url = tunnel::wait_ngrok(&compose).await?;
        env.set("AGMEN_PUBLIC_URL", public_url.as_str())?;
        env.write(&dir.join(".env"))?;
    }
    if let Err(err) = compose.up(&[]).await {
        print_hub_logs(&compose).await;
        return Err(err);
    }
    wait_healthy(args.port).await?;
    if tunnel == Tunnel::Tailscale {
        // The stack can take minutes to start; check 443 again before taking it.
        tunnel::prepare_tailscale(args.port).await?;
        tunnel::tailscale(&["funnel", "--bg", "--yes", &args.port.to_string()]).await?;
    }
    wait_public(&public_url, &tunnel, args.port, args.wait).await?;
    onboard::onboard(
        &public_url,
        &compose,
        args.project.as_deref(),
        args.handle.as_deref(),
    )
    .await
}

fn parse_host_port(text: &str) -> Result<u16, String> {
    let port: u16 = text
        .parse()
        .map_err(|_| "the port must be from 1 to 65535".to_owned())?;
    if port == 0 {
        return Err("the port must be from 1 to 65535".to_owned());
    }
    Ok(port)
}

fn parse_public_url(text: String) -> Result<Url> {
    // `$` would be expanded by compose when this value is written into `.env`.
    if text.contains('$') {
        return Err(Fail::usage(
            "--public-url cannot contain '$', because the value lands in .env and compose expands it",
        )
        .into());
    }
    let url = match Url::parse(&text) {
        Ok(url) => url,
        Err(_err) => {
            return Err(Fail::usage(
                "--public-url must be an https URL with a host and no query or fragment",
            )
            .into());
        }
    };
    if url.as_str().contains('$') {
        return Err(Fail::usage(
            "--public-url cannot contain '$', because the value lands in .env and compose expands it",
        )
        .into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Fail::usage("--public-url cannot contain a username or password").into());
    }
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
                let line = clean(line);
                if !line.contains("claim_token=") {
                    eprintln!("{line}");
                }
            }
        }
        Err(err) => {
            tracing::debug!(%err, "reading hub logs after a failed start");
        }
    }
}

async fn wait_healthy(port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/healthz");
    wait_healthy_at(&url).await
}

async fn wait_healthy_at(url: &str) -> Result<()> {
    let client = probe_client(Duration::from_secs(2))?;
    for attempt in 1..=HEALTH_TRIES {
        match client.get(url).send().await {
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
    require_host(&dir, args.name.as_str())?;
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
    let compose = Compose::new(dir.clone());
    let ngrok = compose.stop_ngrok().await;
    let stopped = compose.stop().await;
    let env = HostEnv::load(&dir.join(".env"))?.unwrap_or_default();
    // The cleanup always runs, but only a Tailscale host hears about its outcome:
    // a host on ngrok or a URL has no Funnel, and Tailscale may not be installed.
    let tailscale = env.get("AGMEN_TUNNEL") == Some("tailscale");
    match funnel_off_stored_port(&env).await {
        Ok(true) => {}
        Ok(false) if tailscale => {
            eprintln!("note: Funnel on 443 does not proxy to this hub, so it was left as it is")
        }
        Err(err) if tailscale => eprintln!(
            "warning: {}; free Funnel with tailscale funnel reset",
            clean(&format!("{err:#}"))
        ),
        Ok(false) | Err(_) => {}
    }
    ngrok?;
    stopped?;
    println!("stopped; the data stays in Docker volumes; agmen host up starts it again");
    Ok(())
}

async fn funnel_off_stored_port(env: &HostEnv) -> Result<bool> {
    let port = env
        .get("AGMEN_HOST_PORT")
        .and_then(|text| text.parse().ok())
        .context("the stored host port is missing")?;
    // Only the mapping `up` made is turned off: another one on 443 is not ours.
    tunnel::funnel_off(port).await
}

async fn logs(args: LogsArgs) -> Result<()> {
    let dir = host_dir_path(args.name.host_name().as_str())?;
    require_host(&dir, args.name.host_name().as_str())?;
    Compose::new(dir)
        .logs(args.service.map(Service::as_str), args.follow, args.tail)
        .await
}

/// `status` and `logs` need a compose file. A bare directory is not a host.
fn require_host(dir: &std::path::Path, name: &str) -> Result<()> {
    if dir.join("compose.yaml").is_file() {
        Ok(())
    } else {
        Err(missing_host(name).into())
    }
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

async fn wait_public(url: &Url, tunnel: &Tunnel, port: u16, wait: u64) -> Result<()> {
    let livez = format!("{}/livez", url.as_str().trim_end_matches('/'));
    let budget = Duration::from_secs(wait);
    let start = tokio::time::Instant::now();
    loop {
        let remaining = budget.saturating_sub(start.elapsed());
        if tokio::time::timeout(remaining, probe_livez(&livez))
            .await
            .is_ok_and(|result| result.is_ok())
        {
            return Ok(());
        }
        let remaining = budget.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2).min(remaining)).await;
    }
    let hint = match tunnel {
        Tunnel::Tailscale => {
            "Tailscale Funnel can take up to 15 minutes to reach a new machine's public DNS."
                .to_owned()
        }
        Tunnel::Url(_) => format!(
            "check that your proxy sends {url} to 127.0.0.1:{port}, including WebSocket upgrades on /mqtt."
        ),
        Tunnel::Ngrok => String::new(),
    };
    bail!(
        "the hub runs, but {url} does not answer yet. {hint} Run agmen host up again; nothing is lost."
    )
}

fn one_line(text: &str) -> String {
    let flat = text.replace(['\n', '\r'], " ");
    let end = flat.floor_char_boundary(200.min(flat.len()));
    flat[..end].to_owned()
}
