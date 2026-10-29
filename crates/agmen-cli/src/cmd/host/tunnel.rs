//! Public tunnel selection and startup for a self-hosted hub.
//!
//! Documentation checked 2026-10-28:
//! - <https://tailscale.com/docs/reference/tailscale-cli/funnel> documents
//!   `--bg`, `--yes`, port targets, `--https=443 off`, `reset` and `status --json`.
//! - <https://tailscale.com/docs/reference/tailscale-cli> documents `status --json`
//!   and warns its JSON format can change.
//! - <https://tailscale.com/docs/integrations/proxmox> uses `Self.DNSName` and
//!   removes its trailing dot. The allowed docs did not specify `BackendState`
//!   or the Funnel `Web`/`Foreground` schema; those follow the task's fixtures
//!   and agmend's existing parser, without importing agmend.
//! - <https://ngrok.com/docs/gateway/agent/api> documents `GET /api/tunnels` and
//!   `tunnels[].public_url`. It is deprecated but remains the task's API.

use std::io::{IsTerminal as _, Read as _, Write as _};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::ValueEnum;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use url::Url;

use super::docker::{Compose, find_program};
use super::files::HostEnv;
use crate::output::Fail;

const OUTPUT_LIMIT: u64 = 1024 * 1024;
const TAILSCALE_LIMIT: Duration = Duration::from_secs(30);
const OPTIONS: &str =
    "sign in to Tailscale, provide an ngrok authtoken, or pass --public-url <https url>";

// `choose` takes an immutable env and Tunnel carries no credentials. Keep a
// pasted token in memory so startup can write it to .env without prompting twice.
static PASTED_TOKEN: OnceLock<String> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum TunnelKind {
    Tailscale,
    Ngrok,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tunnel {
    Url(Url),
    Tailscale,
    Ngrok,
}

impl Tunnel {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Url(_) => "url",
            Self::Tailscale => "tailscale",
            Self::Ngrok => "ngrok",
        }
    }
}

/// Chooses an explicit URL, flag, stored choice, signed-in Tailscale, then ngrok.
///
/// # Errors
///
/// Usage errors when no tunnel is available or the stored choice is invalid.
pub(crate) async fn choose(
    public_url: Option<Url>,
    flag: Option<TunnelKind>,
    env: &HostEnv,
) -> Result<Tunnel> {
    if let Some(url) = public_url {
        return Ok(Tunnel::Url(url));
    }
    if let Some(kind) = flag {
        return Ok(match kind {
            TunnelKind::Tailscale => Tunnel::Tailscale,
            TunnelKind::Ngrok => Tunnel::Ngrok,
        });
    }
    match env.get("AGMEN_TUNNEL") {
        Some("url") => {
            let text = env
                .get("AGMEN_PUBLIC_URL")
                .context("stored public URL is missing")?;
            return super::parse_public_url(text.to_owned()).map(Tunnel::Url);
        }
        Some("tailscale") => return Ok(Tunnel::Tailscale),
        Some("ngrok") => return Ok(Tunnel::Ngrok),
        Some(_) => return Err(Fail::usage("AGMEN_TUNNEL must be url, tailscale or ngrok").into()),
        None => {}
    }
    // An unavailable, failed, malformed, or stopped Tailscale falls through.
    if let Ok(status) = tailscale(&["status", "--json"]).await
        && running_url(&status).is_ok()
    {
        return Ok(Tunnel::Tailscale);
    }
    ngrok_token(env).await?;
    Ok(Tunnel::Ngrok)
}

#[derive(Deserialize)]
struct TailscaleStatus {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "Self")]
    this_machine: Option<TailscalePeer>,
}

#[derive(Deserialize)]
struct TailscalePeer {
    #[serde(rename = "DNSName")]
    dns_name: String,
}

/// The HTTPS URL of this machine, with its DNS name's trailing dot removed.
///
/// # Errors
///
/// When status is malformed, the name is missing, or it is not a DNS name.
pub(crate) fn tailscale_url(status_json: &str) -> Result<Url> {
    let status: TailscaleStatus = serde_json::from_str(status_json)
        .map_err(|_| anyhow::anyhow!("Tailscale returned invalid status JSON"))?;
    let peer = status
        .this_machine
        .context("Tailscale has no DNS name for this machine")?;
    let dns = peer.dns_name.trim_end_matches('.');
    if dns.is_empty()
        || dns.len() > 253
        || !dns
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
        || dns.split('.').any(str::is_empty)
    {
        bail!("Tailscale has no valid DNS name for this machine");
    }
    Url::parse(&format!("https://{dns}")).context("parsing the Tailscale URL")
}

fn running_url(status_json: &str) -> Result<Url> {
    let status: TailscaleStatus = serde_json::from_str(status_json)
        .map_err(|_| anyhow::anyhow!("Tailscale returned invalid status JSON"))?;
    if status.backend_state != "Running" {
        bail!("Tailscale is not signed in and running");
    }
    tailscale_url(status_json)
}

