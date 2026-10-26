//! The agmen hub: auth, identities, ingest, numbering, replay, and the ledger.
//!
//! `agmen-hub` (or `agmen-hub serve`) serves the HTTP API and, connected to
//! the broker at `AGMEN_MQTT_URL`, ingests what clients publish and
//! publishes the feed, inboxes and replies; it exits if that connection
//! cannot be kept. Configuration
//! comes from `AGMEN_*` environment variables (see `agmen_hub::Config`);
//! `RUST_LOG` sets the log filter (default `info`). Logs go to stderr.
//!
//! `agmen-hub bootstrap --tenant <name> --project <name> --admin-github
//! <login>` makes a tenant, a project and its first admin, reusing what an
//! earlier run with the same names made, and prints their ids as JSON. The
//! login is resolved once to its GitHub account id, which is what the admin
//! is bound to; `--admin-github-id <id>` names the account id directly and
//! skips the lookup. It reads `AGMEN_DATABASE_URL` and, for the lookup,
//! `AGMEN_GITHUB_API_URL` (default `https://api.github.com`).

use std::ffi::OsString;

use agmen_hub::{AdminGithub, GithubConfig};
use anyhow::{Context as _, bail};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: agmen-hub [serve]
       agmen-hub bootstrap --tenant <name> --project <name> (--admin-github <login> | --admin-github-id <id>)";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Serve,
    Bootstrap {
        tenant: String,
        project: String,
        admin: AdminGithub,
    },
    Help,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_crypto_provider()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match parse_args(std::env::args_os().skip(1))? {
        Command::Serve => serve().await,
        Command::Bootstrap {
            tenant,
            project,
            admin,
        } => bootstrap(tenant, project, admin).await,
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

/// Makes aws-lc-rs the process-level rustls provider (ADR-0003). The
/// workspace builds rustls with both the `ring` and `aws-lc-rs` features, so
/// rustls cannot pick a default on its own and panics at the first TLS use.
fn install_crypto_provider() -> anyhow::Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("a rustls crypto provider is already installed"))
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> anyhow::Result<Command> {
    let mut args = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| anyhow::anyhow!("argument {arg:?} is not UTF-8"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter();
    let command = match args.next().as_deref() {
        None | Some("serve") => Command::Serve,
        Some("-h" | "--help" | "help") => return Ok(Command::Help),
        Some("bootstrap") => {
            let (mut tenant, mut project, mut login, mut id) = (None, None, None, None);
            while let Some(flag) = args.next() {
                let slot = match flag.as_str() {
                    "--tenant" => &mut tenant,
                    "--project" => &mut project,
                    "--admin-github" => &mut login,
                    "--admin-github-id" => &mut id,
                    _ => bail!("unknown option {flag:?}\n{USAGE}"),
                };
                let value = args
                    .next()
                    .with_context(|| format!("{flag} needs a value\n{USAGE}"))?;
                if slot.replace(value).is_some() {
                    bail!("{flag} is given twice");
                }
            }
            let missing = |flag: &str| format!("bootstrap needs {flag}\n{USAGE}");
            let admin = match (login, id) {
                (Some(login), None) => AdminGithub::Login(login),
                (None, Some(id)) => AdminGithub::Id(id.parse().with_context(|| {
                    format!("--admin-github-id {id:?} is not a GitHub account id")
                })?),
                (None, None) => bail!(missing("--admin-github or --admin-github-id")),
                (Some(_), Some(_)) => {
                    bail!("give --admin-github or --admin-github-id, not both\n{USAGE}")
                }
            };
            return Ok(Command::Bootstrap {
                tenant: tenant.with_context(|| missing("--tenant"))?,
                project: project.with_context(|| missing("--project"))?,
                admin,
            });
        }
        Some(other) => bail!("unknown command {other:?}\n{USAGE}"),
    };
    if let Some(extra) = args.next() {
        bail!("unexpected argument {extra:?}\n{USAGE}");
    }
    Ok(command)
}

async fn serve() -> anyhow::Result<()> {
    let config = agmen_hub::Config::from_env().context("reading the configuration")?;
    let hub = agmen_hub::Hub::connect(&config)
        .await
        .context("starting the hub")?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("listening on {}", config.listen))?;
    tracing::info!(
        listen = %config.listen,
        version = env!("CARGO_PKG_VERSION"),
        "agmen-hub serving"
    );
    // HTTP serves before the broker connect: in the cluster EMQX verifies
    // the hub's own broker token against this hub's /v1/jwks, so on a cold
    // start it cannot accept the hub until that route answers.
    let server = axum::serve(listener, hub.router()).with_graceful_shutdown(shutdown());
    let messaging = async {
        let mut messaging = hub
            .start_messaging(&config.mqtt)
            .await
            .context("connecting to the broker")?;
        let failed = messaging.failed().await;
        Err::<(), _>(anyhow::Error::new(failed).context("messaging stopped"))
    };
    tokio::select! {
        served = server => served.context("serving HTTP"),
        failed = messaging => failed,
    }
}