/// The root proxy on port 443, including foreground Funnel configurations.
pub(crate) fn funnel_target(funnel_status_json: &str, dns_name: &str) -> Option<String> {
    // Malformed status has no readable target. Startup validates JSON first.
    let status: Value = serde_json::from_str(funnel_status_json).ok()?;
    let host = format!("{}:443", dns_name.trim_end_matches('.'));
    let target = |config: &Value| web_target(config, &host).map(str::to_owned);
    target(&status).or_else(|| {
        status
            .get("Foreground")?
            .as_object()?
            .values()
            .find_map(target)
    })
}

fn web_target<'a>(config: &'a Value, host: &str) -> Option<&'a str> {
    config
        .get("Web")?
        .get(host)?
        .get("Handlers")?
        .get("/")?
        .get("Proxy")?
        .as_str()
}

/// Checks the existing 443 proxy before any stack or Funnel changes.
pub(crate) async fn prepare_tailscale(port: u16) -> Result<Url> {
    let url = running_url(&tailscale(&["status", "--json"]).await?)?;
    let status = tailscale(&["funnel", "status", "--json"]).await?;
    let config: Value = serde_json::from_str(&status)
        .map_err(|_| anyhow::anyhow!("Tailscale returned invalid Funnel status JSON"))?;
    let dns = url.host_str().context("Tailscale URL has no host")?;
    let host = format!("{dns}:443");
    let foreground = config
        .get("Foreground")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|sessions| sessions.values())
        .filter_map(|session| web_target(session, &host));
    // The first target alone can hide another session that owns port 443.
    let first = funnel_target(&status, dns);
    for target in first.as_deref().into_iter().chain(foreground) {
        if target != format!("http://127.0.0.1:{port}") {
            bail!(
                "Tailscale Funnel on this machine already serves {target} on 443; free it with tailscale funnel reset, or use --public-url; if agmend host mode serves this machine's hub, agmend host init --tailscale-serve may be holding it"
            );
        }
    }
    // Funnel publishes every handler on 443, so any other entry there must stop us.
    let own = format!("http://127.0.0.1:{port}");
    let sessions = config
        .get("Foreground")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|sessions| sessions.values());
    for part in std::iter::once(&config).chain(sessions) {
        if let Some(other) = unexpected_443(part, &host, &own) {
            bail!(
                "Tailscale on this machine already serves {other} on 443; free it with tailscale funnel reset, or use --public-url; if agmend host mode serves this machine's hub, agmend host init --tailscale-serve may be holding it"
            );
        }
    }
    Ok(url)
}

/// Describes anything on 443 other than a single `/` proxy to `own`.
/// A shape that cannot be read counts as in use.
fn unexpected_443(config: &Value, host: &str, own: &str) -> Option<String> {
    if let Some(forward) = config.pointer("/TCP/443/TCPForward")
        && !forward.is_null()
        && forward.as_str() != Some("")
    {
        return Some("a TCP forward".to_owned());
    }
    let web = config.get("Web").filter(|web| !web.is_null())?;
    let Some(web) = web.as_object() else {
        return Some("an unreadable Web entry".to_owned());
    };
    let entry = web.get(host)?;
    let Some(handlers) = entry.get("Handlers").and_then(Value::as_object) else {
        return Some("an unreadable handler list".to_owned());
    };
    handlers.iter().find_map(|(path, handler)| {
        let proxy = handler.get("Proxy").and_then(Value::as_str);
        (path != "/" || proxy != Some(own)).then(|| format!("a handler for {path}"))
    })
}

/// Turns Funnel off on 443 only when it still proxies to this hub's port.
///
/// Returns whether it ran `funnel --https=443 off`.
pub(crate) async fn funnel_off(port: u16) -> Result<bool> {
    let url = tailscale_url(&tailscale(&["status", "--json"]).await?)?;
    let dns = url.host_str().context("Tailscale URL has no host")?;
    let status = tailscale(&["funnel", "status", "--json"]).await?;
    serde_json::from_str::<Value>(&status)
        .map_err(|_| anyhow::anyhow!("Tailscale returned invalid Funnel status JSON"))?;
    if funnel_target(&status, dns).as_deref() != Some(&format!("http://127.0.0.1:{port}")) {
        return Ok(false);
    }
    tailscale(&["funnel", "--https=443", "off"]).await?;
    Ok(true)
}

/// Runs one Tailscale command within 30 seconds and at most 1 MiB of stdout.
pub(crate) async fn tailscale(args: &[&str]) -> Result<String> {
    let program = find_program("tailscale").context("Tailscale is not installed")?;
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null());
    command_output(command, TAILSCALE_LIMIT)
        .await
        .context("running Tailscale with a 30 second timeout")
}

async fn command_output(mut command: Command, limit: Duration) -> Result<String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("starting the tunnel helper")?;
    let stdout = child
        .stdout
        .take()
        .context("tunnel helper stdout was not piped")?;
    let mut bytes = Vec::new();
    let mut stdout = stdout.take(OUTPUT_LIMIT + 1);
    tokio::time::timeout(limit, async {
        let (status, _) = tokio::try_join!(child.wait(), stdout.read_to_end(&mut bytes))
            .context("reading tunnel helper output")?;
        if !status.success() {
            bail!("tunnel helper command failed");
        }
        if bytes.len() as u64 > OUTPUT_LIMIT {
            bail!("tunnel helper output exceeds 1 MiB");
        }
        String::from_utf8(bytes).context("tunnel helper output is not UTF-8")
    })
    .await
    .context("tunnel helper timed out")?
}

fn valid_token(token: &str) -> bool {
    (20..=100).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Returns a validated token without including its value in any errors.
pub(crate) async fn ngrok_token(env: &HostEnv) -> Result<String> {
    if let Some(token) = env
        .get("AGMEN_NGROK_AUTHTOKEN")
        .filter(|token| valid_token(token))
    {
        return Ok(token.to_owned());
    }
    if let Ok(token) = std::env::var("NGROK_AUTHTOKEN")
        && valid_token(&token)
    {
        return Ok(token);
    }
    if let Some(token) = PASTED_TOKEN.get() {
        return Ok(token.clone());
    }
    if !std::io::stdin().is_terminal() {
        return Err(Fail::usage(OPTIONS).into());
    }
    // Use the native terminal tool rather than adding a terminal dependency.
    // Each invocation is bounded, and the saved state is restored on read errors.
    let stty = find_program("stty")
        .context("stty is required to paste a token privately; set NGROK_AUTHTOKEN instead")?;
    let limit = Duration::from_secs(5);
    let mut save = Command::new(&stty);
    save.arg("-g").stdin(Stdio::inherit());
    let settings = command_output(save, limit)
        .await
        .context("reading terminal settings")?;
    let mut hide = Command::new(&stty);
    // Consume Ctrl+C as input so cancellation returns through restoration.
    hide.args(["-echo", "-icanon", "-isig", "min", "1", "time", "0"])
        .stdin(Stdio::inherit());
    command_output(hide, limit)
        .await
        .context("turning off terminal echo")?;
    let read = tokio::task::spawn_blocking(read_terminal_token).await;
    let mut restore = Command::new(&stty);
    restore.arg(settings.trim()).stdin(Stdio::inherit());
    let restored = command_output(restore, limit).await;
    eprintln!();
    restored.context("restoring terminal settings")?;
    let token = read.context("reading the ngrok token")??;
    Ok(PASTED_TOKEN.get_or_init(|| token).clone())
}

fn read_terminal_token() -> Result<String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(Fail::usage(OPTIONS).into());
    }
    eprint!(
        "Paste your ngrok authtoken from https://dashboard.ngrok.com/get-started/your-authtoken: "
    );
    std::io::stderr()
        .flush()
        .context("printing the token prompt")?;
    let mut bytes = Vec::new();
    let mut input = stdin.take(256);
    for _ in 0..256 {
        let mut byte = [0u8];
        if input.read(&mut byte).context("reading the ngrok token")? == 0 {
            break;
        }
        match byte[0] {
            b'\n' | b'\r' | 4 => break,
            3 | 26 | 28 => {
                return Err(Fail::usage(format!("ngrok token entry cancelled; {OPTIONS}")).into());
            }
            8 | 127 => {
                bytes.pop();
            }
            byte => bytes.push(byte),
        }
    }
    let token = String::from_utf8(bytes).map_err(|_| Fail::usage(OPTIONS))?;
    let token = token.trim_end_matches(['\r', '\n']);
    if !valid_token(token) {
        return Err(Fail::usage(OPTIONS).into());
    }
    Ok(token.to_owned())
}