async fn bootstrap(tenant: String, project: String, admin: AdminGithub) -> anyhow::Result<()> {
    let request = agmen_hub::Bootstrap::new(tenant, project, admin)?;
    let database_url = std::env::var("AGMEN_DATABASE_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .context("AGMEN_DATABASE_URL is not set")?;
    let github_api_url = GithubConfig::api_url_from_env()?;
    let made = request
        .run(&database_url, &github_api_url)
        .await
        .context("bootstrapping the project")?;
    tracing::info!(
        tenant = %made.tenant,
        project = %made.project,
        admin_github_id = %made.admin_github_id,
        admin_github = made.admin_github.as_deref(),
        "bootstrapped"
    );
    println!(
        "{}",
        serde_json::to_string(&made).context("encoding the ids")?
    );
    Ok(())
}

/// Resolves on Ctrl-C or SIGTERM, so in-flight requests finish first.
async fn shutdown() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Command> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn commands() {
        assert_eq!(parse(&[]).unwrap(), Command::Serve);
        assert_eq!(parse(&["serve"]).unwrap(), Command::Serve);
        assert_eq!(parse(&["--help"]).unwrap(), Command::Help);
        assert_eq!(
            parse(&[
                "bootstrap",
                "--project",
                "agmen",
                "--admin-github",
                "Andy",
                "--tenant",
                "Acme Co",
            ])
            .unwrap(),
            Command::Bootstrap {
                tenant: "Acme Co".to_owned(),
                project: "agmen".to_owned(),
                admin: AdminGithub::Login("Andy".to_owned()),
            }
        );
        assert_eq!(
            parse(&[
                "bootstrap",
                "--tenant",
                "t",
                "--admin-github-id",
                "583231",
                "--project",
                "p",
            ])
            .unwrap(),
            Command::Bootstrap {
                tenant: "t".to_owned(),
                project: "p".to_owned(),
                admin: AdminGithub::Id("583231".parse().unwrap()),
            }
        );
    }

    #[test]
    fn bad_command_lines() {
        for args in [
            &["serve", "now"][..],
            &["start"],
            &["bootstrap", "--tenant", "t", "--project", "p"],
            &[
                "bootstrap",
                "--tenant",
                "t",
                "--project",
                "p",
                "--admin-github",
            ],
            &[
                "bootstrap",
                "--tenant",
                "t",
                "--tenant",
                "u",
                "--project",
                "p",
                "--admin-github",
                "a",
            ],
            &[
                "bootstrap",
                "--tenant",
                "t",
                "--project",
                "p",
                "--admin-github",
                "a",
                "--x",
            ],
            &[
                "bootstrap",
                "--tenant",
                "t",
                "--project",
                "p",
                "--admin-github",
                "a",
                "--admin-github-id",
                "1",
            ],
            &[
                "bootstrap",
                "--tenant",
                "t",
                "--project",
                "p",
                "--admin-github-id",
                "0",
            ],
            &[
                "bootstrap",
                "--tenant",
                "t",
                "--project",
                "p",
                "--admin-github-id",
                "andy",
            ],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }
}