fn ngrok_url(tunnels_json: &str) -> Result<Url> {
    let tunnels: Value = serde_json::from_str(tunnels_json)
        .map_err(|_| anyhow::anyhow!("ngrok returned invalid tunnels JSON"))?;
    tunnels
        .get("tunnels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tunnel| tunnel.get("public_url").and_then(Value::as_str))
        .filter(|url| url.starts_with("https://"))
        // A malformed URL is not a published HTTPS tunnel.
        .find_map(|text| Url::parse(text).ok().filter(|url| url.has_host()))
        .context("ngrok has no HTTPS tunnel yet")
}

/// Polls the agent's loopback API every second, at most thirty times.
pub(crate) async fn wait_ngrok(compose: &Compose) -> Result<Url> {
    let addr = compose.port("ngrok", 4040).await?;
    if !addr.ip().is_loopback() {
        bail!("ngrok's agent API must be published on loopback");
    }
    let client = super::probe_client(Duration::from_secs(1))?;
    let url = format!("http://{addr}/api/tunnels");
    for attempt in 1..=30 {
        let probe = async {
            let mut response = client.get(&url).send().await?.error_for_status()?;
            if response
                .content_length()
                .is_some_and(|length| length > OUTPUT_LIMIT)
            {
                bail!("ngrok tunnels response exceeds 1 MiB");
            }
            // Reading is bounded independently of Content-Length.
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.context("reading ngrok tunnels")? {
                if bytes.len() + chunk.len() > OUTPUT_LIMIT as usize {
                    bail!("ngrok tunnels response exceeds 1 MiB");
                }
                bytes.extend_from_slice(&chunk);
            }
            let text = std::str::from_utf8(&bytes).context("ngrok tunnels JSON is not UTF-8")?;
            ngrok_url(text)
        }
        .await;
        if let Ok(url) = probe {
            return Ok(url);
        }
        if attempt < 30 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    bail!("ngrok did not publish an HTTPS tunnel after 30 attempts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn selection_precedence() {
        let url = Url::parse("https://hub.example.com").unwrap();
        let mut env = HostEnv::default();
        env.set("AGMEN_TUNNEL", "ngrok").unwrap();
        assert_eq!(
            choose(Some(url.clone()), Some(TunnelKind::Tailscale), &env)
                .await
                .unwrap(),
            Tunnel::Url(url.clone())
        );
        assert_eq!(
            choose(None, Some(TunnelKind::Tailscale), &env)
                .await
                .unwrap(),
            Tunnel::Tailscale
        );
        assert_eq!(choose(None, None, &env).await.unwrap(), Tunnel::Ngrok);
        env.set("AGMEN_TUNNEL", "tailscale").unwrap();
        assert_eq!(
            choose(None, Some(TunnelKind::Ngrok), &env).await.unwrap(),
            Tunnel::Ngrok
        );
        assert_eq!(choose(None, None, &env).await.unwrap(), Tunnel::Tailscale);
        env.set("AGMEN_TUNNEL", "url").unwrap();
        env.set("AGMEN_PUBLIC_URL", url.as_str()).unwrap();
        assert_eq!(choose(None, None, &env).await.unwrap(), Tunnel::Url(url));
    }

    #[test]
    fn ngrok_token_shape() {
        for length in [20, 100] {
            assert!(valid_token(&"a".repeat(length)));
        }
        for token in [
            "a".repeat(19),
            "a".repeat(101),
            "-".repeat(20),
            "a\n".repeat(20),
            "$".repeat(20),
        ] {
            assert!(!valid_token(&token));
        }
        assert!(valid_token(&"Aa0_".repeat(5)));
    }

    #[test]
    fn tailscale_url_trims_dot() {
        let status = r#"{"BackendState":"Running","Self":{"DNSName":"box.tail1234.ts.net."}}"#;
        assert_eq!(
            tailscale_url(status).unwrap().as_str(),
            "https://box.tail1234.ts.net/"
        );
        for bad in [
            "{}",
            "not json",
            r#"{"Self":{"DNSName":""}}"#,
            r#"{"Self":{"DNSName":"bad/path"}}"#,
        ] {
            assert!(tailscale_url(bad).is_err());
        }
    }

    #[test]
    fn funnel_target_parsing() {
        let web = serde_json::json!({"Web": {"box.tail1234.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:3000"}}}}});
        assert_eq!(funnel_target("{}", "box.tail1234.ts.net"), None);
        assert_eq!(funnel_target("invalid", "box.tail1234.ts.net"), None);
        assert_eq!(
            funnel_target(&web.to_string(), "box.tail1234.ts.net"),
            Some("http://127.0.0.1:3000".to_owned())
        );
        let foreground = serde_json::json!({"Foreground": {"session": web}});
        assert_eq!(
            funnel_target(&foreground.to_string(), "box.tail1234.ts.net"),
            Some("http://127.0.0.1:3000".to_owned())
        );
        assert_eq!(
            funnel_target(&foreground.to_string(), "another.ts.net"),
            None
        );
    }

    #[test]
    fn ngrok_url_from_tunnels() {
        let tunnels = r#"{"tunnels":[{"public_url":"http://a"},{"public_url":"https://able-otter-123.ngrok-free.app"}]}"#;
        assert_eq!(
            ngrok_url(tunnels).unwrap().as_str(),
            "https://able-otter-123.ngrok-free.app/"
        );
        for bad in [
            "{}",
            "invalid",
            r#"{"tunnels":[]}"#,
            r#"{"tunnels":[{"public_url":"http://a"}]}"#,
        ] {
            assert!(ngrok_url(bad).is_err());
        }
    }
}
